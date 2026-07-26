use crate::config::Config;
use crate::git_update::{UpdateAvailable, UpdateState};
use eframe::egui;
use std::sync::mpsc;

pub struct ColormemuchApp {
    pub config: Config,
    pub status: Option<String>,

    // The RGB control screen.
    #[cfg(windows)]
    rgb: crate::ui::RgbControl,

    // Tray presence so closing the window keeps the engine holding colors.
    #[cfg(windows)]
    tray: Option<crate::tray::Tray>,

    // Self-update plumbing.
    pub update_state: UpdateState,
    pub update_error: Option<String>,
    pub update_rx: Option<mpsc::Receiver<Option<UpdateAvailable>>>,
}

impl ColormemuchApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        cc.egui_ctx.set_visuals(if config.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        cc.egui_ctx.set_zoom_factor(config.zoom);

        // Kick off an update check in the background so the status bar can surface it.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git_update::check_latest_release());
        });

        Self {
            config,
            status: None,
            #[cfg(windows)]
            rgb: crate::ui::RgbControl::new(&cc.egui_ctx),
            #[cfg(windows)]
            tray: crate::tray::Tray::new(),
            update_state: UpdateState::Checking,
            update_error: None,
            update_rx: Some(rx),
        }
    }
}

impl eframe::App for ColormemuchApp {
    /// Called by eframe periodically and on exit — persist the lighting setup
    /// here so there's no per-frame disk IO.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        #[cfg(windows)]
        self.rgb.save_setup();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tray + close-to-tray: keep the engine holding colors after the window
        // is dismissed. Only the tray's Quit actually exits.
        #[cfg(windows)]
        if let Some(tray) = &self.tray {
            match tray.poll() {
                Some(crate::tray::TrayAction::Show) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Some(crate::tray::TrayAction::Quit) => std::process::exit(0),
                None => {}
            }

            if ctx.input(|i| i.viewport().close_requested()) {
                // Cancel the close and hide to the tray instead of exiting.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            // Keep ticking while hidden so tray events are still polled.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Quit").clicked() {
                        std::process::exit(0);
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.checkbox(&mut self.config.dark_mode, "Dark mode").changed() {
                        ctx.set_visuals(if self.config.dark_mode {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        self.config.save();
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                crate::git_update::render(
                    ui,
                    &mut self.update_state,
                    &mut self.update_error,
                    &mut self.update_rx,
                );
                ui.separator();
                if let Some(s) = self.status.as_ref() {
                    ui.label(s);
                }
            });
        });

        // The RGB control screen owns the left device panel + central editor.
        #[cfg(windows)]
        self.rgb.show(ctx);

        #[cfg(not(windows))]
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading(crate::APP_NAME);
                ui.label("Windows only.");
            });
        });
    }
}
