//! Floating windows: the custom-effect editor and the read-only device
//! inspector.

use eframe::egui::{self, Vec2};

use colormemuch::library::{ColorStop, Motion};

use super::theme::{self, tokens, SP_M, SP_S, SP_XS};
use super::widgets::{self, form_row, primary_button, quiet_button, Segment};
use super::{Conn, RgbControl};

impl RgbControl {
    pub(super) fn effect_editor_window(&mut self, ctx: &egui::Context, t: f32) {
        let Some(mut ed) = self.editor.take() else {
            return;
        };
        let mut keep_open = true;
        let mut save = false;
        let mut delete = false;
        let is_new = ed.replacing.is_none();

        egui::Window::new(if is_new { "New effect" } else { "Edit effect" })
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .min_width(420.0)
            .max_width(420.0)
            .show(ctx, |ui| {
                let tk = tokens(ui);
                ui.spacing_mut().item_spacing.y = SP_S;
                ui.add_space(SP_XS);
                ui.label(theme::heading(if is_new { "New effect" } else { "Edit effect" }));
                ui.label(theme::caption(
                    ui,
                    "A palette and a motion. Assign it to any zone from the Custom tab.",
                ));
                ui.add_space(SP_S);

                // Live preview.
                let colors = ed.draft.sample(t, 24, 1.0, 1.0);
                widgets::gradient_bar(ui, &colors, Vec2::new(ui.available_width(), 44.0), 10.0);
                ui.add_space(SP_S);

                form_row(ui, "Name", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut ed.draft.name)
                            .desired_width(ui.available_width())
                            .hint_text("Aurora"),
                    );
                });
                form_row(ui, "Motion", |ui| {
                    let segs: Vec<Segment<Motion>> =
                        Motion::ALL.into_iter().map(|m| Segment::new(m, m.label())).collect();
                    widgets::segmented(ui, "motion", &mut ed.draft.motion, &segs);
                });
                form_row(ui, "Speed", |ui| {
                    ui.spacing_mut().slider_width = 220.0;
                    ui.add(egui::Slider::new(&mut ed.draft.speed, 0.1..=4.0).show_value(false));
                    ui.label(theme::caption(ui, format!("{:.1}×", ed.draft.speed)));
                });
                form_row(ui, "Brightness", |ui| {
                    ui.spacing_mut().slider_width = 220.0;
                    ui.add(egui::Slider::new(&mut ed.draft.brightness, 0.0..=1.0).show_value(false));
                    ui.label(theme::caption(ui, format!("{}%", (ed.draft.brightness * 100.0).round())));
                });

                ui.add_space(SP_S);
                widgets::section_header(ui, "Palette", |ui| {
                    if widgets::icon_button(ui, "+").on_hover_text("Add a color stop").clicked() {
                        ed.draft.palette.push(ColorStop { pos: 1.0, rgb: [0xFF, 0xFF, 0xFF] });
                    }
                });
                let mut remove: Option<usize> = None;
                let can_remove = ed.draft.palette.len() > 1;
                widgets::well(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = SP_XS;
                    for (i, stop) in ed.draft.palette.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.color_edit_button_srgb(&mut stop.rgb);
                            ui.spacing_mut().slider_width = 200.0;
                            ui.add(egui::Slider::new(&mut stop.pos, 0.0..=1.0).show_value(false));
                            ui.label(
                                egui::RichText::new(format!("{:>3}%", (stop.pos * 100.0).round()))
                                    .monospace()
                                    .color(tk.text2),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add_enabled(can_remove, egui::Button::new("✕").frame(false))
                                    .on_hover_text("Remove stop")
                                    .clicked()
                                {
                                    remove = Some(i);
                                }
                            });
                        });
                    }
                });
                if let Some(i) = remove {
                    ed.draft.palette.remove(i);
                }

                ui.add_space(SP_M);
                ui.horizontal(|ui| {
                    if !is_new && quiet_button(ui, "Delete", tk.danger).clicked() {
                        delete = true;
                        keep_open = false;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let valid = !ed.draft.name.trim().is_empty();
                        if ui.add_enabled_ui(valid, |ui| primary_button(ui, "Save")).inner.clicked() {
                            save = true;
                            keep_open = false;
                        }
                        if quiet_button(ui, "Cancel", tk.text2).clicked() {
                            keep_open = false;
                        }
                    });
                });
                ui.add_space(SP_XS);
            });

        if save && !ed.draft.name.trim().is_empty() {
            if let Some(old) = &ed.replacing {
                if old != &ed.draft.name {
                    self.library.remove(old);
                }
            }
            self.library.upsert(ed.draft.clone());
            self.dirty = true;
        }
        if delete {
            if let Some(old) = &ed.replacing {
                self.library.remove(old);
                self.dirty = true;
            }
        }
        if keep_open {
            self.editor = Some(ed);
        }
    }

    // ---- devices inspector (read-only discovery surface) -------------------

    pub(super) fn devices_inspector(&mut self, ctx: &egui::Context) {
        if !self.inspector_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Discovered devices")
            .open(&mut open)
            .default_width(480.0)
            .show(ctx, |ui| {
                let tk = tokens(ui);
                let Conn::Ready(controllers) = &self.conn else {
                    ui.label(theme::caption(ui, "Not connected to an OpenRGB server."));
                    return;
                };
                if controllers.is_empty() {
                    ui.label(theme::caption(
                        ui,
                        "No OpenRGB controllers exposed (on this tower the case is WMI-driven).",
                    ));
                }
                egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                    for d in controllers {
                        widgets::card(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(theme::heading(d.name.clone()));
                                widgets::chip(ui, tk.accent, d.kind.label());
                            });
                            if !d.vendor.is_empty() || !d.description.is_empty() {
                                ui.label(theme::caption(ui, format!("{} — {}", d.vendor, d.description)));
                            }
                            ui.add_space(SP_XS);
                            for z in &d.zones {
                                let m = z
                                    .matrix
                                    .as_ref()
                                    .map(|m| format!(" · matrix {}×{}", m.height, m.width))
                                    .unwrap_or_default();
                                ui.label(format!(
                                    "{}  ·  {} · {} LEDs{}",
                                    z.name,
                                    z.kind.label(),
                                    z.leds_count,
                                    m
                                ));
                            }
                            let names: Vec<&str> = d.leds.iter().map(|l| l.name.as_str()).collect();
                            ui.label(theme::hint(ui, format!("LEDs: {}", names.join(", "))));
                            ui.collapsing(format!("Modes ({})", d.modes.len()), |ui| {
                                for md in &d.modes {
                                    let mut caps = Vec::new();
                                    if md.has_speed {
                                        caps.push("speed");
                                    }
                                    if md.has_brightness {
                                        caps.push("brightness");
                                    }
                                    if md.has_direction {
                                        caps.push("direction");
                                    }
                                    if md.takes_color {
                                        caps.push("color");
                                    }
                                    if md.can_save {
                                        caps.push("save");
                                    }
                                    ui.horizontal(|ui| {
                                        ui.label(theme::strong(md.name.clone()));
                                        ui.label(theme::hint(ui, caps.join(" · ")));
                                    });
                                }
                            });
                        });
                        ui.add_space(SP_S);
                    }
                });
            });
        if !open {
            self.inspector_open = false;
        }
    }
}
