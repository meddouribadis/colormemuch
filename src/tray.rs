//! System-tray presence, so closing the window doesn't stop the lighting.
//!
//! The window's close button hides to the tray instead of exiting; the engine
//! thread keeps holding the profile against Acer. Only the tray's **Quit**
//! actually ends the process. Menu events are polled from `app::update` — the
//! egui/winit loop pumps the OS messages tray-icon needs.

#![cfg(windows)]

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub enum TrayAction {
    Show,
    Quit,
}

pub struct Tray {
    _tray: TrayIcon,
    show_id: MenuId,
    quit_id: MenuId,
}

impl Tray {
    pub fn new() -> Option<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("Show colormemuch", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append(&show).ok()?;
        menu.append(&quit).ok()?;
        let show_id = show.id().clone();
        let quit_id = quit.id().clone();

        let (rgba, w, h) = brand_icon();
        let icon = Icon::from_rgba(rgba, w, h).ok()?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(crate::APP_NAME)
            .with_icon(icon)
            .build()
            .ok()?;

        Some(Self {
            _tray: tray,
            show_id,
            quit_id,
        })
    }

    /// Drain pending tray-menu events; returns the last actionable one.
    pub fn poll(&self) -> Option<TrayAction> {
        let mut action = None;
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == self.show_id {
                action = Some(TrayAction::Show);
            } else if ev.id == self.quit_id {
                action = Some(TrayAction::Quit);
            }
        }
        action
    }
}

/// A 32×32 diagonal brand gradient (pink → blue → teal → amber), matching the
/// app's mark, synthesized so we ship no icon asset for the tray.
fn brand_icon() -> (Vec<u8>, u32, u32) {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let t = (x + y) as f32 / (W + H - 2) as f32;
            let (r, g, b) = grad(t);
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    (px, W, H)
}

fn grad(t: f32) -> (u8, u8, u8) {
    // Four brand stops across the diagonal.
    let stops = [
        (0.00, (0xD4, 0x53, 0x7E)),
        (0.40, (0x37, 0x8A, 0xDD)),
        (0.72, (0x1D, 0x9E, 0x75)),
        (1.00, (0xEF, 0x9F, 0x27)),
    ];
    let t = t.clamp(0.0, 1.0);
    for w in stops.windows(2) {
        let (p0, c0) = w[0];
        let (p1, c1) = w[1];
        if t >= p0 && t <= p1 {
            let f = (t - p0) / (p1 - p0).max(1e-4);
            return (
                lerp(c0.0, c1.0, f),
                lerp(c0.1, c1.1, f),
                lerp(c0.2, c1.2, f),
            );
        }
    }
    stops[stops.len() - 1].1
}

fn lerp(a: u8, b: u8, f: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * f) as u8
}
