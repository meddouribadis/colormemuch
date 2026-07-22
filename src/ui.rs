//! The egui RGB control screen.
//!
//! Three effect types, wired to their nature:
//! * **Core** — firmware effects; whole-device, zero host CPU. A device-level
//!   mode ([`DeviceMode::Hardware`]) set once via `apply_effect`.
//! * **Program** — our built-in `fn(t,n)` effects; per-zone, animated.
//! * **Custom** — user-built palette+motion effects from the [`EffectLibrary`];
//!   per-zone, animated, and fully editable in the in-app editor.

#![cfg(windows)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};

use crate::effects::{render_plan, scale, shared_clock_groups, Effect, Fx, Group, Params, ZoneSource};
use crate::library::{ColorStop, CustomEffect, EffectLibrary, Motion};
use crate::openrgb::{Controller, OpenRgb};
use crate::rgb::Rgb;

/// Animation/repaint rate is chosen per active effect between these bounds — a
/// breathing effect needs far fewer frames than a comet.
const MIN_HZ: f32 = 8.0;
const MAX_HZ: f32 = 30.0;

// Type accents (match the mockup tokens).
const CORE_HUE: Color32 = Color32::from_rgb(0x4b, 0xbf, 0x73); // green: 0 CPU
const PROG_HUE: Color32 = Color32::from_rgb(0x1f, 0xb7, 0xa6); // teal
const CUST_HUE: Color32 = Color32::from_rgb(0xb0, 0x7c, 0xff); // purple

/// A zone's animated/static source choice.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
enum Kind {
    Solid,
    Program(Effect),
    Custom(String),
}

#[derive(Clone, Serialize, Deserialize)]
struct ZoneUi {
    kind: Kind,
    color: [u8; 3],
    speed: u32,      // 1..=9
    brightness: u32, // 0..=100
}

impl Default for ZoneUi {
    fn default() -> Self {
        Self {
            kind: Kind::Solid,
            color: [0x00, 0xE5, 0xFF],
            speed: 5,
            brightness: 100,
        }
    }
}

impl ZoneUi {
    fn to_source(&self, lib: &EffectLibrary) -> ZoneSource {
        let color = Rgb(self.color[0], self.color[1], self.color[2]);
        let params = Params {
            color,
            color_b: Rgb(0xFF, 0x00, 0x88),
            speed: self.speed as f32 / 5.0,
            brightness: self.brightness as f32 / 100.0,
        };
        match &self.kind {
            Kind::Solid => ZoneSource::Solid(color),
            Kind::Program(e) => ZoneSource::Function(Fx::Program(*e), params),
            Kind::Custom(name) => match lib.get(name) {
                Some(c) => ZoneSource::Function(Fx::Custom(c.clone()), params),
                None => ZoneSource::Solid(color),
            },
        }
    }
}

/// A whole-device firmware (Core) effect — the zero-CPU path.
#[derive(Clone, Serialize, Deserialize)]
struct HwEffect {
    mode: String,
    color: [u8; 3],
    speed: u32,
    brightness: u32,
}

impl HwEffect {
    fn signature(&self) -> String {
        format!(
            "{}|{:?}|{}|{}",
            self.mode, self.color, self.speed, self.brightness
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
enum DeviceMode {
    PerZone,
    Hardware(HwEffect),
}

/// The persisted lighting setup — per-device mode + zones, keyed by controller
/// name (stable across restarts), plus master brightness and the Spread set.
#[derive(Default, Serialize, Deserialize)]
struct LightingSetup {
    master: u32,
    spread: Vec<String>,
    devices: HashMap<String, DeviceSetup>,
}

#[derive(Serialize, Deserialize)]
struct DeviceSetup {
    mode: DeviceMode,
    zones: Vec<ZoneUi>,
}

impl LightingSetup {
    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join(crate::APP_NAME).join("setup.json"))
    }
    fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        let Some(p) = Self::path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }
}

/// State of the custom-effect editor window.
struct EditorState {
    /// Name being replaced (rename support); None for a brand-new effect.
    replacing: Option<String>,
    draft: CustomEffect,
}

enum Conn {
    Connecting,
    Ready {
        client: OpenRgb,
        controllers: Vec<Controller>,
        zones: Vec<Vec<ZoneUi>>,
        dev_mode: Vec<DeviceMode>,
        direct_set: Vec<bool>,
        applied_hw: Vec<Option<String>>,
        last_frame: Vec<Vec<Rgb>>,
    },
    Failed(String),
}

type ConnResult = Result<(OpenRgb, Vec<Controller>), String>;

pub struct RgbControl {
    rx: Option<mpsc::Receiver<ConnResult>>,
    conn: Conn,
    selected: usize,
    spread: HashSet<String>,
    master: u32,
    clock: Instant,
    last_push: Instant,
    dirty: bool,
    library: EffectLibrary,
    editor: Option<EditorState>,
}

impl RgbControl {
    pub fn new() -> Self {
        Self {
            rx: Some(spawn_connect()),
            conn: Conn::Connecting,
            selected: 0,
            spread: HashSet::new(),
            master: 100,
            clock: Instant::now(),
            last_push: Instant::now(),
            dirty: true,
            library: EffectLibrary::load(),
            editor: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll_connection();
        let t = self.clock.elapsed().as_secs_f32();

        egui::SidePanel::left("devices")
            .exact_width(210.0)
            .show(ctx, |ui| self.side_panel(ui));

        egui::CentralPanel::default().show(ctx, |ui| self.editor_view(ui, t));

        self.effect_editor_window(ctx, t);

        let hz = self.active_hz();
        let animating = self.push_if_due(t, hz);
        if animating || self.editor.is_some() {
            ctx.request_repaint_after(Duration::from_secs_f32(1.0 / hz));
        }
    }

    /// Pick a frame rate from the busiest active effect — a breathing pulse
    /// wants ~10 Hz, a comet ~24. Idle → the floor. Keeps CPU proportional to
    /// what's actually moving.
    fn active_hz(&self) -> f32 {
        let Conn::Ready { zones, dev_mode, .. } = &self.conn else {
            return MIN_HZ;
        };
        let mut hz = MIN_HZ;
        for (i, m) in dev_mode.iter().enumerate() {
            if !matches!(m, DeviceMode::PerZone) {
                continue;
            }
            for z in &zones[i] {
                let want = match &z.kind {
                    Kind::Solid => 0.0,
                    Kind::Program(e) => match e {
                        Effect::Comet | Effect::Fire | Effect::Police | Effect::Wave => 24.0,
                        Effect::Rainbow | Effect::Gradient => 16.0,
                        Effect::Breathe => 10.0,
                    },
                    Kind::Custom(name) => match self.library.get(name).map(|c| c.motion) {
                        Some(Motion::Twinkle) => 22.0,
                        Some(Motion::Scroll) | Some(Motion::Bounce) => 18.0,
                        Some(Motion::Pulse) => 10.0,
                        _ => 0.0,
                    },
                };
                hz = hz.max(want);
            }
        }
        hz.clamp(MIN_HZ, MAX_HZ)
    }

    /// Capture and persist the current setup. Called by eframe's periodic /
    /// on-exit save hook, so there's no per-frame disk IO.
    pub fn save_setup(&self) {
        let Conn::Ready {
            controllers,
            zones,
            dev_mode,
            ..
        } = &self.conn
        else {
            return;
        };
        let devices = controllers
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    c.name.clone(),
                    DeviceSetup {
                        mode: dev_mode[i].clone(),
                        zones: zones[i].clone(),
                    },
                )
            })
            .collect();
        LightingSetup {
            master: self.master,
            spread: self.spread.iter().cloned().collect(),
            devices,
        }
        .save();
    }

    fn poll_connection(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(res) = rx.try_recv() {
                self.rx = None;
                self.conn = match res {
                    Ok((client, controllers)) => {
                        let n = controllers.len();
                        let mut zones: Vec<Vec<ZoneUi>> = controllers
                            .iter()
                            .map(|c| vec![ZoneUi::default(); c.led_count as usize])
                            .collect();
                        let mut dev_mode = vec![DeviceMode::PerZone; n];

                        // Restore the saved setup where the device still matches.
                        let setup = LightingSetup::load();
                        for (i, c) in controllers.iter().enumerate() {
                            if let Some(ds) = setup.devices.get(&c.name) {
                                if ds.zones.len() == zones[i].len() {
                                    zones[i] = ds.zones.clone();
                                    dev_mode[i] = ds.mode.clone();
                                }
                            }
                        }
                        if setup.master > 0 || !setup.devices.is_empty() {
                            self.master = setup.master.clamp(0, 100).max(1);
                        }
                        self.spread = setup.spread.into_iter().collect();

                        Conn::Ready {
                            client,
                            controllers,
                            zones,
                            dev_mode,
                            direct_set: vec![false; n],
                            applied_hw: vec![None; n],
                            last_frame: vec![Vec::new(); n],
                        }
                    }
                    Err(e) => Conn::Failed(e),
                };
                self.dirty = true;
            }
        }
    }

    // ---- left panel: devices, custom-effect library, master ----------------

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("DEVICES").weak().small());
        ui.add_space(4.0);

        match &self.conn {
            Conn::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("connecting…");
                });
            }
            Conn::Failed(e) => {
                ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), "no OpenRGB server");
                ui.label(RichText::new(e.as_str()).weak().small());
                if ui.button("Retry").clicked() {
                    self.rx = Some(spawn_connect());
                    self.conn = Conn::Connecting;
                }
            }
            Conn::Ready {
                controllers, zones, ..
            } => {
                for (i, c) in controllers.iter().enumerate() {
                    let sw = zones[i].first().map(|z| z.color).unwrap_or([80, 80, 80]);
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(rect, 2.0, Color32::from_rgb(sw[0], sw[1], sw[2]));
                        if ui
                            .selectable_label(self.selected == i, short_name(&c.name))
                            .clicked()
                        {
                            self.selected = i;
                        }
                    });
                }
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("CUSTOM EFFECTS").weak().small());
            if ui.small_button("+ New").clicked() {
                self.editor = Some(EditorState {
                    replacing: None,
                    draft: CustomEffect::new_default(self.library.fresh_name()),
                });
            }
        });
        let names: Vec<String> = self.library.effects.iter().map(|e| e.name.clone()).collect();
        for name in names {
            ui.horizontal(|ui| {
                if let Some(e) = self.library.get(&name) {
                    let d = e.dominant();
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 2.0, Color32::from_rgb(d.0, d.1, d.2));
                }
                ui.label(RichText::new("★").color(CUST_HUE));
                ui.label(&name);
                if ui.small_button("edit").clicked() {
                    if let Some(e) = self.library.get(&name) {
                        self.editor = Some(EditorState {
                            replacing: Some(name.clone()),
                            draft: e.clone(),
                        });
                    }
                }
            });
        }

        egui::TopBottomPanel::bottom("master")
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                ui.separator();
                ui.label(RichText::new("Master brightness").weak().small());
                if ui
                    .add(egui::Slider::new(&mut self.master, 0..=100).suffix("%"))
                    .changed()
                {
                    self.dirty = true;
                }
            });
    }

    // ---- central editor ----------------------------------------------------

    fn editor_view(&mut self, ui: &mut egui::Ui, t: f32) {
        let sel = self.selected;
        let n_dev = match &self.conn {
            Conn::Ready { controllers, .. } => controllers.len(),
            _ => 0,
        };
        if n_dev == 0 || sel >= n_dev {
            ui.centered_and_justified(|ui| {
                ui.label("Connect to an OpenRGB server to edit lighting.");
            });
            return;
        }

        let (dev_name, mode_names): (String, Vec<String>) = match &self.conn {
            Conn::Ready { controllers, .. } => (
                short_name(&controllers[sel].name),
                controllers[sel]
                    .mode_names()
                    .iter()
                    .filter(|m| !m.eq_ignore_ascii_case("direct"))
                    .map(|s| s.to_string())
                    .collect(),
            ),
            _ => (String::new(), Vec::new()),
        };

        // Header + device-mode toggle.
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(dev_name);
            ui.label(RichText::new(format!("· clock {t:6.2}s")).weak().monospace());
        });
        let is_hardware = matches!(self.dev_mode_of(sel), Some(DeviceMode::Hardware(_)));
        ui.horizontal(|ui| {
            if ui.selectable_label(!is_hardware, "  Per-Zone  ").clicked() && is_hardware {
                self.set_dev_mode(sel, DeviceMode::PerZone);
                self.dirty = true;
            }
            let hw_btn = ui.selectable_label(
                is_hardware,
                RichText::new("  Hardware effect · 0 CPU  ").color(if is_hardware {
                    Color32::WHITE
                } else {
                    CORE_HUE
                }),
            );
            if hw_btn.clicked() && !is_hardware {
                let mode = mode_names.first().cloned().unwrap_or_else(|| "STATIC".into());
                self.set_dev_mode(
                    sel,
                    DeviceMode::Hardware(HwEffect {
                        mode,
                        color: [0x00, 0xE5, 0xFF],
                        speed: 5,
                        brightness: 100,
                    }),
                );
                self.dirty = true;
            }
        });
        ui.separator();
        ui.add_space(8.0);

        if is_hardware {
            self.hardware_panel(ui, sel, &mode_names);
        } else {
            self.per_zone_panels(ui, sel, t);
        }
    }

    fn hardware_panel(&mut self, ui: &mut egui::Ui, sel: usize, mode_names: &[String]) {
        let mut dirty = false;
        if let Some(DeviceMode::Hardware(hw)) = self.dev_mode_of_mut(sel) {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_max_width(360.0);
                ui.label(
                    RichText::new("CORE effect — runs on the keyboard controller, no host CPU")
                        .color(CORE_HUE)
                        .small(),
                );
                ui.add_space(6.0);
                egui::ComboBox::from_label("Effect")
                    .selected_text(hw.mode.clone())
                    .show_ui(ui, |ui| {
                        for m in mode_names {
                            if ui.selectable_value(&mut hw.mode, m.clone(), m).clicked() {
                                dirty = true;
                            }
                        }
                    });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Color");
                    if ui.color_edit_button_srgb(&mut hw.color).changed() {
                        dirty = true;
                    }
                });
                if ui
                    .add(egui::Slider::new(&mut hw.speed, 1..=9).text("Speed"))
                    .changed()
                {
                    dirty = true;
                }
                if ui
                    .add(egui::Slider::new(&mut hw.brightness, 0..=100).suffix("%").text("Bright"))
                    .changed()
                {
                    dirty = true;
                }
            });
        }
        self.dirty |= dirty;
    }

    fn per_zone_panels(&mut self, ui: &mut egui::Ui, sel: usize, t: f32) {
        // Snapshot the library (names + swatches) so we don't hold a borrow of
        // it while mutating the zones.
        let customs: Vec<(String, [u8; 3])> = self
            .library
            .effects
            .iter()
            .map(|e| {
                let d = e.dominant();
                (e.name.clone(), [d.0, d.1, d.2])
            })
            .collect();

        // Build sources + the live preview frame (owned; no borrow held after).
        let (sources, frame) = {
            let m = self.master as f32 / 100.0;
            match &self.conn {
                Conn::Ready { zones, .. } => {
                    let srcs: Vec<ZoneSource> =
                        zones[sel].iter().map(|z| z.to_source(&self.library)).collect();
                    let mut f = render_plan(&srcs, &self.spread, t);
                    for c in &mut f {
                        *c = scale(*c, m);
                    }
                    (srcs, f)
                }
                _ => (Vec::new(), Vec::new()),
            }
        };
        let groups = shared_clock_groups(&sources);
        let n = sources.len();

        let mut local_dirty = false;
        if let Conn::Ready { zones, .. } = &mut self.conn {
            let zrow = &mut zones[sel];
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for zi in 0..n {
                        let preview = frame.get(zi).copied().unwrap_or(Rgb(0, 0, 0));
                        zone_panel(ui, zi, &mut zrow[zi], preview, &groups, &customs, &mut local_dirty);
                    }
                });
            });
        }
        self.dirty |= local_dirty;

        // Shared-clock strip with Link/Spread per group.
        if !groups.is_empty() {
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("SHARED CLOCKS").weak().small());
            for g in &groups {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(
                        rect.center(),
                        5.0,
                        Color32::from_rgb(g.hue.0, g.hue.1, g.hue.2),
                    );
                    ui.label(format!(
                        "{} · zones {}",
                        g.label,
                        g.zones.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join(",")
                    ));
                    let mut spread = self.spread.contains(&g.key);
                    ui.selectable_value(&mut spread, false, "Link");
                    ui.selectable_value(&mut spread, true, "Spread");
                    if spread {
                        if self.spread.insert(g.key.clone()) {
                            self.dirty = true;
                        }
                    } else if self.spread.remove(&g.key) {
                        self.dirty = true;
                    }
                });
            }
        }
    }

    // ---- effect editor window ----------------------------------------------

    fn effect_editor_window(&mut self, ctx: &egui::Context, t: f32) {
        let Some(mut ed) = self.editor.take() else {
            return;
        };
        let mut keep_open = true;
        let mut save = false;
        let mut delete = false;

        egui::Window::new("Effect Editor")
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut ed.draft.name);
                });

                egui::ComboBox::from_label("Motion")
                    .selected_text(ed.draft.motion.label())
                    .show_ui(ui, |ui| {
                        for m in Motion::ALL {
                            ui.selectable_value(&mut ed.draft.motion, m, m.label());
                        }
                    });

                ui.add(egui::Slider::new(&mut ed.draft.speed, 0.1..=4.0).text("Speed"));
                ui.add(egui::Slider::new(&mut ed.draft.brightness, 0.0..=1.0).text("Brightness"));

                ui.add_space(6.0);
                ui.label(RichText::new("Palette").weak().small());
                let mut remove: Option<usize> = None;
                let can_remove = ed.draft.palette.len() > 1;
                for (i, stop) in ed.draft.palette.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.color_edit_button_srgb(&mut stop.rgb);
                        ui.add(
                            egui::Slider::new(&mut stop.pos, 0.0..=1.0)
                                .fixed_decimals(2)
                                .text("pos"),
                        );
                        if ui
                            .add_enabled(can_remove, egui::Button::new("✕").small())
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    ed.draft.palette.remove(i);
                }
                if ui.button("+ Add stop").clicked() {
                    ed.draft.palette.push(ColorStop {
                        pos: 1.0,
                        rgb: [0xFF, 0xFF, 0xFF],
                    });
                }

                // Live preview strip (16 cells).
                ui.add_space(8.0);
                ui.label(RichText::new("Preview").weak().small());
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(300.0, 26.0), egui::Sense::hover());
                let cells = 16usize;
                let colors = ed.draft.sample(t, cells, 1.0, 1.0);
                let cw = rect.width() / cells as f32;
                for (i, c) in colors.iter().enumerate() {
                    let x = rect.left() + i as f32 * cw;
                    let r = egui::Rect::from_min_size(
                        egui::pos2(x, rect.top()),
                        egui::vec2(cw + 1.0, rect.height()),
                    );
                    ui.painter().rect_filled(r, 0.0, Color32::from_rgb(c.0, c.1, c.2));
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                        keep_open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                    if ed.replacing.is_some() && ui.button("Delete").clicked() {
                        delete = true;
                        keep_open = false;
                    }
                });
            });

        if save && !ed.draft.name.trim().is_empty() {
            // If renamed, drop the old entry first.
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
        // Repaint keeps the preview animating.
    }

    // ---- device-mode helpers -----------------------------------------------

    fn dev_mode_of(&self, i: usize) -> Option<&DeviceMode> {
        match &self.conn {
            Conn::Ready { dev_mode, .. } => dev_mode.get(i),
            _ => None,
        }
    }
    fn dev_mode_of_mut(&mut self, i: usize) -> Option<&mut DeviceMode> {
        match &mut self.conn {
            Conn::Ready { dev_mode, .. } => dev_mode.get_mut(i),
            _ => None,
        }
    }
    fn set_dev_mode(&mut self, i: usize, m: DeviceMode) {
        if let Conn::Ready { dev_mode, .. } = &mut self.conn {
            if let Some(slot) = dev_mode.get_mut(i) {
                *slot = m;
            }
        }
    }

    // ---- hardware push -----------------------------------------------------

    fn push_if_due(&mut self, t: f32, hz: f32) -> bool {
        let lib = &self.library;
        let spread = &self.spread;
        let master = self.master as f32 / 100.0;

        let Conn::Ready {
            client,
            controllers,
            zones,
            dev_mode,
            direct_set,
            applied_hw,
            last_frame,
        } = &mut self.conn
        else {
            return false;
        };

        let animating = dev_mode.iter().enumerate().any(|(i, m)| {
            matches!(m, DeviceMode::PerZone)
                && zones[i].iter().any(|z| !matches!(z.kind, Kind::Solid))
        });

        let due = self.last_push.elapsed().as_secs_f32() >= 1.0 / hz;
        if !due && !self.dirty {
            return animating;
        }

        for ci in 0..controllers.len() {
            match &dev_mode[ci] {
                DeviceMode::Hardware(hw) => {
                    let sig = hw.signature();
                    if applied_hw[ci].as_deref() != Some(sig.as_str()) {
                        let _ = client.apply_effect(
                            &controllers[ci],
                            &hw.mode,
                            Rgb(hw.color[0], hw.color[1], hw.color[2]),
                            Some(hw.speed),
                            Some(hw.brightness),
                        );
                        applied_hw[ci] = Some(sig);
                        // We left Direct mode; force re-entry if we go back.
                        direct_set[ci] = false;
                        last_frame[ci].clear();
                    }
                }
                DeviceMode::PerZone => {
                    applied_hw[ci] = None;
                    let srcs: Vec<ZoneSource> =
                        zones[ci].iter().map(|z| z.to_source(lib)).collect();
                    let mut frame = render_plan(&srcs, spread, t);
                    for c in &mut frame {
                        *c = scale(*c, master);
                    }
                    if frame == last_frame[ci] {
                        continue;
                    }
                    if !direct_set[ci] {
                        if client.enter_direct(&controllers[ci]).is_ok() {
                            direct_set[ci] = true;
                        }
                    }
                    if client.update_leds(&controllers[ci], &frame).is_ok() {
                        last_frame[ci] = frame;
                    }
                }
            }
        }
        self.last_push = Instant::now();
        self.dirty = false;
        animating
    }
}

/// One zone control card (free function so it borrows only the zone + a dirty
/// flag, never `self`).
fn zone_panel(
    ui: &mut egui::Ui,
    zi: usize,
    z: &mut ZoneUi,
    preview: Rgb,
    groups: &[Group],
    customs: &[(String, [u8; 3])],
    dirty: &mut bool,
) {
    const W: f32 = 176.0;
    let group_hue = groups
        .iter()
        .find(|g| g.zones.contains(&zi))
        .map(|g| Color32::from_rgb(g.hue.0, g.hue.1, g.hue.2));

    let mut frame = egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .rounding(6.0)
        .inner_margin(egui::Margin::same(8.0));
    if let Some(h) = group_hue {
        frame = frame.stroke(egui::Stroke::new(1.5_f32, h));
    }

    frame.show(ui, |ui| {
        ui.vertical(|ui| {
            ui.set_width(W);
            ui.spacing_mut().slider_width = W - 70.0;

            // Preview header.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(W, 42.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 5.0, Color32::from_rgb(preview.0, preview.1, preview.2));
            let ink = if luminance(preview) > 0.55 {
                Color32::from_black_alpha(200)
            } else {
                Color32::from_white_alpha(220)
            };
            ui.painter().text(
                rect.left_top() + egui::vec2(8.0, 6.0),
                egui::Align2::LEFT_TOP,
                format!("Zone {}", zi + 1),
                egui::FontId::proportional(13.0),
                ink,
            );
            if let Some(h) = group_hue {
                ui.painter()
                    .circle_filled(rect.right_top() + egui::vec2(-10.0, 10.0), 5.0, h);
            }

            ui.add_space(8.0);

            // Three-type selector, color-coded.
            ui.horizontal(|ui| {
                let is_solid = matches!(z.kind, Kind::Solid);
                let is_prog = matches!(z.kind, Kind::Program(_));
                let is_cust = matches!(z.kind, Kind::Custom(_));
                if ui.selectable_label(is_solid, "Solid").clicked() && !is_solid {
                    z.kind = Kind::Solid;
                    *dirty = true;
                }
                if ui
                    .selectable_label(is_prog, RichText::new("ƒ Prog").color(PROG_HUE))
                    .clicked()
                    && !is_prog
                {
                    z.kind = Kind::Program(Effect::Rainbow);
                    *dirty = true;
                }
                if ui
                    .selectable_label(is_cust, RichText::new("★ Custom").color(CUST_HUE))
                    .clicked()
                    && !is_cust
                {
                    z.kind = customs
                        .first()
                        .map(|(n, _)| Kind::Custom(n.clone()))
                        .unwrap_or(Kind::Solid);
                    *dirty = true;
                }
            });

            ui.add_space(6.0);
            match &mut z.kind {
                Kind::Solid => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Color").weak());
                        if ui.color_edit_button_srgb(&mut z.color).changed() {
                            *dirty = true;
                        }
                    });
                }
                Kind::Program(e) => {
                    egui::ComboBox::from_id_source(("prog", zi))
                        .width(W)
                        .selected_text(e.label())
                        .show_ui(ui, |ui| {
                            for opt in Effect::ALL {
                                if ui.selectable_value(e, opt, opt.label()).clicked() {
                                    *dirty = true;
                                }
                            }
                        });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Color").weak());
                        if ui.color_edit_button_srgb(&mut z.color).changed() {
                            *dirty = true;
                        }
                    });
                }
                Kind::Custom(name) => {
                    egui::ComboBox::from_id_source(("cust", zi))
                        .width(W)
                        .selected_text(name.clone())
                        .show_ui(ui, |ui| {
                            for (n, _) in customs {
                                if ui.selectable_value(name, n.clone(), n).clicked() {
                                    *dirty = true;
                                }
                            }
                        });
                }
            }

            if !matches!(z.kind, Kind::Solid) {
                ui.add_space(4.0);
                ui.label(RichText::new("Speed").weak().small());
                if ui.add(egui::Slider::new(&mut z.speed, 1..=9)).changed() {
                    *dirty = true;
                }
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Brightness").weak().small());
            if ui
                .add(egui::Slider::new(&mut z.brightness, 0..=100).suffix("%"))
                .changed()
            {
                *dirty = true;
            }
        });
    });
    ui.add_space(8.0);
}

fn spawn_connect() -> mpsc::Receiver<ConnResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = (|| {
            let mut c = OpenRgb::connect().map_err(|e| e.to_string())?;
            let ctrls = c.controllers().map_err(|e| e.to_string())?;
            Ok((c, ctrls))
        })();
        let _ = tx.send(res);
    });
    rx
}

fn short_name(name: &str) -> String {
    name.trim_end_matches(" Device")
        .replace("AcerHID", "")
        .replace("CoverLogoLED", "Cover Logo")
        .replace("ModeKeyLED", "Mode Key")
        .trim()
        .to_string()
}

fn luminance(c: Rgb) -> f32 {
    (0.299 * c.0 as f32 + 0.587 * c.1 as f32 + 0.114 * c.2 as f32) / 255.0
}
