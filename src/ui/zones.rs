//! The per-device editor: a Hardware (firmware) card, or one card per zone
//! with a live preview strip, plus the shared-clock groups (Link / Spread).

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, Vec2};

use colormemuch::effects::{render_plan, scale, shared_clock_groups, Effect, Group, ZoneSource};
use colormemuch::rgb::Rgb;

use super::theme::{self, tokens, wash, Tokens, R_CONTROL, SP_L, SP_M, SP_S, SP_XS};
use super::widgets::{self, card, color_field, form_row, primary_button, rgb32, Segment};
use super::{short_name, Conn, DeviceMode, HwEffect, Kind, RgbControl, ZoneUi, CUST_HUE, PROG_HUE};

#[derive(Clone, Copy, PartialEq)]
enum ModeTab {
    PerZone,
    Hardware,
}

#[derive(Clone, Copy, PartialEq)]
enum SourceTab {
    Solid,
    Program,
    Custom,
}

impl RgbControl {
    pub(super) fn zones_view(&mut self, ui: &mut egui::Ui, t: f32) {
        let sel = self.selected;
        let controllers = match &self.conn {
            Conn::Ready(c) => c,
            Conn::Connecting => {
                widgets::empty_state(
                    ui,
                    "…",
                    "Connecting",
                    "Looking for the OpenRGB lighting server that PredatorSense runs.",
                    |ui| {
                        ui.spinner();
                    },
                );
                return;
            }
            Conn::Failed(e) => {
                let e = e.clone();
                widgets::empty_state(
                    ui,
                    "⚠",
                    "No lighting server",
                    &format!("{e}\n\nStart PredatorSense (it launches the server) and try again."),
                    |ui| {
                        if primary_button(ui, "Try again").clicked() {
                            self.reconnect();
                        }
                    },
                );
                return;
            }
        };
        if sel >= controllers.len() {
            return;
        }

        let dev = &controllers[sel];
        let dev_name = short_name(&dev.name);
        let mode_names: Vec<String> = dev.effect_mode_names();
        let n_zones = dev.led_count as usize;
        let kind_label = dev.kind.label().to_string();
        let can_hardware = !mode_names.is_empty();

        let is_hardware = matches!(self.dev_mode[sel], DeviceMode::Hardware(_));

        // Header.
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(theme::title(dev_name));
                let where_ = if is_hardware {
                    "Runs on the device controller · zero host CPU · can be saved to firmware"
                } else if self.host.via_service() {
                    "Composited by the service · kept after the window closes"
                } else {
                    "Composited by colormemuch · kept while the app runs (tray)"
                };
                ui.label(theme::caption(
                    ui,
                    format!("{kind_label} · {n_zones} zone{} · {where_}", if n_zones == 1 { "" } else { "s" }),
                ));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if is_hardware {
                    if primary_button(ui, "Save to device")
                        .on_hover_text("Write this firmware effect to the device's flash so it survives a reboot with nothing running.")
                        .clicked()
                    {
                        self.request_save();
                    }
                }
            });
        });
        ui.add_space(SP_M);

        // Mode.
        let mut tab = if is_hardware { ModeTab::Hardware } else { ModeTab::PerZone };
        let segs = [
            Segment::new(ModeTab::PerZone, "Per-zone"),
            Segment::new(ModeTab::Hardware, "Hardware effect")
                .enabled(can_hardware)
                .hint(if can_hardware {
                    "A firmware mode animated by the device itself — no host CPU."
                } else {
                    "This device exposes no firmware modes."
                }),
        ];
        if widgets::segmented(ui, ("mode", sel), &mut tab, &segs) {
            self.dev_mode[sel] = match tab {
                ModeTab::PerZone => DeviceMode::PerZone,
                ModeTab::Hardware => DeviceMode::Hardware(HwEffect {
                    mode: mode_names.first().cloned().unwrap_or_else(|| "STATIC".into()),
                    color: [0x00, 0xE5, 0xFF],
                    speed: 5,
                    brightness: 100,
                }),
            };
            self.dirty = true;
        }
        ui.add_space(SP_L);

        match tab {
            ModeTab::Hardware => self.hardware_card(ui, sel, &mode_names),
            ModeTab::PerZone => self.zone_cards(ui, sel, t),
        }
    }

    fn hardware_card(&mut self, ui: &mut egui::Ui, sel: usize, mode_names: &[String]) {
        let mut dirty = false;
        let DeviceMode::Hardware(hw) = &mut self.dev_mode[sel] else { return };
        widgets::narrow(ui, 520.0, |ui| card(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::swatch(ui, widgets::arr32(hw.color), true, 52.0);
                ui.add_space(SP_XS);
                ui.vertical(|ui| {
                    ui.label(theme::heading(hw.mode.clone()));
                    ui.label(theme::caption(
                        ui,
                        "Applied live now. “Save to device” persists it to firmware.",
                    ));
                });
            });
            ui.add_space(SP_M);
            widgets::divider(ui);
            ui.add_space(SP_XS);
            form_row(ui, "Effect", |ui| {
                egui::ComboBox::from_id_source(("hw-mode", sel))
                    .width(220.0)
                    .selected_text(hw.mode.clone())
                    .show_ui(ui, |ui| {
                        for m in mode_names {
                            if ui.selectable_value(&mut hw.mode, m.clone(), m).clicked() {
                                dirty = true;
                            }
                        }
                    });
            });
            form_row(ui, "Color", |ui| {
                if color_field(ui, &mut hw.color) {
                    dirty = true;
                }
            });
            form_row(ui, "Speed", |ui| {
                ui.spacing_mut().slider_width = 220.0;
                if ui.add(egui::Slider::new(&mut hw.speed, 1..=9).show_value(false)).changed() {
                    dirty = true;
                }
                ui.label(theme::caption(ui, format!("{}", hw.speed)));
            });
            form_row(ui, "Brightness", |ui| {
                ui.spacing_mut().slider_width = 220.0;
                if ui
                    .add(egui::Slider::new(&mut hw.brightness, 0..=100).show_value(false))
                    .changed()
                {
                    dirty = true;
                }
                ui.label(theme::caption(ui, format!("{}%", hw.brightness)));
            });
        }));
        self.dirty |= dirty;
    }

    fn zone_cards(&mut self, ui: &mut egui::Ui, sel: usize, t: f32) {
        let customs: Vec<(String, [u8; 3])> = self
            .library
            .effects
            .iter()
            .map(|e| {
                let d = e.dominant();
                (e.name.clone(), [d.0, d.1, d.2])
            })
            .collect();

        // Real per-LED names from the discovered descriptor (owned so we don't
        // hold a `self.conn` borrow while mutating `self.zones`).
        let led_names: Vec<String> = self
            .controllers()
            .get(sel)
            .map(|d| d.leds.iter().map(|l| l.name.clone()).collect())
            .unwrap_or_default();

        // Local preview frame (owned; no engine involvement).
        let m = self.master as f32 / 100.0;
        let sources: Vec<ZoneSource> =
            self.zones[sel].iter().map(|z| z.to_source(&self.library)).collect();
        let mut frame = render_plan(&sources, &self.spread, t);
        for c in &mut frame {
            *c = scale(*c, m);
        }
        let groups = shared_clock_groups(&sources);
        let n = sources.len();

        let mut local_dirty = false;
        let zrow = &mut self.zones[sel];
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(SP_M);
            for zi in 0..n {
                let preview = frame.get(zi).copied().unwrap_or(Rgb(0, 0, 0));
                let label = led_names.get(zi).map(|s| s.as_str()).unwrap_or("");
                zone_card(ui, zi, label, &mut zrow[zi], preview, &groups, &customs, &mut local_dirty);
            }
        });
        self.dirty |= local_dirty;

        // Shared-clock groups with Link/Spread per group.
        if groups.is_empty() {
            return;
        }
        ui.add_space(SP_L);
        widgets::section_header(ui, "Shared clocks", |_| {});
        ui.horizontal(|ui| {
            ui.add_space(SP_XS);
            ui.label(theme::caption(
                ui,
                "Zones running the same effect are phase-locked. Link keeps them identical; \
                 Spread lets the effect travel across them.",
            ));
        });
        ui.add_space(SP_S);
        widgets::narrow(ui, 520.0, |ui| card(ui, |ui| {
            for (gi, g) in groups.iter().enumerate() {
                if gi > 0 {
                    widgets::divider(ui);
                }
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 5.0, rgb32(g.hue));
                    ui.vertical(|ui| {
                        ui.label(theme::strong(g.label.clone()));
                        ui.label(theme::caption(
                            ui,
                            format!(
                                "Zones {}",
                                g.zones.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join(", ")
                            ),
                        ));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut spread = self.spread.contains(&g.key);
                        let segs = [Segment::new(false, "Link"), Segment::new(true, "Spread")];
                        if widgets::segmented(ui, ("spread", &g.key), &mut spread, &segs) {
                            if spread {
                                self.spread.insert(g.key.clone());
                            } else {
                                self.spread.remove(&g.key);
                            }
                            self.dirty = true;
                        }
                    });
                });
            }
        }));
    }
}

/// One zone card (free function so it borrows only the zone + a dirty flag,
/// never `self`).
#[allow(clippy::too_many_arguments)]
fn zone_card(
    ui: &mut egui::Ui,
    zi: usize,
    label: &str,
    z: &mut ZoneUi,
    preview: Rgb,
    groups: &[Group],
    customs: &[(String, [u8; 3])],
    dirty: &mut bool,
) {
    const W: f32 = 232.0;
    let tk = tokens(ui);
    let group_hue = groups.iter().find(|g| g.zones.contains(&zi)).map(|g| rgb32(g.hue));

    ui.allocate_ui_with_layout(Vec2::new(W, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(W);
        card(ui, |ui| {
            // Live preview strip with the zone name inked on top.
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 58.0), Sense::hover());
            paint_preview(ui, rect, preview, group_hue, tk);
            let title = if label.is_empty() { format!("Zone {}", zi + 1) } else { label.to_string() };
            let ink = if theme::luminance(rgb32(preview)) > 0.55 {
                Color32::from_black_alpha(210)
            } else {
                Color32::from_white_alpha(230)
            };
            ui.painter().text(
                rect.left_top() + Vec2::new(12.0, 10.0),
                Align2::LEFT_TOP,
                title,
                FontId::new(13.0, theme::semibold()),
                ink,
            );
            ui.add_space(SP_M);

            // Source.
            let mut tab = match z.kind {
                Kind::Solid => SourceTab::Solid,
                Kind::Program(_) => SourceTab::Program,
                Kind::Custom(_) => SourceTab::Custom,
            };
            let segs = [
                Segment::new(SourceTab::Solid, "Solid"),
                Segment::new(SourceTab::Program, "Program").tint(PROG_HUE),
                Segment::new(SourceTab::Custom, "Custom")
                    .tint(CUST_HUE)
                    .enabled(!customs.is_empty())
                    .hint(if customs.is_empty() { "Create a custom effect first (sidebar › +)." } else { "Your own palette + motion effects." }),
            ];
            if widgets::segmented(ui, ("src", zi), &mut tab, &segs) {
                z.kind = match tab {
                    SourceTab::Solid => Kind::Solid,
                    SourceTab::Program => Kind::Program(Effect::Rainbow),
                    SourceTab::Custom => customs
                        .first()
                        .map(|(n, _)| Kind::Custom(n.clone()))
                        .unwrap_or(Kind::Solid),
                };
                *dirty = true;
            }
            ui.add_space(SP_S);

            let label_w = 64.0;
            let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(label_w, 28.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(theme::caption(ui, label));
                        },
                    );
                    add(ui);
                });
            };

            match &mut z.kind {
                Kind::Solid => {
                    row(ui, "Color", &mut |ui| {
                        if color_field(ui, &mut z.color) {
                            *dirty = true;
                        }
                    });
                }
                Kind::Program(e) => {
                    row(ui, "Effect", &mut |ui| {
                        egui::ComboBox::from_id_source(("prog", zi))
                            .width(W - label_w - 40.0)
                            .selected_text(e.label())
                            .show_ui(ui, |ui| {
                                for opt in Effect::ALL {
                                    if ui.selectable_value(e, opt, opt.label()).clicked() {
                                        *dirty = true;
                                    }
                                }
                            });
                    });
                    row(ui, "Color", &mut |ui| {
                        if color_field(ui, &mut z.color) {
                            *dirty = true;
                        }
                    });
                }
                Kind::Custom(name) => {
                    row(ui, "Effect", &mut |ui| {
                        egui::ComboBox::from_id_source(("cust", zi))
                            .width(W - label_w - 40.0)
                            .selected_text(name.clone())
                            .show_ui(ui, |ui| {
                                for (n, _) in customs {
                                    if ui.selectable_value(name, n.clone(), n).clicked() {
                                        *dirty = true;
                                    }
                                }
                            });
                    });
                }
            }

            let slider_w = W - label_w - 84.0;
            if !matches!(z.kind, Kind::Solid) {
                row(ui, "Speed", &mut |ui| {
                    ui.spacing_mut().slider_width = slider_w;
                    if ui.add(egui::Slider::new(&mut z.speed, 1..=9).show_value(false)).changed() {
                        *dirty = true;
                    }
                    ui.label(theme::caption(ui, format!("{}", z.speed)));
                });
            }
            row(ui, "Bright.", &mut |ui| {
                ui.spacing_mut().slider_width = slider_w;
                if ui
                    .add(egui::Slider::new(&mut z.brightness, 0..=100).show_value(false))
                    .changed()
                {
                    *dirty = true;
                }
                ui.label(theme::caption(ui, format!("{}%", z.brightness)));
            });
        });
    });
}

/// The zone's live look: a rounded color slab with a soft glow, and — when it
/// shares a clock with other zones — a hairline in the group hue along the top
/// (the "reads the global clock" mark from the mockup).
fn paint_preview(ui: &egui::Ui, rect: Rect, color: Rgb, group_hue: Option<Color32>, tk: Tokens) {
    let p = ui.painter();
    let c = rgb32(color);
    p.rect_filled(rect.expand(3.0), R_CONTROL + 5.0, wash(c, 40));
    p.rect(rect, R_CONTROL + 2.0, c, Stroke::new(1.0_f32, wash(Color32::WHITE, 50)));
    if let Some(h) = group_hue {
        let y = rect.top() + 2.5;
        p.line_segment(
            [egui::pos2(rect.left() + 10.0, y), egui::pos2(rect.right() - 10.0, y)],
            Stroke::new(2.0_f32, h),
        );
        p.circle_filled(rect.right_top() + Vec2::new(-12.0, 12.0), 5.0, h);
        p.circle_stroke(rect.right_top() + Vec2::new(-12.0, 12.0), 5.0, Stroke::new(1.0_f32, tk.surface));
    }
}
