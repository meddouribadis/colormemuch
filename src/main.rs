#![windows_subsystem = "windows"]

// The GUI binary — a thin host over the `colormemuch` core lib. Core lighting
// modules live in the lib; only the UI shell is here.
mod app;
mod config;
mod git_update;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod ui;

use eframe::egui;

// App identity. `APP_NAME` is owned by the core lib (config/profile paths);
// re-export it so `crate::APP_NAME` keeps working in the GUI modules.
#[cfg(windows)]
pub use colormemuch::APP_NAME;
#[cfg(not(windows))]
pub const APP_NAME: &str = "ColorMeMuch";
pub const APP_WINDOW_TITLE: &str = "ColorMeMuch";
// GitHub repo in "owner/repo" form — used by the update checker.
pub const APP_GH_REPO: &str = "ophiocus/colormemuch";

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title(APP_WINDOW_TITLE),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| Ok(Box::new(app::ColormemuchApp::new(cc)))),
    )
}
