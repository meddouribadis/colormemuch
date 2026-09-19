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
//!
//! Module map: [`theme`] (tokens + egui style), [`widgets`] (the painted
//! control kit), and one file per view — [`sidebar`], [`toolbar`], [`case`]
//! (PO5-660 tower), [`zones`] (per-zone / hardware editor), [`library`] (the
//! custom-effect editor + device inspector).

#![cfg(windows)]

pub mod theme;
pub mod widgets;

mod case;
mod library;
mod sidebar;
mod toolbar;
mod zones;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};

use colormemuch::dt::{AreaCmd, DtEffect, DtState};
use colormemuch::effects::{Effect, Fx, Params, ZoneSource};
use colormemuch::engine::{DeviceMode as EngDeviceMode, EngineState, HwSpec};
use colormemuch::host::{self, Host, HostEvent};
use colormemuch::library::{CustomEffect, EffectLibrary};
use colormemuch::model::DeviceDescriptor;
use colormemuch::rgb::Rgb;

use theme::SP_XL;

/// Height of the top toolbar (drawn by `app.rs`).
pub fn toolbar_height() -> f32 {
    toolbar::HEIGHT
}

// Effect-type accents (shared by the zone cards and the sidebar).
pub(crate) const PROG_HUE: Color32 = Color32::from_rgb(0x1f, 0xb7, 0xa6); // teal
pub(crate) const CUST_HUE: Color32 = Color32::from_rgb(0xb0, 0x7c, 0xff); // purple

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
    /// Desktop-tower case (PO5-660) global static color. `None` = the case
    /// section was never enabled — the engine leaves the hardware alone.
    #[serde(default)]
    dt: Option<DtSetup>,
}

/// Persisted form of the case section (plain array color for serde comfort).
#[derive(Clone, Serialize, Deserialize)]
struct DtSetup {
    color: [u8; 3],
    on: bool,
    #[serde(default)]
    effect: DtEffect,
    /// Per-area overrides (TOP/FRONT/REAR/AUX). FRONT is capture-backed;
    /// the rest are experimental until their own Frida captures land.
    #[serde(default)]
    areas: Vec<AreaCmd>,
}

impl DtSetup {
    fn to_state(&self) -> DtState {
        DtState {
            color: Rgb(self.color[0], self.color[1], self.color[2]),
            on: self.on,
            effect: self.effect,
            areas: self.areas.clone(),
        }
    }
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
    Ready(Vec<DeviceDescriptor>),
    Failed(String),
}

/// Severity of a transient notice (drives the toast accent).
#[derive(Clone, Copy)]
enum Notice {
    Info,
    Success,
    Error,
}

pub struct RgbControl {
    host: Box<dyn Host>,
    /// Kept so the host can be re-created on a dropped service connection.
    waker: colormemuch::engine::Waker,
    conn: Conn,
    /// Index into the controller list, or `controllers.len()` for the
    /// virtual "Tower case" entry (see [`Self::case_selected`]).
    selected: usize,

    // Per-device UI state, parallel to the controller list when Ready.
    zones: Vec<Vec<ZoneUi>>,
    dev_mode: Vec<DeviceMode>,

    spread: HashSet<String>,
    master: u32,
    hold: bool,
    battery_saver: bool,
    on_battery: bool,

    /// Desktop-tower case section: channel state from the host, user setup.
    dt_available: bool,
    dt_error: Option<String>,
    dt: Option<DtSetup>,

    clock: Instant,
    dirty: bool,
    library: EffectLibrary,
    editor: Option<EditorState>,
    inspector_open: bool,

    notice: Option<(Instant, Notice, String)>,
}

const NOTICE_SECS: f32 = 6.0;

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
            dt_available: false,
            dt_error: None,
            dt: setup.dt,
            clock: Instant::now(),
            dirty: false,
            library: EffectLibrary::load(),
            editor: None,
            inspector_open: false,
            notice: None,
        }
    }

    /// Everything below the toolbar: sidebar, content, floating windows.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll_host();
        let t = self.clock.elapsed().as_secs_f32();

        // If the case entry disappeared from under a case selection (the WMI
        // channel went away with real controllers present), fall back to the
        // last controller instead of a blank editor.
        let n = self.controllers().len();
        if !self.case_offered() && n > 0 && self.selected >= n {
            self.selected = n - 1;
        }

        let tk = theme::tokens_for(ctx.style().visuals.dark_mode);

        egui::SidePanel::left("sidebar")
            .exact_width(236.0)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(tk.side)
                    .inner_margin(egui::Margin::symmetric(theme::SP_M, theme::SP_M)),
            )
            .show(ctx, |ui| self.sidebar(ui, t));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(tk.bg)
                    .inner_margin(egui::Margin::symmetric(SP_XL + 4.0, SP_XL)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.case_selected() {
                            self.case_view(ui);
                        } else {
                            self.zones_view(ui, t);
                        }
                    });
            });

        self.effect_editor_window(ctx, t);
        self.devices_inspector(ctx);
        self.show_notice(ctx);

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

    // ---- selection helpers ---------------------------------------------------

    fn controllers(&self) -> &[DeviceDescriptor] {
        match &self.conn {
            Conn::Ready(c) => c,
            _ => &[],
        }
    }

    /// Whether the tower-case entry is offered in the sidebar: the host
    /// reports a WMI channel, or there's nothing else to show (PO5-660: every
    /// OpenRGB controller is filtered — the tower is WMI-driven).
    fn case_offered(&self) -> bool {
        matches!(&self.conn, Conn::Ready(cs) if self.dt_available || cs.is_empty())
    }

    fn case_index(&self) -> usize {
        self.controllers().len()
    }

    fn case_selected(&self) -> bool {
        self.case_offered() && self.selected >= self.case_index()
    }

    fn notify(&mut self, kind: Notice, msg: impl Into<String>) {
        self.notice = Some((Instant::now(), kind, msg.into()));
    }

    fn show_notice(&mut self, ctx: &egui::Context) {
        let Some((when, kind, msg)) = &self.notice else { return };
        let age = when.elapsed().as_secs_f32();
        if age > NOTICE_SECS {
            self.notice = None;
            return;
        }
        let tk = theme::tokens_for(ctx.style().visuals.dark_mode);
        let hue = match kind {
            Notice::Info => tk.accent,
            Notice::Success => tk.success,
            Notice::Error => tk.danger,
        };
        widgets::toast(ctx, hue, msg, age, NOTICE_SECS);
    }

    // ---- engine plumbing (unchanged behavior) -----------------------------

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
            dt: self.dt.as_ref().map(DtSetup::to_state),
        }
    }

    fn poll_host(&mut self) {
        for ev in self.host.poll() {
            match ev {
                HostEvent::Connected(controllers) => {
                    self.rebuild_devices(&controllers);
                    let n = controllers.len();
                    self.conn = Conn::Ready(controllers);
                    // Keep a case selection if the case is offered; otherwise
                    // clamp into the controller list.
                    let max = if self.case_offered() { n } else { n.saturating_sub(1) };
                    self.selected = self.selected.min(max);
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
                HostEvent::DtStatus { available, error } => {
                    self.dt_available = available;
                    self.dt_error = error;
                }
                HostEvent::SaveResult(res) => match res {
                    Ok(true) => self.notify(Notice::Success, "Saved to device — survives reboot."),
                    Ok(false) => self.notify(Notice::Info, "This effect can't be saved to firmware."),
                    Err(e) => self.notify(Notice::Error, format!("Save failed: {e}")),
                },
            }
        }
    }

    /// Re-create the host so a returned service is picked up (and a stalled
    /// embedded engine gets a fresh connection).
    fn reconnect(&mut self) {
        self.host = host::create(self.waker.clone());
        self.conn = Conn::Connecting;
        self.dirty = true;
    }

    /// Size the per-device UI state to the controllers, restoring the saved
    /// setup wherever a device still matches.
    fn rebuild_devices(&mut self, controllers: &[DeviceDescriptor]) {
        let setup = LightingSetup::load();
        let mut zones: Vec<Vec<ZoneUi>> = controllers
            .iter()
            .map(|c| vec![ZoneUi::default(); c.led_count as usize])
            .collect();
        let mut dev_mode = vec![DeviceMode::PerZone; controllers.len()];
        for (i, c) in controllers.iter().enumerate() {
            // Prefer the topology signature; fall back to the old name key for
            // one-time migration of pre-P1 profiles.
            let ds = setup
                .devices
                .get(&c.topology_sig())
                .or_else(|| setup.devices.get(&c.name));
            if let Some(ds) = ds {
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
                    c.topology_sig(),
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
            dt: self.dt.clone(),
        }
        .save();
    }

    fn selected_animating(&self) -> bool {
        if self.case_selected() {
            return false;
        }
        if let (Conn::Ready(_), Some(DeviceMode::PerZone)) =
            (&self.conn, self.dev_mode.get(self.selected))
        {
            return self.zones[self.selected]
                .iter()
                .any(|z| !matches!(z.kind, Kind::Solid));
        }
        false
    }

    fn request_save(&mut self) {
        let Some(DeviceMode::Hardware(hw)) = self.dev_mode.get(self.selected) else {
            return;
        };
        let spec = hw.to_spec();
        let device = self.selected;
        self.host.save_firmware(device, spec);
        self.notify(Notice::Info, "Saving to device…");
    }

    fn open_new_effect(&mut self) {
        self.editor = Some(EditorState {
            replacing: None,
            draft: CustomEffect::new_default(self.library.fresh_name()),
        });
    }

    fn open_edit_effect(&mut self, name: &str) {
        if let Some(e) = self.library.get(name) {
            self.editor = Some(EditorState {
                replacing: Some(name.to_string()),
                draft: e.clone(),
            });
        }
    }
}

/// Generic display cleanup — trim the common " Device" suffix OpenRGB appends.
/// Acer desktop towers expose `AcerDTGlobal`, `AcerDTArea1..5`, `AcerDTDIMM`:
/// give them short human names. No other vendor-specific surgery; the
/// descriptor's name stays the source of truth for identity.
fn short_name(name: &str) -> String {
    let base = name.trim_end_matches(" Device").trim();
    if let Some(n) = base.strip_prefix("AcerDTArea") {
        return format!("Area {n}");
    }
    match base {
        "AcerDTGlobal" => "Global (all areas)".to_string(),
        "AcerDTDIMM" => "DIMM / RAM".to_string(),
        "AcerHIDKeyboard" => "Keyboard".to_string(),
        "AcerHIDCoverLogoLED" => "Lid logo".to_string(),
        "AcerHIDModeKeyLED" => "Mode key".to_string(),
        _ => base.to_string(),
    }
}
