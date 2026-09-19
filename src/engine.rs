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
/// Desktop-tower (WMI) debounce: a change is written only once the requested
/// state has stopped moving for this long. The color picker streams dozens of
/// values per second while dragging, and every transaction blanks the zone
/// for the firmware's mode-apply — so a drag must collapse into one write.
const DT_SETTLE: Duration = Duration::from_millis(220);
/// After a failed DT write, wait this long before replaying the whole state.
const DT_RETRY: Duration = Duration::from_secs(2);
/// With Hold on, how often the case registers are read back and compared to
/// what we applied. Reads are free of side effects; only a diverged zone is
/// rewritten — so an untouched case never blinks.
const DT_VERIFY_PERIOD: Duration = Duration::from_secs(4);
/// A zone whose echo still disagrees after this many consecutive rewrites is
/// judged unreadable (the getter isn't reflecting that selector) and dropped
/// from re-assert rather than rewritten forever.
const DT_VERIFY_STRIKES: u8 = 2;
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
    let mut dt_track = DtTracker::default();
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
            let mut tick = if animating {
                Duration::from_secs_f32(1.0 / active_hz(&state))
            } else {
                IDLE_TICK
            };
            // A debounced case write is waiting: wake right as it settles.
            if let Some(d) = dt_track.wake_in() {
                tick = tick.min(d);
            }

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
            // The case is firmware state — it keeps itself, so the hold
            // re-push (`force`) deliberately does not apply to it: a replay
            // would blank the LEDs every HOLD_PERIOD for nothing. Hold instead
            // *verifies* the case by read-back and rewrites only what diverged.
            dt_track.verify(&wmi, state.hold, &mut dt_error, &evt_tx, &wake);
            dt_track.push(&wmi, &state, &mut dt_error, &evt_tx, &wake);
            if force {
                last_forced = Instant::now();
            }
        }
    }
}

/// One resolved case write: what a zone should show after master scaling.
/// The unit of change detection — two equal `ZoneWrite`s never hit the wire.
#[derive(Clone, PartialEq)]
struct ZoneWrite {
    area: u16,
    color: Rgb,
    on: bool,
    effect: dt::DtEffect,
}

/// Flatten the requested case state into the writes it implies: the global
/// broadcast first, then each per-area override.
fn resolve_dt(want: &DtState, master: f32) -> Vec<ZoneWrite> {
    let mut out = vec![ZoneWrite {
        area: dt::area::ALL,
        color: scale(want.color, master),
        on: want.on,
        effect: want.effect,
    }];
    out.extend(want.areas.iter().map(|a| ZoneWrite {
        area: a.area,
        color: scale(a.color, master),
        on: a.on,
        effect: a.effect,
    }));
    out
}

/// Desktop-tower (PO5-660, WMI) write scheduler.
///
/// Every WMI transaction blanks its zone while the firmware re-applies the
/// mode, and a broadcast (`area::ALL`) repaints *every* area — so the naive
/// "replay everything on any change" turned one color pick into a second of
/// dark case. This tracker does three things instead:
///
/// 1. **Debounce** — a change is written only after [`DT_SETTLE`] with no
///    further change, so a picker drag lands as one transaction.
/// 2. **Diff** — only zones whose resolved write differs from what was last
///    applied are sent. An area edit touches that area alone. A global edit
///    sends the broadcast and then re-applies every override the broadcast
///    just repainted. A removed override is re-covered by writing the global
///    values to that one area (same end state as a broadcast, no blink).
/// 3. **Verify, don't replay** — with Hold on, the case registers are read
///    back every [`DT_VERIFY_PERIOD`] and only zones whose echo diverges from
///    what we applied are rewritten (PredatorSense repainted). Because it's
///    unconfirmed whether `GetGamingRgbSetting` echoes per selector, a zone
///    that still disagrees after [`DT_VERIFY_STRIKES`] rewrites is marked
///    unreadable and left alone — a wrong assumption can cost at most two
///    blinks, never a blink loop.
///
/// `wmi` is `None` unelevated / off-target — then a requested DT state
/// surfaces one sticky error instead of failing silently. Writes go through
/// the captured static transactions only (see [`dt`]).
#[derive(Default)]
struct DtTracker {
    /// What the hardware currently shows, as far as we've written it.
    applied: Option<Vec<ZoneWrite>>,
    /// The most recent request and when it last changed (debounce anchor).
    seen: Option<Vec<ZoneWrite>>,
    changed_at: Option<Instant>,
    /// After a failure, don't retry before this.
    retry_at: Option<Instant>,

    // ---- read-back re-assert (Hold) ----
    last_verify: Option<Instant>,
    /// Consecutive verify passes in which this area's echo disagreed right
    /// after we rewrote it. Reset on agreement or on any user change.
    strikes: Vec<(u16, u8)>,
    /// Areas whose echo never reflects our writes — excluded from verify.
    unreadable: Vec<u16>,
    /// The getter itself failed: verification is off for this session.
    verify_broken: bool,
    /// The pending write was requested by [`Self::verify`], not the user —
    /// its success must not clear the strikes it is counting.
    verify_rewrite: bool,
}

impl DtTracker {
    fn verify_note(&self) -> Option<String> {
        verify_note(self.verify_broken, &self.unreadable)
    }

    /// Hold's re-assert for the case. Reads each applied zone's register and,
    /// where the hardware disagrees with `applied`, records what the hardware
    /// shows — the very next [`Self::push`] then plans exactly the writes
    /// needed to bring it back (global diverged → broadcast + overrides;
    /// one area diverged → that area). Nothing diverged → nothing written.
    fn verify(
        &mut self,
        wmi: &Option<Wmi>,
        hold: bool,
        dt_error: &mut Option<String>,
        evt_tx: &Sender<EngineEvent>,
        wake: &Waker,
    ) {
        let Some(w) = wmi else { return };
        if !hold || self.verify_broken || self.seen != self.applied {
            return;
        }
        if self.retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        if self.last_verify.is_some_and(|t| t.elapsed() < DT_VERIFY_PERIOD) {
            return;
        }
        let Some(applied) = &mut self.applied else { return };
        self.last_verify = Some(Instant::now());

        for z in applied.iter_mut() {
            if self.unreadable.contains(&z.area) {
                continue;
            }
            let (color, flags) = match dt::get_color(w, z.area) {
                Ok(v) => v,
                Err(e) => {
                    // Can't read — don't guess, don't hammer. Hold stays
                    // honest for OpenRGB devices; the case just isn't verified.
                    self.verify_broken = true;
                    set_dt_error(
                        dt_error,
                        evt_tx,
                        wake,
                        Some(format!("case read-back unavailable ({e}) — hold won't re-assert the case")),
                    );
                    return;
                }
            };
            let on = flags == dt::flags::ON;
            let expected_flags = if z.on { dt::flags::ON } else { dt::flags::OFF };
            if color == z.color && flags == expected_flags {
                // Agreement: clear any strike against this area.
                self.strikes.retain(|(a, _)| *a != z.area);
                continue;
            }
            // Diverged. Count it; give up on the selector if a rewrite didn't
            // change what it echoes.
            let n = match self.strikes.iter_mut().find(|(a, _)| *a == z.area) {
                Some((_, n)) => {
                    *n += 1;
                    *n
                }
                None => {
                    self.strikes.push((z.area, 1));
                    1
                }
            };
            if n > DT_VERIFY_STRIKES {
                self.unreadable.push(z.area);
                self.strikes.retain(|(a, _)| *a != z.area);
                // Leave `applied` at the expected value: we stop judging this
                // selector, we don't rewrite it. (Free fn: `applied` is
                // still mutably borrowed here.)
                let note = verify_note(self.verify_broken, &self.unreadable);
                set_dt_error(dt_error, evt_tx, wake, note);
                continue;
            }
            // Record what the hardware shows; `push` diffs against it.
            z.color = color;
            z.on = on;
            self.verify_rewrite = true;
        }
    }
    /// How long the loop may sleep before a pending write needs flushing:
    /// `None` when nothing is waiting (or a retry backoff is running), else
    /// the remaining settle time, so the write lands right as the request
    /// stops moving instead of up to a full extra `DT_SETTLE` later.
    fn wake_in(&self) -> Option<Duration> {
        let waiting = self.seen.is_some() && self.seen != self.applied;
        if !waiting || self.retry_at.is_some_and(|t| Instant::now() < t) {
            return None;
        }
        let elapsed = self.changed_at.map(|t| t.elapsed()).unwrap_or(DT_SETTLE);
        Some(DT_SETTLE.saturating_sub(elapsed).max(Duration::from_millis(5)))
    }

    fn push(
        &mut self,
        wmi: &Option<Wmi>,
        state: &EngineState,
        dt_error: &mut Option<String>,
        evt_tx: &Sender<EngineEvent>,
        wake: &Waker,
    ) {
        let Some(want) = &state.dt else {
            // Case lighting disabled: leave the hardware alone, but forget
            // what we applied so a re-enable replays in full (PredatorSense
            // may have repainted meanwhile).
            self.applied = None;
            self.seen = None;
            self.changed_at = None;
            self.retry_at = None;
            self.last_verify = None;
            self.strikes.clear();
            // Off/on is a fresh start: give every selector another chance.
            self.unreadable.clear();
            return;
        };
        let resolved = resolve_dt(want, state.master);

        // Debounce: restart the settle timer whenever the request moves.
        if self.seen.as_ref() != Some(&resolved) {
            self.seen = Some(resolved);
            self.changed_at = Some(Instant::now());
            // The user moved the target while a verify rewrite was still
            // waiting (only possible during a retry backoff): the coming
            // write is theirs now.
            self.verify_rewrite = false;
            return;
        }
        if self.applied.as_ref() == Some(&resolved) {
            return;
        }
        if self.changed_at.is_some_and(|t| t.elapsed() < DT_SETTLE) {
            return;
        }
        if self.retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }

        let Some(w) = wmi else {
            set_dt_error(dt_error, evt_tx, wake, Some("case unavailable: not elevated".into()));
            // Nothing can happen until the request changes — don't keep the
            // loop awake for it.
            self.applied = self.seen.clone();
            return;
        };

        // The writes this change implies, shortest list the diff allows.
        let plan: Vec<ZoneWrite> = match &self.applied {
            None => resolved.clone(),
            Some(prev) if resolved[0] != prev[0] => {
                // The broadcast repaints every area — re-apply overrides.
                resolved.clone()
            }
            Some(prev) => {
                // Changed or new overrides only…
                let mut plan: Vec<ZoneWrite> = resolved[1..]
                    .iter()
                    .filter(|z| !prev[1..].contains(z))
                    .cloned()
                    .collect();
                // …plus areas whose override was removed: paint them with the
                // global values directly instead of broadcasting. (This sends
                // a per-area transaction to a selector the user already
                // exercised while the override was on.)
                let global = &resolved[0];
                plan.extend(
                    prev[1..]
                        .iter()
                        .filter(|p| !resolved[1..].iter().any(|z| z.area == p.area))
                        .map(|p| ZoneWrite { area: p.area, ..global.clone() }),
                );
                plan
            }
        };

        match write_zones(w, &plan) {
            Ok(()) => {
                self.applied = Some(resolved);
                self.retry_at = None;
                // A fresh write: give verify a full period before judging it.
                // A user change wipes the strike history; a verify-triggered
                // rewrite keeps it (that's what it's counting).
                self.last_verify = Some(Instant::now());
                if self.verify_rewrite {
                    self.verify_rewrite = false;
                } else {
                    self.strikes.clear();
                }
                // Clears a transient write error, but keeps any standing
                // verify note (unreadable selectors) visible.
                let note = self.verify_note();
                set_dt_error(dt_error, evt_tx, wake, note);
            }
            Err((done, e)) => {
                // Keep what did land so the retry only re-sends the failed
                // tail instead of blinking every zone again.
                let landed = &plan[..done];
                if done == 0 {
                    // Nothing changed on the hardware; `applied` stands.
                } else if landed[0].area == dt::area::ALL {
                    // A broadcast landed: it repainted every area, so only the
                    // overrides written after it are still on the hardware.
                    self.applied = Some(landed.to_vec());
                } else if let Some(p) = &mut self.applied {
                    // Area-only plan (`applied` is always Some here). A landed
                    // write for an area that still has an override is patched
                    // in; a landed *re-cover* (override removed) means the area
                    // now follows the global — drop it, or the retry would
                    // treat it as uncovered and send it again.
                    for z in landed {
                        let still_overridden = resolved[1..].iter().any(|r| r.area == z.area);
                        if still_overridden {
                            match p.iter_mut().find(|q| q.area == z.area) {
                                Some(slot) => *slot = z.clone(),
                                None => p.push(z.clone()),
                            }
                        } else {
                            p.retain(|q| q.area != z.area);
                        }
                    }
                }
                self.retry_at = Some(Instant::now() + DT_RETRY);
                set_dt_error(dt_error, evt_tx, wake, Some(e));
            }
        }
    }
}

/// Standing status about what verify can't cover — re-asserted after every
/// successful write so it isn't wiped along with a transient error.
fn verify_note(broken: bool, unreadable: &[u16]) -> Option<String> {
    if broken {
        return Some("case read-back unavailable — hold won't re-assert the case".into());
    }
    if unreadable.is_empty() {
        return None;
    }
    let names: Vec<&str> = unreadable.iter().map(|a| area_name(*a)).collect();
    Some(format!(
        "{} doesn't echo our writes — hold isn't re-asserting it",
        names.join(", ")
    ))
}

/// Human name for a case selector, matching the cards in the UI.
fn area_name(area: u16) -> &'static str {
    match area {
        dt::area::ALL => "the whole case",
        dt::area::FRONT => "the front",
        dt::area::TOP => "the top",
        dt::area::REAR => "the rear",
        dt::area::AUX => "the aux area",
        _ => "an area",
    }
}

/// Send each write in order; stop at the first failure, reporting how many
/// landed. Each is a full captured-shape transaction (~2 WMI round-trips +
/// 60 ms), so callers keep the list as short as the diff allows.
fn write_zones(w: &Wmi, zones: &[ZoneWrite]) -> Result<(), (usize, String)> {
    for (i, z) in zones.iter().enumerate() {
        apply_dt_zone(w, z.area, &z.color, z.on, z.effect).map_err(|e| (i, e))?;
    }
    Ok(())
}

/// One DT zone write with its effect gate. Non-static effects have no capture
/// behind them, so they report an honest error instead of firing guessed
/// firmware bytes. Gate rejections and transport failures both come back as
/// `Err(String)`; [`DtTracker`] surfaces them (deduped) and owns the retry.
fn apply_dt_zone(
    w: &Wmi,
    area: u16,
    color: &Rgb,
    on: bool,
    effect: dt::DtEffect,
) -> Result<(), String> {
    if !effect.is_supported() {
        return Err(format!(
            "effect '{}' has no Frida capture yet — static only",
            effect.label()
        ));
    }
    dt::apply_static(w, area, *color, on)
        .map(|_| ())
        .map_err(|e| e.to_string())
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
    // Desktop towers (PO5-660) expose AcerDTGlobal / AcerDTArea1..5 /
    // AcerDTDIMM on the OpenRGB server, but the server is a showroom there:
    // every write is accepted and nothing reaches the LEDs (proven on
    // hardware — PredatorSense drives them over WMI instead, see `dt`).
    // Third-party peripherals (Logitech, …) are excluded on this branch too:
    // this build owns the tower case, nothing else. Laptop controllers
    // (AcerHID*) stay untouched.
    // Listing any of these would offer dead or out-of-scope controls next to
    // the live CASE section, so they are filtered here, once, for every host.
    let (live, dead): (Vec<_>, Vec<_>) = ctrls.into_iter().partition(|c| {
        // Match name + vendor: Logitech hides its brand out of some device
        // names ("G502 HERO Gaming Mouse" only carries it in the vendor).
        let hay = format!("{} {}", c.name, c.vendor).to_uppercase();
        !hay.starts_with("ACERDT")
            && !hay.contains("LOGITECH")
            && !hay.contains("G502")
            && !hay.contains("G512")
    });
    for d in &dead {
        eprintln!("engine: hiding inert OpenRGB controller '{}' (WMI owns it)", d.name);
    }
    Ok((c, live))
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
