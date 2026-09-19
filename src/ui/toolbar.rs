//! The slim top toolbar: brand on the left; status pill, update dot and the
//! ⚙ settings popover on the right. The old "persistence spine" lives here as
//! two toggles and one honest status line.

use eframe::egui::{self, Sense, Vec2};

use super::theme::{self, tokens, wash, SP_S, SP_XS};
use super::widgets::{self, icon_button, status_pill, toggle_row};
use super::{Conn, RgbControl};
use crate::config::Config;

/// Toolbar height, for the panel in `app.rs`.
pub const HEIGHT: f32 = 54.0;

impl RgbControl {
    /// `updater` renders the version / update-check line (owned by `app.rs`).
    pub fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut Config,
        update_available: bool,
        updater: impl FnOnce(&mut egui::Ui),
    ) {
        let tk = tokens(ui);
        ui.horizontal_centered(|ui| {
            // Brand.
            let (r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
            paint_logo(ui, r);
            ui.add_space(SP_XS);
            ui.label(egui::RichText::new(crate::APP_WINDOW_TITLE).text_style(egui::TextStyle::Heading));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = SP_S;

                // ⚙ with an update dot.
                let gear = icon_button(ui, "⚙").on_hover_text("Settings");
                if update_available {
                    ui.painter().circle_filled(
                        gear.rect.right_top() + Vec2::new(-6.0, 6.0),
                        4.0,
                        tk.accent,
                    );
                }
                let popup_id = ui.make_persistent_id("settings-popover");
                if gear.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(popup_id));
                }
                egui::popup_below_widget(
                    ui,
                    popup_id,
                    &gear,
                    egui::PopupCloseBehavior::CloseOnClickOutside,
                    |ui| {
                        ui.set_min_width(300.0);
                        ui.set_max_width(300.0);
                        self.settings_popover(ui, config, updater);
                    },
                );

                // Battery / reactive layer.
                if self.battery_saver && self.on_battery {
                    status_pill(ui, tk.warn, "On battery")
                        .on_hover_text("Battery saver is dimming and warming the frame.");
                }

                // Connection / ownership.
                let via_service = self.host.via_service();
                let (hue, text, tip) = match &self.conn {
                    Conn::Connecting => (tk.text3, "Connecting…", "Looking for the lighting server."),
                    Conn::Failed(_) => (tk.danger, "Offline", "No OpenRGB server reachable. Open Settings › Try again, or start PredatorSense."),
                    Conn::Ready(_) if via_service && self.hold => (
                        tk.success,
                        "Held · Service",
                        "Your lighting is re-asserted every few seconds and survives closing this window.",
                    ),
                    Conn::Ready(_) if via_service => (
                        tk.success,
                        "Service",
                        "The background service owns the lights; they persist after you close this window.",
                    ),
                    Conn::Ready(_) if self.hold => (
                        tk.success,
                        "Holding",
                        "Re-asserting your lighting every few seconds while the app runs (also from the tray).",
                    ),
                    Conn::Ready(_) => (
                        tk.warn,
                        "Live",
                        "Colors are pushed on change only. Turn on “Keep my lighting” so PredatorSense can't repaint over them.",
                    ),
                };
                status_pill(ui, hue, text).on_hover_text(tip);
            });
        });
    }

    fn settings_popover(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut Config,
        updater: impl FnOnce(&mut egui::Ui),
    ) {
        let tk = tokens(ui);
        ui.spacing_mut().item_spacing.y = SP_S;
        ui.add_space(SP_XS);

        ui.label(theme::section(ui, "Appearance"));
        if toggle_row(ui, "Dark mode", None, &mut config.dark_mode) {
            theme::apply(ui.ctx(), config.dark_mode);
            config.save();
        }
        ui.horizontal(|ui| {
            ui.label(theme::strong("Text size"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut z = (config.zoom * 100.0).round() as u32;
                let segs = [
                    widgets::Segment::new(90u32, "S"),
                    widgets::Segment::new(100u32, "M"),
                    widgets::Segment::new(115u32, "L"),
                ];
                if widgets::segmented(ui, "zoom", &mut z, &segs) {
                    config.zoom = z as f32 / 100.0;
                    ui.ctx().set_zoom_factor(config.zoom);
                    config.save();
                }
            });
        });

        widgets::divider(ui);
        ui.label(theme::section(ui, "Lighting"));
        if toggle_row(
            ui,
            "Keep my lighting",
            Some("Re-assert your colors so PredatorSense can't repaint them."),
            &mut self.hold,
        ) {
            self.dirty = true;
        }
        if toggle_row(
            ui,
            "Battery saver",
            Some("On battery, dim and warm the lights."),
            &mut self.battery_saver,
        ) {
            self.dirty = true;
        }
        ui.label(theme::hint(
            ui,
            if self.host.via_service() {
                "Closing the window keeps your lighting — the service holds it."
            } else {
                "Closing the window keeps the app in the tray so your lighting stays."
            },
        ));

        widgets::divider(ui);
        ui.label(theme::section(ui, "Advanced"));
        ui.horizontal(|ui| {
            if ui.button("Device inspector").clicked() {
                self.inspector_open = true;
                ui.memory_mut(|m| m.close_popup());
            }
            if matches!(self.conn, Conn::Failed(_)) && ui.button("Try again").clicked() {
                self.reconnect();
            }
        });

        widgets::divider(ui);
        ui.label(theme::section(ui, "About"));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = SP_XS;
            ui.label(theme::caption(ui, format!("{} ", crate::APP_WINDOW_TITLE)));
            updater(ui);
        });

        ui.add_space(SP_XS);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::quiet_button(ui, "Quit", tk.danger)
                .on_hover_text("Stop the app (lighting is released to PredatorSense unless the service runs).")
                .clicked()
            {
                std::process::exit(0);
            }
        });
    }
}

/// The brand mark: a rounded tile with a three-hue arc — the "color me" glyph.
fn paint_logo(ui: &egui::Ui, rect: egui::Rect) {
    let p = ui.painter();
    let tk = tokens(ui);
    p.rect_filled(rect, 7.0, tk.elevated);
    let c = rect.center();
    let hues = [
        egui::Color32::from_rgb(0x00, 0xe5, 0xff),
        egui::Color32::from_rgb(0xb0, 0x7c, 0xff),
        egui::Color32::from_rgb(0xff, 0x5c, 0x8a),
    ];
    for (i, h) in hues.iter().enumerate() {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * 2.1;
        let pos = c + Vec2::new(a.cos(), a.sin()) * 5.5;
        p.circle_filled(pos, 6.5, wash(*h, 150));
    }
    p.circle_filled(c, 3.0, egui::Color32::WHITE);
}
