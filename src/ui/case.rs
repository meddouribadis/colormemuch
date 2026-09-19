//! The tower-case editor (PO5-660, WMI). A hero card for the whole case, then
//! a 2×2 grid of per-area override cards. Effects the firmware hasn't been
//! captured for stay visible but disabled, with the reason on hover — the UI
//! never offers a payload the engine would refuse.

use eframe::egui::{self, Vec2};

use colormemuch::dt::{area, AreaCmd, DtEffect};
use colormemuch::rgb::Rgb;

use super::theme::{self, tokens, SP_L, SP_M, SP_S, SP_XS};
use super::widgets::{self, arr32, card, card_header, chip, color_field, form_row, toggle, Segment};
use super::{DtSetup, RgbControl};

const DEFAULT_COLOR: [u8; 3] = [0x00, 0xE5, 0xFF];

/// Area cards, in display order: (selector, title, subtitle, capture-verified).
const AREAS: [(u16, &str, &str, bool); 4] = [
    (area::FRONT, "Front", "Front face", true),
    (area::TOP, "Top", "Top fan · above the GPU", false),
    (area::REAR, "Rear", "Rear exhaust fan", false),
    (area::AUX, "Aux", "Unmapped channel", false),
];

impl RgbControl {
    pub(super) fn case_view(&mut self, ui: &mut egui::Ui) {
        let tk = tokens(ui);

        // Header: title + subtitle, master enable on the right.
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(theme::title("Tower Case"));
                ui.label(theme::caption(
                    ui,
                    "Acer Predator Orion · driven over WMI · memory DIMMs are not included",
                ));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.dt_available {
                    return;
                }
                let mut enabled = self.dt.is_some();
                let r = toggle(ui, &mut enabled);
                ui.label(theme::strong("Case lighting"));
                if r.changed() {
                    self.dt = enabled.then(|| DtSetup {
                        color: DEFAULT_COLOR,
                        on: true,
                        effect: DtEffect::Static,
                        areas: Vec::new(),
                    });
                    self.dirty = true;
                }
            });
        });
        ui.add_space(SP_L);

        if !self.dt_available {
            widgets::empty_state(
                ui,
                "⚡",
                "Case channel unavailable",
                "Driving the tower needs the colormemuch service, or the app launched \
                 as administrator. PredatorSense settings are untouched.",
                |_| {},
            );
            if let Some(msg) = self.dt_error.clone() {
                ui.add_space(SP_S);
                widgets::banner(ui, tk.danger, "WMI error", &msg);
            }
            return;
        }

        if self.dt.is_none() {
            widgets::empty_state(
                ui,
                "●",
                "Case lighting is off",
                "Turn on Case lighting to take control of the tower LEDs. Until then, \
                 PredatorSense keeps whatever it last set.",
                |ui| {
                    if widgets::primary_button(ui, "Turn on").clicked() {
                        self.dt = Some(DtSetup {
                            color: DEFAULT_COLOR,
                            on: true,
                            effect: DtEffect::Static,
                            areas: Vec::new(),
                        });
                        self.dirty = true;
                    }
                },
            );
            return;
        }

        // Hero card — the whole case.
        let mut changed = false;
        if let Some(dt) = &mut self.dt {
            card(ui, |ui| {
                let color = arr32(dt.color);
                let lit = dt.on;
                card_header(
                    ui,
                    |ui| {
                        widgets::swatch(ui, color, lit, 52.0);
                    },
                    "Whole case",
                    "Every area at once — the base the overrides sit on",
                    |ui| {
                        if toggle(ui, &mut dt.on).changed() {
                            changed = true;
                        }
                        ui.label(theme::caption(ui, if dt.on { "On" } else { "Off" }));
                    },
                );
                ui.add_space(SP_M);
                widgets::divider(ui);
                ui.add_space(SP_XS);
                form_row(ui, "Color", |ui| {
                    if color_field(ui, &mut dt.color) {
                        changed = true;
                    }
                });
                form_row(ui, "Effect", |ui| {
                    if dt_effect_picker(ui, "dt-fx-global", &mut dt.effect) {
                        changed = true;
                    }
                });
            });
        }
        if changed {
            self.dirty = true;
        }

        // Areas.
        ui.add_space(SP_L);
        widgets::section_header(ui, "Areas", |ui| {
            let n = self.dt.as_ref().map(|d| d.areas.len()).unwrap_or(0);
            if n > 0 {
                ui.label(theme::hint(ui, format!("{n} override{}", if n == 1 { "" } else { "s" })));
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(SP_XS);
            ui.label(theme::caption(
                ui,
                "Give one area its own color. Front replays a verified PredatorSense \
                 transaction; the others share the same command shape but are still experimental.",
            ));
        });
        ui.add_space(SP_S);

        let gap = SP_M;
        let col_w = ((ui.available_width() - gap) / 2.0).max(240.0);
        for pair in AREAS.chunks(2) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (sel, title, sub, verified) in pair {
                    ui.allocate_ui_with_layout(
                        Vec2::new(col_w, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(col_w);
                            self.area_card(ui, *sel, title, sub, *verified);
                        },
                    );
                }
            });
            ui.add_space(gap);
        }

        if let Some(msg) = self.dt_error.clone() {
            widgets::banner(ui, tk.danger, "The case didn't accept the last change", &msg);
        }
    }

    /// One per-area card. Enabling the override pushes a default entry
    /// (dirty → engine applies the captured transaction with patched sel);
    /// disabling removes it, so the whole-case color shows through again
    /// (restore the area via PredatorSense if it had a custom setting).
    fn area_card(&mut self, ui: &mut egui::Ui, sel: u16, title: &str, sub: &str, verified: bool) {
        let tk = tokens(ui);
        let Some(dt) = &mut self.dt else { return };
        let pos = dt.areas.iter().position(|a| a.area == sel);
        let mut enabled = pos.is_some();
        let mut changed = false;

        card(ui, |ui| {
            let (color, on) = dt
                .areas
                .iter()
                .find(|a| a.area == sel)
                .map(|e| (arr32([e.color.0, e.color.1, e.color.2]), e.on))
                .unwrap_or((arr32(dt.color), false));

            let lit = on && enabled;
            card_header(
                ui,
                |ui| {
                    widgets::swatch(ui, color, lit, 40.0);
                },
                title,
                sub,
                |ui| {
                    if toggle(ui, &mut enabled).changed() {
                        changed = true;
                    }
                },
            );
            ui.add_space(SP_XS);
            ui.horizontal(|ui| {
                ui.add_space(52.0);
                if verified {
                    chip(ui, tk.success, "Verified");
                } else {
                    chip(ui, tk.warn, "Experimental");
                }
                ui.label(theme::hint(ui, if enabled { "Override" } else { "Follows whole case" }));
            });

            if let Some(entry) = dt.areas.iter_mut().find(|a| a.area == sel) {
                ui.add_space(SP_S);
                widgets::divider(ui);
                ui.add_space(SP_XS);
                form_row(ui, "Color", |ui| {
                    let mut c = [entry.color.0, entry.color.1, entry.color.2];
                    if color_field(ui, &mut c) {
                        entry.color = Rgb(c[0], c[1], c[2]);
                        changed = true;
                    }
                });
                form_row(ui, "Power", |ui| {
                    if toggle(ui, &mut entry.on).changed() {
                        changed = true;
                    }
                    ui.label(theme::caption(ui, if entry.on { "On" } else { "Off" }));
                });
                form_row(ui, "Effect", |ui| {
                    let mut fx = entry.effect;
                    if dt_effect_picker(ui, ("dt-fx", sel), &mut fx) {
                        entry.effect = fx;
                        changed = true;
                    }
                });
            }
        });

        if enabled != pos.is_some() {
            if enabled {
                dt.areas.push(AreaCmd {
                    area: sel,
                    color: Rgb(DEFAULT_COLOR[0], DEFAULT_COLOR[1], DEFAULT_COLOR[2]),
                    on: true,
                    effect: DtEffect::Static,
                });
            } else if let Some(i) = pos {
                dt.areas.remove(i);
            }
            changed = true;
        }
        if changed {
            self.dirty = true;
        }
    }
}

/// Firmware-effect picker: Static applies now; the rest are disabled with the
/// reason on hover, lighting up as Frida captures land.
fn dt_effect_picker(ui: &mut egui::Ui, id: impl std::hash::Hash, current: &mut DtEffect) -> bool {
    let segs: Vec<Segment<DtEffect>> = DtEffect::all()
        .into_iter()
        .map(|fx| {
            let s = Segment::new(fx, fx.label());
            if fx.is_supported() {
                s
            } else {
                s.enabled(false)
                    .hint("Not yet captured from PredatorSense — coming once its firmware bytes are verified.")
            }
        })
        .collect();
    widgets::segmented(ui, id, current, &segs)
}
