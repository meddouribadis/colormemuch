//! The lighting engine — a worker thread that owns the OpenRGB connection and
//! *holds* the composited profile against `AcerLightingService`'s reassertions.
//!
//! Why a thread and not the UI loop: the service repaints its own profile on
//! resume / AC-battery / its periodic timer, and OpenRGB DIRECT mode is a live
//! stream that dies when the client disconnects. If we only pushed from the
//! egui loop, a hidden window (or a closed one) would stop pushing and Acer
//! would win. The engine runs independently of the GUI — the window can hide to
//! the tray and the profile keeps holding. It also decouples lighting cadence
//! from egui repaints, which is what keeps CPU proportional to what's moving.
//!
//! The UI never touches the socket. It sends a fully-resolved [`EngineState`]
//! snapshot whenever anything changes; the engine renders and pushes.

#![cfg(windows)]
#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::effects::{render_plan, scale, Effect, Fx, ZoneSource};
use crate::dt::{self, DtState};
use crate::library::Motion;
use crate::openrgb::{Controller, OpenRgb};
use crate::rgb::Rgb;
use crate::wmi::Wmi;

/// A UI-agnostic "something changed, wake up and drain events" nudge. The
/// embedded host wires this to `egui::Context::request_repaint`; the daemon
/// passes a no-op (it polls its own channel).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A firmware effect the controller animates host-free.
#[derive(Clone, Serialize, Deserialize)]
pub struct HwSpec {
    pub mode: String,
    pub color: Rgb,
    pub speed: u32,
    pub brightness: u32,
}

impl HwSpec {
    fn signature(&self) -> String {
        format!(
            "{}|{:?}|{}|{}",
            self.mode, self.color, self.speed, self.brightness
        )
    }
}

/// Per-device rendering plan — fully resolved, no library lookups engine-side.
#[derive(Clone, Serialize, Deserialize)]
pub enum DeviceMode {
    /// Independent per-zone sources, composited via [`render_plan`].
    PerZone(Vec<ZoneSource>),
    /// A firmware effect (the zero-CPU "Base identity" tier).
    Hardware(HwSpec),
}

/// The whole compositor state. The UI ships a fresh snapshot on every change.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct EngineState {
    /// Parallel to the controller list the engine reported on connect.
    pub devices: Vec<DeviceMode>,
    pub master: f32, // 0..=1
    pub spread: HashSet<String>,
    /// Re-push periodically to out-time Acer's reassert, even when unchanged.
    pub hold: bool,
    /// Reactive layer: on battery, dim + warm the composited frame.
    pub battery_saver: bool,
    /// Desktop-tower case (PO5-660) global static color. `None` = don't touch
    /// the case (laptop hardware, or the user never enabled it).
    #[serde(default)]
    pub dt: Option<DtState>,
}

pub enum EngineCmd {
    SetState(EngineState),
    /// Write a firmware mode to a device's flash. Replies `Ok(true)` if saved,
    /// `Ok(false)` if the mode isn't savable, `Err` on a transport failure.
    SaveFirmware {
        device: usize,
        spec: HwSpec,
        reply: Sender<Result<bool, String>>,
    },
    Reconnect,
}

pub enum EngineEvent {
    Connected(Vec<Controller>),
    Disconnected(String),
    /// True when the last loop saw AC unplugged (drives the spine's readout).
    OnBattery(bool),
    /// Desktop-tower (WMI) channel state. Sent once at startup (available iff
    /// this process is elevated on Acer DT hardware) and again whenever a DT
    /// write fails or recovers (error carries the last failure, None clears).
    DtStatus { available: bool, error: Option<String> },
}

pub struct EngineHandle {
    pub cmd: Sender<EngineCmd>,
    pub evt: Receiver<EngineEvent>,
}

impl EngineHandle {
    pub fn send(&self, cmd: EngineCmd) {
        let _ = self.cmd.send(cmd);
    }
}

const HOLD_PERIOD: Duration = Duration::from_secs(3);
/// Minimum gap between two desktop-tower (WMI) transactions — the color
/// picker streams while dragging and the firmware flickers under back-to-back
/// writes. Pending values coalesce: change detection re-fires until applied.
const DT_MIN_INTERVAL: Duration = Duration::from_millis(400);
const IDLE_TICK: Duration = Duration::from_millis(900);
const RECONNECT_WAIT: Duration = Duration::from_secs(2);
const MIN_HZ: f32 = 8.0;
const MAX_HZ: f32 = 30.0;

/// Spawn the engine thread. `wake` is invoked when connection state changes so
/// the host drains events promptly even while idle.
pub fn spawn(wake: Waker) -> EngineHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (evt_tx, evt_rx) = mpsc::channel();
    std::thread::spawn(move || run(wake, cmd_rx, evt_tx));
    EngineHandle {
        cmd: cmd_tx,
        evt: evt_rx,
    }
}

fn run(wake: Waker, cmd_rx: Receiver<EngineCmd>, evt_tx: Sender<EngineEvent>) {
    // Desktop-tower (WMI) channel: probe once up front so the UI knows whether
    // the case section applies. Fails gracefully unelevated / off-target.
    let wmi: Option<Wmi> = match Wmi::connect() {
        Ok(c) => {
            let _ = evt_tx.send(EngineEvent::DtStatus {
                available: true,
                error: None,
            });
            Some(c)
        }
        Err(e) => {
            let _ = evt_tx.send(EngineEvent::DtStatus {
                available: false,
                error: Some(e.to_string()),
            });
            None
        }
    };
    let mut last_dt: Option<DtState> = None;
    let mut last_dt_push = Instant::now() - DT_MIN_INTERVAL;
    let mut dt_error: Option<String> = None;
    (wake)();

    loop {
        // --- connect (retry until the UI channel closes) --------------------
        let (mut client, controllers) = match connect() {
            Ok(v) => v,
            Err(e) => {
                let _ = evt_tx.send(EngineEvent::Disconnected(e));
                (wake)();
                match cmd_rx.recv_timeout(RECONNECT_WAIT) {
                    Err(RecvTimeoutError::Disconnected) => return,
                    _ => continue,
                }
            }
        };
        let n = controllers.len();
        let _ = evt_tx.send(EngineEvent::Connected(controllers.clone()));
        (wake)();

        let mut state = EngineState::default();
        let mut direct_set = vec![false; n];
        let mut applied_hw: Vec<Option<String>> = vec![None; n];
        let mut last_frame: Vec<Vec<Rgb>> = vec![Vec::new(); n];
        let mut last_forced = Instant::now() - HOLD_PERIOD;
        let mut last_batt = false;
        let clock = Instant::now();

        // --- render / hold loop --------------------------------------------
        loop {
            let animating = is_animating(&state);
            let tick = if animating {
                Duration::from_secs_f32(1.0 / active_hz(&state))
            } else {
                IDLE_TICK
            };

            match cmd_rx.recv_timeout(tick) {
                Ok(EngineCmd::SetState(s)) => state = s,
                Ok(EngineCmd::SaveFirmware {
                    device,
                    spec,
                    reply,
                }) => {
                    let res = if device < n {
                        client
                            .save_mode(
                                &controllers[device],
                                &spec.mode,
                                spec.color,
                                Some(spec.speed),
                                Some(spec.brightness),
                            )
                            .map_err(|e| e.to_string())
                    } else {
                        Err("no such device".into())
                    };
                    // A save changes the active mode; invalidate that device's
                    // trackers so the next render re-establishes it.
                    if device < n {
                        applied_hw[device] = None;
                        direct_set[device] = false;
                        last_frame[device].clear();
                    }
                    if res.is_err() {
                        let _ = evt_tx.send(EngineEvent::Disconnected("save failed".into()));
                        let _ = reply.send(res);
                        break;
                    }
                    let _ = reply.send(res);
                    continue;
                }
                Ok(EngineCmd::Reconnect) => break,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return, // UI gone → quit
            }

            let on_batt = state.battery_saver && on_battery();
            if on_batt != last_batt {
                let _ = evt_tx.send(EngineEvent::OnBattery(on_batt));
                last_batt = on_batt;
                (wake)();
            }

            let force = state.hold && last_forced.elapsed() >= HOLD_PERIOD;
            let t = clock.elapsed().as_secs_f32();
            if let Err(e) = push_all(
                &mut client,
                &controllers,
                &state,
                force,
                on_batt,
                t,
                &mut direct_set,
                &mut applied_hw,
                &mut last_frame,
            ) {
                let _ = evt_tx.send(EngineEvent::Disconnected(e.to_string()));
                (wake)();
                break;
            }
            push_dt(
                &wmi,
                &state,
                force,
                &mut last_dt,
                &mut last_dt_push,
                &mut dt_error,
                &evt_tx,
                &wake,
            );
            if force {
                last_forced = Instant::now();
            }
        }
    }
}

/// Apply the desktop-tower case state (PO5-660, WMI) if the UI asked for it.
///
/// `wmi` is `None` unelevated / off-target — then a requested DT state
/// surfaces one sticky error instead of failing silently. Writes go through
/// the captured static-global transaction only (see [`dt`]).
///
/// Rate-limited: the egui color picker streams dozens of values per second
/// while dragging, and the firmware visibly chokes on back-to-back
/// transactions. At most one push per [`DT_MIN_INTERVAL`]; the pending value
/// lands on a later tick (change detection keeps it, so nothing is lost).
#[allow(clippy::too_many_arguments)]
fn push_dt(
    wmi: &Option<Wmi>,
    state: &EngineState,
    force: bool,
    last_dt: &mut Option<DtState>,
    last_push: &mut Instant,
    dt_error: &mut Option<String>,
    evt_tx: &Sender<EngineEvent>,
    wake: &Waker,
) {
    let Some(want) = &state.dt else {
        return;
    };
    if !force && last_dt.as_ref() == Some(want) {
        return;
    }
    if !force && last_push.elapsed() < DT_MIN_INTERVAL {
        return;
    }
    let Some(w) = wmi else {
        set_dt_error(dt_error, evt_tx, wake, Some("case unavailable: not elevated".into()));
        return;
    };
    let res = if want.on {
        dt::apply_static_global(w, want.color)
    } else {
        // OFF mirrors PredatorSense: same behavior template, flags flipped.
        // The template targets static-global; reuse it, then cut the output.
        w.call_bytes("SetGamingLedBehavior", &dt::BEHAVIOR_STATIC_GLOBAL)
            .and_then(|_| {
                std::thread::sleep(std::time::Duration::from_millis(60));
                w.call_packed(
                    "SetGamingRgbSetting",
                    dt::pack_setting(dt::area::ALL, want.color, dt::flags::OFF),
                )
            })
    };
    match res {
        Ok(_) => {
            *last_dt = Some(want.clone());
            *last_push = Instant::now();
            set_dt_error(dt_error, evt_tx, wake, None);
        }
        Err(e) => set_dt_error(dt_error, evt_tx, wake, Some(e.to_string())),
    }
}

/// Report a DT status change only when the error text actually changes, so a
/// wedged firmware doesn't spam the UI every tick.
fn set_dt_error(
    dt_error: &mut Option<String>,
    evt_tx: &Sender<EngineEvent>,
    wake: &Waker,
    next: Option<String>,
) {
    if *dt_error != next {
        *dt_error = next.clone();
        let _ = evt_tx.send(EngineEvent::DtStatus {
            available: true,
            error: next,
        });
        (wake)();
    }
}

#[allow(clippy::too_many_arguments)]
fn push_all(
    client: &mut OpenRgb,
    controllers: &[Controller],
    state: &EngineState,
    force: bool,
    on_batt: bool,
    t: f32,
    direct_set: &mut [bool],
    applied_hw: &mut [Option<String>],
    last_frame: &mut [Vec<Rgb>],
) -> std::io::Result<()> {
    for i in 0..controllers.len() {
        let Some(mode) = state.devices.get(i) else {
            continue;
        };
        match mode {
            DeviceMode::Hardware(spec) => {
                let sig = spec.signature();
                if force || applied_hw[i].as_deref() != Some(sig.as_str()) {
                    client.apply_effect(
                        &controllers[i],
                        &spec.mode,
                        spec.color,
                        Some(spec.speed),
                        Some(spec.brightness),
                    )?;
                    applied_hw[i] = Some(sig);
                    direct_set[i] = false;
                    last_frame[i].clear();
                }
            }
            DeviceMode::PerZone(sources) => {
                applied_hw[i] = None;
                let mut frame = render_plan(sources, &state.spread, t);
                for c in &mut frame {
                    *c = scale(*c, state.master);
                }
                if on_batt {
                    for c in &mut frame {
                        *c = battery_transform(*c);
                    }
                }
                if !force && frame == last_frame[i] {
                    continue;
                }
                if !direct_set[i] && client.enter_direct(&controllers[i]).is_ok() {
                    direct_set[i] = true;
                }
                client.update_leds(&controllers[i], &frame)?;
                last_frame[i] = frame;
            }
        }
    }
    Ok(())
}

/// The Reactive rule: warm the color (kill most of the blue, ease the green)
/// and dim it, so the keyboard reads "on battery" at a glance.
fn battery_transform(c: Rgb) -> Rgb {
    let warm = Rgb(c.0, (c.1 as f32 * 0.85) as u8, (c.2 as f32 * 0.45) as u8);
    scale(warm, 0.55)
}

fn is_animating(state: &EngineState) -> bool {
    state.devices.iter().any(|d| match d {
        DeviceMode::Hardware(_) => false,
        DeviceMode::PerZone(sources) => sources.iter().any(|s| s.fx().is_some()),
    })
}

fn active_hz(state: &EngineState) -> f32 {
    let mut hz = MIN_HZ;
    for d in &state.devices {
        if let DeviceMode::PerZone(sources) = d {
            for s in sources {
                if let Some(fx) = s.fx() {
                    hz = hz.max(fx_hz(fx));
                }
            }
        }
    }
    hz.clamp(MIN_HZ, MAX_HZ)
}

/// Per-effect frame rate — a breathing pulse wants ~10 Hz, a comet ~24.
fn fx_hz(fx: &Fx) -> f32 {
    match fx {
        Fx::Program(e) => match e {
            Effect::Comet | Effect::Fire | Effect::Police | Effect::Wave => 24.0,
            Effect::Rainbow | Effect::Gradient => 16.0,
            Effect::Breathe => 10.0,
        },
        Fx::Custom(c) => match c.motion {
            Motion::Twinkle => 22.0,
            Motion::Scroll | Motion::Bounce => 18.0,
            Motion::Pulse => 10.0,
            Motion::Static => 0.0,
        },
    }
}

fn connect() -> Result<(OpenRgb, Vec<Controller>), String> {
    let mut c = OpenRgb::connect().map_err(|e| e.to_string())?;
    let ctrls = c.controllers().map_err(|e| e.to_string())?;
    Ok((c, ctrls))
}

/// AC line status via the Win32 power API. `ACLineStatus == 0` means offline
/// (running on battery); 1 is plugged in, 255 unknown.
fn on_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    unsafe {
        let mut s = SYSTEM_POWER_STATUS::default();
        GetSystemPowerStatus(&mut s).is_ok() && s.ACLineStatus == 0
    }
}
