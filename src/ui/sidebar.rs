//! Left sidebar — devices, the custom-effect library, master brightness.

use eframe::egui::{self, Align2, FontId, Sense, Vec2};

use super::theme::{self, tokens, SP_L, SP_M, SP_S, SP_XS};
use super::widgets::{self, arr32, icon_button, info_button, list_row, section_header};
use super::{short_name, Conn, RgbControl};

impl RgbControl {
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui, t: f32) {
        ui.spacing_mut().item_spacing.y = 2.0;

        // Master brightness is pinned to the bottom; the rest scrolls.
        egui::TopBottomPanel::bottom("master")
            .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(SP_XS, SP_S)))
            .show_separator_line(false)
            .show_inside(ui, |ui| self.master_row(ui));

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.devices_section(ui);
                ui.add_space(SP_L);
                self.effects_section(ui, t);
            });
    }

    fn devices_section(&mut self, ui: &mut egui::Ui) {
        let tk = tokens(ui);
        section_header(ui, "Devices", |ui| {
            if info_button(ui)
                .on_hover_text("Show the discovered device tree")
                .clicked()
            {
                self.inspector_open = !self.inspector_open;
            }
        });

        match &self.conn {
            Conn::Connecting => {
                ui.horizontal(|ui| {
                    ui.add_space(SP_M);
                    ui.spinner();
                    ui.label(theme::caption(ui, "Looking for devices…"));
                });
            }
            Conn::Failed(e) => {
                let e = e.clone();
                widgets::banner(ui, tk.danger, "No lighting server", &e);
                ui.add_space(SP_XS);
                ui.horizontal(|ui| {
                    ui.add_space(SP_XS);
                    if ui.button("Try again").clicked() {
                        self.reconnect();
                    }
                });
            }
            Conn::Ready(controllers) => {
                let rows: Vec<(String, String, [u8; 3])> = controllers
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let n = c.led_count as usize;
                        let sub = format!(
                            "{} · {} zone{}",
                            c.kind.label(),
                            n,
                            if n == 1 { "" } else { "s" }
                        );
                        let sw = self.zones[i].first().map(|z| z.color).unwrap_or([80, 80, 80]);
                        (short_name(&c.name), sub, sw)
                    })
                    .collect();
                for (i, (name, sub, sw)) in rows.iter().enumerate() {
                    let selected = !self.case_selected() && self.selected == i;
                    let color = arr32(*sw);
                    let r = list_row(
                        ui,
                        selected,
                        |ui, rect| {
                            let tk = tokens(ui);
                            widgets::paint_swatch(ui, rect.shrink(2.0), color, true, tk);
                        },
                        name,
                        sub,
                    );
                    if r.clicked() {
                        self.selected = i;
                    }
                }
                if self.case_offered() {
                    let selected = self.case_selected();
                    let (color, on) = match &self.dt {
                        Some(dt) => (arr32(dt.color), dt.on),
                        None => (tk.text3, false),
                    };
                    let sub = if self.dt_available { "Tower · WMI" } else { "Tower · unavailable" };
                    let r = list_row(
                        ui,
                        selected,
                        |ui, rect| {
                            let tk = tokens(ui);
                            widgets::paint_swatch(ui, rect.shrink(2.0), color, on, tk);
                        },
                        "Tower case",
                        sub,
                    );
                    if r.clicked() {
                        self.selected = self.case_index();
                    }
                }
                if controllers.is_empty() && !self.case_offered() {
                    ui.label(theme::hint(ui, "No RGB devices found."));
                }
            }
        }
    }

    fn effects_section(&mut self, ui: &mut egui::Ui, t: f32) {
        let tk = tokens(ui);
        section_header(ui, "Custom effects", |ui| {
            if icon_button(ui, "+").on_hover_text("New effect").clicked() {
                self.open_new_effect();
            }
        });

        if self.library.effects.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(SP_S);
                ui.label(theme::hint(
                    ui,
                    "Build palette-based effects and assign them to any zone.",
                ));
            });
            return;
        }

        let names: Vec<String> = self.library.effects.iter().map(|e| e.name.clone()).collect();
        for name in names {
            let Some(e) = self.library.get(&name) else { continue };
            let colors = e.sample(t, 12, 1.0, 1.0);
            let motion = e.motion.label().to_string();
            let r = list_row(
                ui,
                false,
                |ui, rect| {
                    let strip = egui::Rect::from_center_size(rect.center(), Vec2::new(22.0, 22.0));
                    widgets::paint_gradient(ui, strip, &colors, 6.0);
                },
                &name,
                &motion,
            );
            // Subtle trailing "edit" affordance on hover.
            if r.hovered() {
                ui.painter().text(
                    egui::pos2(r.rect.right() - 12.0, r.rect.center().y),
                    Align2::RIGHT_CENTER,
                    "›",
                    FontId::proportional(16.0),
                    tk.text3,
                );
            }
            if r.clicked() {
                self.open_edit_effect(&name);
            }
        }
    }

    fn master_row(&mut self, ui: &mut egui::Ui) {
        let tk = tokens(ui);
        widgets::divider(ui);
        ui.horizontal(|ui| {
            ui.label(theme::section(ui, "Master brightness"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{}%", self.master))
                        .monospace()
                        .color(tk.text2),
                );
            });
        });
        ui.add_space(SP_XS);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                "☀",
                FontId::proportional(14.0),
                tk.text2,
            );
            ui.spacing_mut().slider_width = ui.available_width() - SP_S;
            if ui
                .add(egui::Slider::new(&mut self.master, 0..=100).show_value(false))
                .changed()
            {
                self.dirty = true;
            }
        });
    }
}
