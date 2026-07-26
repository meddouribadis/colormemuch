//! The egui compositor screen.
//!
//! The UI never touches the socket — it owns *state* and hands a resolved
//! [`EngineState`] to the [`crate::engine`] thread, which renders and holds it.
//! That split is what lets a hidden window keep its colors alive.
//!
//! Three effect types, wired to their persistence tier:
//! * **Core / Hardware** — a firmware mode; whole-device, zero host CPU. Can be
//!   written to the keyboard's flash (**Firmware** tier) so it survives a reboot
//!   with no process running.
//! * **Program** — built-in `fn(t,n)` effects; per-zone, animated (**Daemon**
//!   tier — the engine holds them while colormemuch runs, incl. in the tray).
//! * **Custom** — user-built palette+motion effects, editable and persisted.

#![cfg(windows)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};

use colormemuch::effects::{
    render_plan, scale, shared_clock_groups, Effect, Fx, Group, Params, ZoneSource,
};
use colormemuch::engine::{ControllerInfo, DeviceMode as EngDeviceMode, EngineState, HwSpec};
use colormemuch::host::{self, Host, HostEvent};
use colormemuch::library::{ColorStop, CustomEffect, EffectLibrary, Motion};
use colormemuch::rgb::Rgb;

// Type accents (match the compositor mockup tokens).
const CORE_HUE: Color32 = Color32::from_rgb(0x4b, 0xbf, 0x73); // green: firmware, 0 CPU
const PROG_HUE: Color32 = Color32::from_rgb(0x1f, 0xb7, 0xa6); // teal
const CUST_HUE: Color32 = Color32::from_rgb(0xb0, 0x7c, 0xff); // purple
// Persistence-tier accents.
const TIER_DAEMON: Color32 = Color32::from_rgb(0x5d, 0xca, 0xa5);
const TIER_FIRMWARE: Color32 = Color32::from_rgb(0x0f, 0x9e, 0x6f);
const DANGER_HUE: Color32 = Color32::from_rgb(0xf0, 0x99, 0x7b);

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
    fn to_spec(&self) -> HwSpec {
        HwSpec {
            mode: self.mode.clone(),
            color: Rgb(self.color[0], self.color[1], self.color[2]),
            speed: self.speed,
            brightness: self.brightness,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
enum DeviceMode {
    PerZone,
    Hardware(HwEffect),
}

/// The persisted lighting setup — per-device mode + zones, keyed by controller
/// name (stable across restarts), plus master brightness, the Spread set, and
/// the two persistence-spine toggles.
#[derive(Default, Serialize, Deserialize)]
struct LightingSetup {
    master: u32,
    spread: Vec<String>,
    #[serde(default)]
    hold: bool,
    #[serde(default)]
    battery_saver: bool,
    devices: std::collections::HashMap<String, DeviceSetup>,
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
    replacing: Option<String>,
    draft: CustomEffect,
}

enum Conn {
    Connecting,
    Ready(Vec<ControllerInfo>),
    Failed(String),
}

pub struct RgbControl {
    host: Box<dyn Host>,
    /// Kept so the host can be re-created on a dropped service connection.
    waker: colormemuch::engine::Waker,
    conn: Conn,
    selected: usize,

    // Per-device UI state, parallel to the controller list when Ready.
    zones: Vec<Vec<ZoneUi>>,
    dev_mode: Vec<DeviceMode>,

    spread: HashSet<String>,
    master: u32,
    hold: bool,
    battery_saver: bool,
    on_battery: bool,

    clock: Instant,
    dirty: bool,
    library: EffectLibrary,
    editor: Option<EditorState>,

    save_status: Option<(Instant, String)>,
}

impl RgbControl {
    pub fn new(ctx: &egui::Context) -> Self {
        let setup = LightingSetup::load();
        let ctx2 = ctx.clone();
        let wake: colormemuch::engine::Waker = Arc::new(move || ctx2.request_repaint());
        Self {
            host: host::create(wake.clone()),
            waker: wake,
            conn: Conn::Connecting,
            selected: 0,
            zones: Vec::new(),
            dev_mode: Vec::new(),
            spread: setup.spread.iter().cloned().collect(),
            master: if setup.master == 0 { 100 } else { setup.master.clamp(1, 100) },
            hold: setup.hold,
            battery_saver: setup.battery_saver,
            on_battery: false,
            clock: Instant::now(),
            dirty: false,
            library: EffectLibrary::load(),
            editor: None,
            save_status: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll_host();
        let t = self.clock.elapsed().as_secs_f32();

        egui::SidePanel::left("devices")
            .exact_width(210.0)
            .show(ctx, |ui| self.side_panel(ui));

        egui::SidePanel::right("spine")
            .exact_width(226.0)
            .show(ctx, |ui| self.spine_panel(ui));

        egui::CentralPanel::default().show(ctx, |ui| self.editor_view(ui, t));

        self.effect_editor_window(ctx, t);

        // Ship a fresh snapshot to the engine only when something changed — the
        // engine's own hold timer handles re-asserting against Acer, so an idle
        // (or hidden) UI never needs to tick for lighting.
        if self.dirty {
            if let Conn::Ready(_) = &self.conn {
                let state = self.engine_state();
                self.host.set_state(&state);
            }
            self.dirty = false;
        }

        // Repaint only for the UI's own live previews / editor animation.
        if self.selected_animating() || self.editor.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(42));
        } else if matches!(self.conn, Conn::Connecting) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Build the resolved compositor snapshot for the engine.
    fn engine_state(&self) -> EngineState {
        let devices = (0..self.dev_mode.len())
            .map(|i| match &self.dev_mode[i] {
                DeviceMode::Hardware(hw) => EngDeviceMode::Hardware(hw.to_spec()),
                DeviceMode::PerZone => EngDeviceMode::PerZone(
                    self.zones[i].iter().map(|z| z.to_source(&self.library)).collect(),
                ),
            })
            .collect();
        EngineState {
            devices,
            master: self.master as f32 / 100.0,
            spread: self.spread.clone(),
            hold: self.hold,
            battery_saver: self.battery_saver,
        }
    }

    fn poll_host(&mut self) {
        for ev in self.host.poll() {
            match ev {
                HostEvent::Connected(controllers) => {
                    self.rebuild_devices(&controllers);
                    self.conn = Conn::Ready(controllers);
                    self.selected = self.selected.min(self.dev_mode.len().saturating_sub(1));
                    self.dirty = true;
                }
                HostEvent::Disconnected(e) => {
                    if self.host.via_service() {
                        // The service dropped — fail over seamlessly: re-create
                        // the host, which reconnects if the service came back,
                        // or embeds and takes the hardware directly. (An
                        // embedded host's own engine self-heals, so we only
                        // re-create for a service-backed one.)
                        self.host = host::create(self.waker.clone());
                        self.conn = Conn::Connecting;
                        self.dirty = true;
                    } else {
                        self.conn = Conn::Failed(e);
                    }
                }
                HostEvent::OnBattery(b) => self.on_battery = b,
                HostEvent::SaveResult(res) => {
                    let msg = match res {
                        Ok(true) => "Saved to keyboard flash — survives reboot.".to_string(),
                        Ok(false) => "This effect can't be saved to firmware.".to_string(),
                        Err(e) => format!("Save failed: {e}"),
                    };
                    self.save_status = Some((Instant::now(), msg));
                }
            }
        }
        if let Some((when, _)) = &self.save_status {
            if when.elapsed().as_secs() > 8 {
                self.save_status = None;
            }
        }
    }

    /// Size the per-device UI state to the controllers, restoring the saved
    /// setup wherever a device still matches.
    fn rebuild_devices(&mut self, controllers: &[ControllerInfo]) {
        let setup = LightingSetup::load();
        let mut zones: Vec<Vec<ZoneUi>> = controllers
            .iter()
            .map(|c| vec![ZoneUi::default(); c.led_count as usize])
            .collect();
        let mut dev_mode = vec![DeviceMode::PerZone; controllers.len()];
        for (i, c) in controllers.iter().enumerate() {
            if let Some(ds) = setup.devices.get(&c.name) {
                if ds.zones.len() == zones[i].len() {
                    zones[i] = ds.zones.clone();
                    dev_mode[i] = ds.mode.clone();
                }
            }
        }
        self.zones = zones;
        self.dev_mode = dev_mode;
    }

    /// Capture and persist the setup (eframe's periodic / on-exit save hook).
    pub fn save_setup(&self) {
        let Conn::Ready(controllers) = &self.conn else {
            return;
        };
        let devices = controllers
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    c.name.clone(),
                    DeviceSetup {
                        mode: self.dev_mode[i].clone(),
                        zones: self.zones[i].clone(),
                    },
                )
            })
            .collect();
        LightingSetup {
            master: self.master,
            spread: self.spread.iter().cloned().collect(),
            hold: self.hold,
            battery_saver: self.battery_saver,
            devices,
        }
        .save();
    }

    fn selected_animating(&self) -> bool {
        if let (Conn::Ready(_), Some(DeviceMode::PerZone)) =
            (&self.conn, self.dev_mode.get(self.selected))
        {
            return self.zones[self.selected]
                .iter()
                .any(|z| !matches!(z.kind, Kind::Solid));
        }
        false
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
                    // Re-create the host so a returned service is picked up (and
                    // a stalled embedded engine gets a fresh connection).
                    self.host = host::create(self.waker.clone());
                    self.conn = Conn::Connecting;
                    self.dirty = true;
                }
            }
            Conn::Ready(controllers) => {
                for (i, c) in controllers.iter().enumerate() {
                    let sw = self.zones[i].first().map(|z| z.color).unwrap_or([80, 80, 80]);
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

    // ---- right panel: the persistence spine --------------------------------

    fn spine_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("PERSISTENCE SPINE").weak().small());
        ui.add_space(6.0);

        // Ownership / connection line.
        let via_service = self.host.via_service();
        let (dot, text) = match &self.conn {
            Conn::Ready(_) if via_service => (CORE_HUE, "Owner: colormemuch · service"),
            Conn::Ready(_) => (CORE_HUE, "Owner: colormemuch"),
            Conn::Connecting => (Color32::GRAY, "connecting…"),
            Conn::Failed(_) => (Color32::from_rgb(0xff, 0x6b, 0x6b), "Acer (no connection)"),
        };
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, dot);
            ui.label(text);
        });
        if self.battery_saver && self.on_battery {
            ui.label(RichText::new("⚡ on battery — reactive layer active").color(DANGER_HUE).small());
        }

        ui.add_space(10.0);
        if ui.checkbox(&mut self.hold, "Keep my lighting").changed() {
            self.dirty = true;
        }
        let hold_help = if self.hold {
            "colormemuch re-asserts your lighting every few seconds so PredatorSense \
             can't repaint over it — and with the service running, it holds after \
             you close the window."
        } else {
            "Stop Acer from overwriting your colors: colormemuch re-asserts them, and \
             (with the service) keeps them after the window closes."
        };
        ui.label(RichText::new(hold_help).weak().small());
        if ui
            .checkbox(&mut self.battery_saver, "Battery saver (reactive)")
            .on_hover_text("On battery: dim and warm the composited frame.")
            .changed()
        {
            self.dirty = true;
        }

        ui.add_space(10.0);
        ui.separator();
        ui.label(RichText::new("WHERE LAYERS LIVE").weak().small());
        tier_row(ui, TIER_FIRMWARE, "Firmware", "survives reboot · 0 CPU");
        tier_row(ui, TIER_DAEMON, "Daemon", "survives close (tray)");
        tier_row(ui, Color32::from_rgb(0xef, 0x9f, 0x27), "Live", "while window open");

        // Save-to-firmware, contextual to a Hardware-mode selection.
        ui.add_space(10.0);
        ui.separator();
        let hw_selected = matches!(self.dev_mode.get(self.selected), Some(DeviceMode::Hardware(_)));
        ui.label(RichText::new("BASE IDENTITY").weak().small());
        if hw_selected {
            if ui
                .button(RichText::new("⬇ Save to keyboard").color(TIER_FIRMWARE))
                .on_hover_text("Write this firmware effect to the keyboard's flash.")
                .clicked()
            {
                self.request_save();
            }
        } else {
            ui.label(
                RichText::new("Pick a Hardware effect to save it to firmware.")
                    .weak()
                    .small(),
            );
        }
        if let Some((_, msg)) = &self.save_status {
            ui.label(RichText::new(msg.as_str()).small());
        }

        // Exclusive mode — deliberately inert (see docs/COMPOSITOR.md).
        ui.add_space(10.0);
        ui.separator();
        ui.add_enabled_ui(false, |ui| {
            let _ = ui.button(RichText::new("⚠ Exclusive mode").color(DANGER_HUE));
        });
        ui.label(
            RichText::new(
                "Advanced · coming soon. Acer's OpenRGB server is a child of its \
                 lighting service, so true takeover needs our own server.",
            )
            .weak()
            .small(),
        );

        egui::TopBottomPanel::bottom("spine_hint")
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                ui.separator();
                ui.label(
                    RichText::new("Closing the window keeps colors held in the tray.")
                        .weak()
                        .small(),
                );
            });
    }

    fn request_save(&mut self) {
        let Some(DeviceMode::Hardware(hw)) = self.dev_mode.get(self.selected) else {
            return;
        };
        let spec = hw.to_spec();
        let device = self.selected;
        self.host.save_firmware(device, spec);
        self.save_status = Some((Instant::now(), "Saving…".to_string()));
    }

    // ---- central editor ----------------------------------------------------

    fn editor_view(&mut self, ui: &mut egui::Ui, t: f32) {
        let sel = self.selected;
        let controllers = match &self.conn {
            Conn::Ready(c) => c,
            _ => {
                ui.centered_and_justified(|ui| {
                    ui.label("Connect to an OpenRGB server to edit lighting.");
                });
                return;
            }
        };
        if sel >= controllers.len() {
            return;
        }

        let dev_name = short_name(&controllers[sel].name);
        let mode_names: Vec<String> = controllers[sel]
            .modes
            .iter()
            .filter(|m| !m.eq_ignore_ascii_case("direct"))
            .cloned()
            .collect();

        // Header + persistence-tier chip + device-mode toggle.
        ui.add_space(4.0);
        let is_hardware = matches!(self.dev_mode[sel], DeviceMode::Hardware(_));
        ui.horizontal(|ui| {
            ui.heading(dev_name);
            if is_hardware {
                tier_chip(ui, TIER_FIRMWARE, "Firmware-capable");
            } else {
                tier_chip(ui, TIER_DAEMON, "Daemon");
            }
            ui.label(RichText::new(format!("· clock {t:6.2}s")).weak().monospace());
        });
        ui.horizontal(|ui| {
            if ui.selectable_label(!is_hardware, "  Per-Zone  ").clicked() && is_hardware {
                self.dev_mode[sel] = DeviceMode::PerZone;
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
                self.dev_mode[sel] = DeviceMode::Hardware(HwEffect {
                    mode,
                    color: [0x00, 0xE5, 0xFF],
                    speed: 5,
                    brightness: 100,
                });
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
        if let DeviceMode::Hardware(hw) = &mut self.dev_mode[sel] {
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
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Applied live now. Use “Save to keyboard” (right) to persist it to \
                     firmware so it survives a reboot with nothing running.",
                )
                .weak()
                .small(),
            );
        }
        self.dirty |= dirty;
    }

    fn per_zone_panels(&mut self, ui: &mut egui::Ui, sel: usize, t: f32) {
        let customs: Vec<(String, [u8; 3])> = self
            .library
            .effects
            .iter()
            .map(|e| {
                let d = e.dominant();
                (e.name.clone(), [d.0, d.1, d.2])
            })
            .collect();

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
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for zi in 0..n {
                    let preview = frame.get(zi).copied().unwrap_or(Rgb(0, 0, 0));
                    zone_panel(ui, zi, &mut zrow[zi], preview, &groups, &customs, &mut local_dirty);
                }
            });
        });
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
}

fn tier_chip(ui: &mut egui::Ui, hue: Color32, label: &str) {
    egui::Frame::none()
        .fill(hue.linear_multiply(0.18))
        .stroke(egui::Stroke::new(1.0_f32, hue))
        .rounding(10.0)
        .inner_margin(egui::Margin::symmetric(7.0, 1.0))
        .show(ui, |ui| {
            ui.label(RichText::new(label).color(hue).small());
        });
}

fn tier_row(ui: &mut egui::Ui, hue: Color32, name: &str, note: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.5, hue);
        ui.label(RichText::new(name).small());
        ui.label(RichText::new(note).weak().small());
    });
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
