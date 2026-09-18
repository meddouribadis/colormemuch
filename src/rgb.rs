//! Domain layer over the raw WMI transport in [`crate::wmi`].
//!
//! **Read-side only, for now.** The probe (`scripts/probe_rgb.ps1`,
//! `probe-20260720-185522.txt`) established the transport and the *shape* of the
//! data, but the semantic byte layout — which byte is red, how brightness and
//! effect are packed — is not yet decoded. Decoding is done by controlled A/B:
//! set one known value in PredatorSense, re-probe, diff. Until that lands, the
//! `Set*` side stays unwritten rather than guessed, because those calls write
//! firmware on real hardware.
//!
//! What the probe *did* nail down and this module encodes:
//! * Array methods return a 15-byte packet ([`PACKET_LEN`]).
//! * `gmReturn == 0` means the payload is valid; non-zero means the `gmInput`
//!   selector addressed nothing (so [`read`] yields `None`, not an error).
//! * The keyboard's live state surfaces as the recurring `C7AE`/`0564` bytes —
//!   the anchor a future decode diffs against.

#![cfg(windows)]
// See wmi.rs: this layer's callers are tests and the coming CLI, not `main`
// yet, so silence dead-code noise until it's wired up.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::wmi::{Result, Wmi};

/// Width of the `UInt8Array` packet the LED/keyboard methods speak, as observed
/// on firmware "Gaming Function, Version 2.94". If a future device reports a
/// different width, decode logic keyed to fixed offsets must not assume this.
pub const PACKET_LEN: usize = 15;

/// A raw, undecoded reading from one of the array methods.
///
/// Holds the bytes exactly as firmware returned them. Deliberately offers no
/// `red()`/`brightness()` accessors yet — inventing them before the A/B decode
/// would bake in guesses. Use [`RawPacket::bytes`] and compare against a
/// baseline capture.
#[derive(Clone, PartialEq, Eq)]
pub struct RawPacket {
    bytes: Vec<u8>,
    /// The method's `gmReturn`. Always `0` for values obtained via [`read`].
    pub status: u8,
}

impl RawPacket {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Hex render matching the probe script's format, for eyeballing diffs.
    pub fn hex(&self) -> String {
        self.bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl std::fmt::Debug for RawPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RawPacket[{}] status={}", self.hex(), self.status)
    }
}

/// The two array-method families worth reading. Named by what they physically
/// drive, not by the raw method string.
#[derive(Clone, Copy, Debug)]
pub enum Surface {
    /// 4-zone keyboard backlight — `GetGamingKBBacklight`.
    Keyboard,
    /// Predator lid "shield" logo — `GetGamingLED`. The family the Linux prior
    /// art never mapped.
    LidShield,
}

impl Surface {
    fn getter(self) -> &'static str {
        match self {
            Surface::Keyboard => "GetGamingKBBacklight",
            Surface::LidShield => "GetGamingLED",
        }
    }
}

/// Read one selector of a surface. Returns `None` when the firmware reports the
/// selector as empty (`gmReturn != 0`), so a caller sweeping selectors can
/// `.flatten()` to keep only the live ones.
///
/// `selector` is the raw `gmInput`. Its encoding is not a plain 0-based index —
/// the probe showed only certain values are live (odd selectors for the
/// keyboard, 1 and 8 for the lid shield) — so callers should sweep and keep
/// whatever answers, not assume a contiguous range.
pub fn read(wmi: &Wmi, surface: Surface, selector: u32) -> Result<Option<RawPacket>> {
    let (bytes, status) = wmi.call_read_bytes(surface.getter(), selector)?;
    Ok((status == 0).then_some(RawPacket { bytes, status }))
}

/// Sweep a selector range and return every live packet, tagged by the selector
/// that produced it.
///
/// Note: on firmware v2.94 these getters return a *static descriptor*, not live
/// lighting state (proven by A/B — setting the keyboard red changed no bytes).
/// So this captures the firmware's template, useful for format study, not a
/// read-back of what's currently displayed.
pub fn snapshot(wmi: &Wmi, surface: Surface, max_selector: u32) -> Result<Vec<(u32, RawPacket)>> {
    let mut live = Vec::new();
    for sel in 0..=max_selector {
        if let Some(pkt) = read(wmi, surface, sel)? {
            live.push((sel, pkt));
        }
    }
    Ok(live)
}

// ---------------------------------------------------------------------------
// Write side — KEYBOARD ONLY.
//
// The lid shield (`SetGamingLED`) is deliberately absent: its only source is a
// different model's kernel module, it is the family the canonical Linux driver
// never mapped, and the getters can't confirm a write. It waits until the
// keyboard path is trusted. The keyboard, by contrast, is well-corroborated
// (our own descriptor matches the documented 16-byte layout) and fully
// reversible — PredatorSense restores it — so it's the safe first write.
// ---------------------------------------------------------------------------

/// An RGB triple. Byte order in the payload is R, G, B (verified against the
/// documented layout, not yet against hardware).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The four keyboard zones, left to right. The firmware addresses them as a
/// bitmask (`1 << (zone-1)`); [`Zone::bit`] yields it.
#[derive(Clone, Copy, Debug)]
pub enum Zone {
    One,
    Two,
    Three,
    Four,
}

impl Zone {
    pub fn bit(self) -> u8 {
        match self {
            Zone::One => 0x01,
            Zone::Two => 0x02,
            Zone::Three => 0x04,
            Zone::Four => 0x08,
        }
    }
}

/// All four zones at once.
pub const ALL_ZONES: u8 = 0x0F;

/// Keyboard lighting effects. Discriminants are the firmware's `mode` byte.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum Effect {
    Static = 0,
    Breath = 1,
    Neon = 2,
    Wave = 3,
    Shifting = 4,
    Zoom = 5,
}

/// Build the 4-byte per-zone static packet: `[zone_bitmask, R, G, B]`.
///
/// NOTE: this is NOT accepted by `SetGamingKBBacklight` — that method rejects a
/// 4-byte array with WBEM_E_INVALID_PARAMETER (verified 2026-07-20); it wants a
/// 16-byte effect packet. Per-zone static color almost certainly routes through
/// `SetGamingRgbKb` (the `UInt64` method) instead, which is not yet wired up.
/// Kept for when that path is decoded.
pub fn static_zone_payload(zones: u8, color: Rgb) -> [u8; 4] {
    [zones, color.0, color.1, color.2]
}

/// Build the 16-byte effect packet.
///
/// `[mode, speed, brightness, 0, direction, R, G, B, 0, EN, 0, 0, 0, 0, 0, 0]`.
/// `brightness` is clamped to 0..=100, `direction` to 1..=2.
///
/// Byte 9 is the **enable flag** and MUST be 1 or firmware rejects the packet
/// (`gmOutput=0x1`, no visible change). Verified on CYPHER 2026-07-20: the same
/// packet with byte 9 = 0 returned 0x1, with byte 9 = 1 returned 0x0 (accepted).
/// Byte 8 (nekro-sense sets 3) turned out irrelevant to acceptance and is left
/// 0 here except where an effect needs it. The method also strictly requires 16
/// bytes — a 4-byte packet is rejected with WBEM_E_INVALID_PARAMETER.
pub fn effect_payload(effect: Effect, speed: u8, brightness: u8, direction: u8, color: Rgb) -> [u8; 16] {
    let mut p = [0u8; 16];
    p[0] = effect as u8;
    p[1] = speed;
    p[2] = brightness.min(100);
    p[3] = 0;
    p[4] = direction.clamp(1, 2);
    p[5] = color.0;
    p[6] = color.1;
    p[7] = color.2;
    p[9] = 1; // enable — required; without it the write is a no-op.
    p
}

/// Fire a keyboard payload at `SetGamingKBBacklight`. Returns the method's
/// `gmOutput` (a status word; meaning not yet decoded).
///
/// This writes firmware state. Reversible via PredatorSense, but still a real
/// hardware write — callers gate it behind explicit user intent.
pub fn write_keyboard(wmi: &Wmi, payload: &[u8]) -> Result<u64> {
    wmi.call_bytes("SetGamingKBBacklight", payload)
}

/// Connect, fire one array-method write, and record the whole story to a
/// plaintext report file — returning its path.
///
/// This mirrors `scripts/probe_rgb.ps1`: results come back as a *file to paste*,
/// not console noise. Connect failures, the exact payload, and the `gmOutput`
/// (or error) all land in the file, so the file alone tells the full tale even
/// when the terminal is unusable. The report is written to the current
/// directory (the package root under `cargo test`), next to the probe reports.
///
/// `method` is the WMI method name, so this serves the lid shield too once that
/// path is trusted — not just `SetGamingKBBacklight`.
pub fn fire_and_report(label: &str, method: &str, payload: &[u8]) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Absolute path anchored at the crate root, NOT the cwd: an elevated shell
    // starts in System32, so a cwd-relative report lands somewhere invisible
    // (or fails to write). This keeps it in the repo where it can be read.
    let path = report_path(&format!("kbwrite-{ts}.txt"));

    let hex = payload
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");

    let mut out = String::new();
    out.push_str(&format!("colormemuch write report — epoch {ts}\n"));
    out.push_str(&format!("label:  {label}\n"));
    out.push_str(&format!("method: {method}\n"));
    out.push_str(&format!("payload: [{hex}] ({} bytes)\n\n", payload.len()));

    match Wmi::connect() {
        Ok(wmi) => match wmi.call_bytes(method, payload) {
            Ok(status) => {
                out.push_str(&format!("result: OK  gmOutput=0x{status:X}\n"));
                out.push_str("watch the hardware — then note what actually happened.\n");
            }
            Err(e) => out.push_str(&format!("result: WRITE FAILED  {e}\n")),
        },
        Err(e) => out.push_str(&format!("result: CONNECT FAILED  {e}\n(elevated?)\n")),
    }

    // Best-effort: if even the file write fails, the returned path lets the
    // caller report that rather than silently swallowing it.
    let _ = std::fs::write(&path, &out);
    path
}

/// Resolve a report filename to an absolute path under the crate root.
///
/// `CARGO_MANIFEST_DIR` is baked in at compile time, so this is stable
/// regardless of the elevated process's working directory.
fn report_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

/// One write in a decode matrix: which method, what payload shape, and how
/// long to hold before the next shot (so the eye can catch what changed).
pub struct Shot {
    pub label: String,
    pub method: String,
    pub payload: Payload,
    pub pause_ms: u64,
}

/// The interface's two parameter shapes, so one matrix can mix byte-array and
/// packed-u64 methods in a single elevated session.
pub enum Payload {
    Bytes(Vec<u8>),
    Packed(u64),
}

/// Pack a per-zone colour for `SetGamingRgbKb` (the `UInt64` method).
///
/// Grounded in CYPHER's own probe, not prior art alone: `GetGamingRgbKb`
/// answered exactly selectors 1, 2, 4, 8 — the zone bitmask — and returned
/// `0x00000000_C7AE0000` while the firmware's template colour is
/// (R,G,B) = (00, AE, C7). That is the Linux per-zone struct `[zone, R, G, B]`
/// packed little-endian: `zone | R<<8 | G<<16 | B<<24` (the getter zeroes the
/// zone echo in the low byte).
pub fn rgbkb_packed(zone_bit: u8, c: Rgb) -> u64 {
    (zone_bit as u64) | ((c.0 as u64) << 8) | ((c.1 as u64) << 16) | ((c.2 as u64) << 24)
}

/// Pack `SetGamingLEDColor` the way its own getter echoes state.
///
/// `GetGamingLEDColor[1]` returned `0x0000_0564_C7AE_0000`, which decomposes as
/// speed(5)<<40 | brightness(0x64)<<32 | B(C7)<<24 | G(AE)<<16 | R(00)<<8 —
/// the same colour/brightness/speed values the 15-byte KB descriptor carries.
/// Selector goes in the low byte.
pub fn ledcolor_packed(selector: u8, c: Rgb, brightness: u8, speed: u8) -> u64 {
    (selector as u64)
        | ((c.0 as u64) << 8)
        | ((c.1 as u64) << 16)
        | ((c.2 as u64) << 24)
        | ((brightness.min(100) as u64) << 32)
        | ((speed as u64) << 40)
}

/// Fire a whole matrix of candidate payloads in ONE elevated session, logging
/// every `gmOutput` to a single report.
///
/// This exists because the firmware answers a malformed packet with a status
/// word (`0x0` accepted, `0x1` rejected) rather than a COM error — so the
/// *accepted* packet is identifiable by its return code without a separate UAC
/// prompt per guess. Pauses between shots let the eye catch which one visibly
/// lands; the return-code column is the primary signal, the eye the tiebreak.
pub fn fire_matrix_and_report(shots: &[Shot]) -> std::path::PathBuf {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = report_path(&format!("kbmatrix-{ts}.txt"));

    let mut out = String::new();
    out.push_str(&format!("colormemuch write matrix — epoch {ts}\n"));
    out.push_str(&format!(
        "{} shots. gmOutput: 0x0 = accepted, 0x1 = rejected.\n\n",
        shots.len()
    ));

    match Wmi::connect() {
        Ok(wmi) => {
            for (i, shot) in shots.iter().enumerate() {
                out.push_str(&format!("[{i}] {}\n", shot.label));
                let result = match &shot.payload {
                    Payload::Bytes(b) => {
                        let hex = b
                            .iter()
                            .map(|x| format!("{x:02X}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        out.push_str(&format!(
                            "    {} <- [{hex}] ({} bytes)\n",
                            shot.method,
                            b.len()
                        ));
                        wmi.call_bytes(&shot.method, b)
                    }
                    Payload::Packed(v) => {
                        out.push_str(&format!("    {} <- 0x{v:016X}\n", shot.method));
                        wmi.call_packed(&shot.method, *v)
                    }
                };
                match result {
                    Ok(status) => out.push_str(&format!("    gmOutput = 0x{status:X}\n\n")),
                    Err(e) => out.push_str(&format!("    ERROR: {e}\n\n")),
                }
                std::thread::sleep(Duration::from_millis(shot.pause_ms));
            }
            out.push_str("done.\n");
        }
        Err(e) => out.push_str(&format!("CONNECT FAILED  {e}\n(elevated?)\n")),
    }

    let _ = std::fs::write(&path, &out);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(label: &str, method: &str, payload: Payload, pause_ms: u64) -> Shot {
        Shot {
            label: label.to_string(),
            method: method.to_string(),
            payload,
            pause_ms,
        }
    }

    #[test]
    fn packed_layouts_match_probe_echoes() {
        // These constants are CYPHER's actual getter echoes — the packers must
        // reproduce them from the decomposed values or the layout is wrong.
        assert_eq!(rgbkb_packed(0, Rgb(0x00, 0xAE, 0xC7)), 0xC7AE0000);
        assert_eq!(rgbkb_packed(1, Rgb(0x00, 0xAE, 0xC7)), 0xC7AE0001);
        assert_eq!(ledcolor_packed(0, Rgb(0x00, 0xAE, 0xC7), 0x64, 5), 0x0000_0564_C7AE_0000);
    }

    #[test]
    fn static_payload_layout() {
        assert_eq!(static_zone_payload(ALL_ZONES, Rgb(0xFF, 0x00, 0x00)), [0x0F, 0xFF, 0x00, 0x00]);
        assert_eq!(Zone::Three.bit(), 0x04);
    }

    #[test]
    fn effect_payload_layout_and_clamps() {
        // Mirrors CYPHER's own descriptor: speed 5, brightness 100, direction 1.
        let p = effect_payload(Effect::Breath, 5, 100, 1, Rgb(0x00, 0xAE, 0xC7));
        assert_eq!(&p[..8], &[1, 5, 100, 0, 1, 0x00, 0xAE, 0xC7]);
        // Byte 9 is the enable flag — firmware rejects the packet without it.
        assert_eq!(p[9], 1);
        // brightness over 100 and direction out of range are clamped, not wrapped.
        let p = effect_payload(Effect::Static, 0, 250, 9, Rgb(0, 0, 0));
        assert_eq!(p[2], 100);
        assert_eq!(p[4], 2);
    }

    /// FIRST HARDWARE WRITE — keyboard only, reversible.
    ///
    /// `#[ignore]` by default: it does not run under plain `cargo test` or in
    /// CI. Fire it deliberately from an ELEVATED shell:
    ///
    /// ```text
    /// cargo test --bin colormemuch -- --ignored --exact \
    ///     rgb::tests::hw_write_keyboard_breath_green --nocapture
    /// ```
    ///
    /// FIRST-WRITE DECODE MATRIX — keyboard only, reversible.
    ///
    /// The first attempt (bytes 8–9 zeroed) returned `gmOutput=0x1` and did
    /// nothing visible. This fires four green candidates in one elevated
    /// session to find which packet the firmware actually accepts. Green is
    /// deliberate — the keyboard is red now, so any green is unmistakably ours.
    ///
    /// `#[ignore]` by default. Run explicitly and elevated:
    ///
    /// ```text
    /// cargo test --bin colormemuch -- --ignored --exact \
    ///     rgb::tests::hw_keyboard_matrix --nocapture
    /// ```
    ///
    /// Reads `kbmatrix-<epoch>.txt` (absolute path, crate root) for the
    /// gmOutput column; restore anytime via PredatorSense.
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_keyboard_matrix() {
        let green = Rgb(0x00, 0xFF, 0x00);

        // Baseline: what returned 0x1 last time (8=0, 9=0).
        let baseline = effect_payload(Effect::Breath, 5, 100, 1, green).to_vec();
        // nekro-sense's exact effect buffer: [8]=3, [9]=1 (the enable flag).
        let mut nekro = baseline.clone();
        nekro[8] = 3;
        nekro[9] = 1;
        // Enable flag only, to isolate whether [9] alone flips it.
        let mut enable_only = baseline.clone();
        enable_only[9] = 1;
        // The other documented shape entirely: 4-byte per-zone static, all zones.
        let static4 = static_zone_payload(ALL_ZONES, green).to_vec();

        let m = "SetGamingKBBacklight";
        let shots = vec![
            shot("baseline breath (8=0,9=0) — expect 0x1", m, Payload::Bytes(baseline), 3000),
            shot("nekro-sense breath (8=3,9=1)", m, Payload::Bytes(nekro), 3000),
            shot("enable-only breath (9=1)", m, Payload::Bytes(enable_only), 3000),
            shot("static 4-byte all-zones green", m, Payload::Bytes(static4), 3000),
        ];

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// CONFIRM the effect write is *visible*, not just accepted.
    ///
    /// Walks the keyboard red → green → blue via the now-decoded effect packet
    /// (breath, enable flag set), ~3s each. A coincidence can't follow a
    /// commanded colour sequence, so if the keys track it, the write path is
    /// confirmed end to end. All three should log `gmOutput=0x0`.
    ///
    /// `#[ignore]` by default. Run elevated:
    ///
    /// ```text
    /// cargo test --bin colormemuch -- --ignored --exact \
    ///     rgb::tests::hw_confirm_effect --nocapture
    /// ```
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_confirm_effect() {
        let m = "SetGamingKBBacklight";
        let breath = |color| Payload::Bytes(effect_payload(Effect::Breath, 4, 100, 1, color).to_vec());
        let shots = vec![
            shot("confirm RED", m, breath(Rgb(0xFF, 0x00, 0x00)), 3000),
            shot("confirm GREEN", m, breath(Rgb(0x00, 0xFF, 0x00)), 3000),
            shot("confirm BLUE", m, breath(Rgb(0x00, 0x00, 0xFF)), 3000),
        ];
        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// THE DECISIVE RUN — tests the two methods we have NOT tried yet, with
    /// packings derived from this machine's own getter echoes.
    ///
    /// `SetGamingKBBacklight` accepts our packets (0x0) but nothing shows, so
    /// it is probably the *effect/mode* register, not the colour register.
    /// The colour candidates, both untouched until now:
    ///
    /// 1. `SetGamingRgbKb` — per-zone, `zone | R<<8 | G<<16 | B<<24`
    ///    (probe: getter answers exactly selectors 1/2/4/8 = zone bits).
    /// 2. `SetGamingLEDColor` — `sel | R<<8 | G<<16 | B<<24 | bright<<32 |
    ///    speed<<40` (probe: getter echo 0x0564C7AE0000 decomposes exactly so).
    ///
    /// Walks all four zones red → green → blue via (1), then tries (2).
    /// Reversible via PredatorSense.
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_rgbkb_walk() {
        let mut shots = Vec::new();

        // Put the keyboard in static mode first so per-zone colours can show.
        shots.push(shot(
            "enter static mode (KBBacklight mode=0, enable)",
            "SetGamingKBBacklight",
            Payload::Bytes(effect_payload(Effect::Static, 0, 100, 1, Rgb(0xFF, 0, 0)).to_vec()),
            1000,
        ));

        // Hypothesis 1: SetGamingRgbKb is the per-zone colour register.
        for (name, c) in [
            ("RED", Rgb(0xFF, 0x00, 0x00)),
            ("GREEN", Rgb(0x00, 0xFF, 0x00)),
            ("BLUE", Rgb(0x00, 0x00, 0xFF)),
        ] {
            for z in [1u8, 2, 4, 8] {
                shots.push(shot(
                    &format!("RgbKb zone 0x{z:X} {name}"),
                    "SetGamingRgbKb",
                    Payload::Packed(rgbkb_packed(z, c)),
                    if z == 8 { 3000 } else { 150 },
                ));
            }
        }

        // Hypothesis 2: the LEDColor/Behavior pair. Behavior first (getter
        // echo for sel 1 was 0x100 → sel | behavior<<8), then colour.
        shots.push(shot(
            "LEDBehavior sel1 behavior1",
            "SetGamingLEDBehavior",
            Payload::Packed(0x101),
            500,
        ));
        for (name, c) in [("RED", Rgb(0xFF, 0x00, 0x00)), ("GREEN", Rgb(0x00, 0xFF, 0x00))] {
            shots.push(shot(
                &format!("LEDColor sel1 {name}"),
                "SetGamingLEDColor",
                Payload::Packed(ledcolor_packed(1, c, 100, 0)),
                3000,
            ));
        }

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// DESKTOP (PO5-660) BEHAVIOR MATRIX — finds the `SetGamingLedBehavior`
    /// array shape by echoing the getter's own bytes.
    ///
    /// Context: on the PO5-660 the color register (`SetGamingRgbSetting`, u64
    /// `[sel16][R][G][B][flags]`) is decoded and proven, and `GetGamingLed-
    /// Behavior` returns a live 8-byte blob `[00][01][mode][03][03][00][FF][00]`
    /// where byte 2 is the effect id (00=static, 02=breathing, 06=rainbow,
    /// 09=wave — all observed via PredatorSense A/B). But the SETTER takes a
    /// `UInt8Array` whose length/layout is unknown (PowerShell CIM can't even
    /// marshal byte[] to it — hence Rust + raw SAFEARRAY here).
    ///
    /// Shots (all reversible via PredatorSense; start from wave-red):
    /// 1. wave echo (control) — expect accepted, no visible change.
    /// 2. static echo — expect accepted AND wave -> static red.
    /// 3. sel-prefixed static echo — if (2) is rejected, maybe the array wants
    ///    an area selector up front.
    ///
    /// `#[ignore]` by default. Run ELEVATED:
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     rgb::tests::hw_dt_behavior_matrix --nocapture
    /// ```
    ///
    /// Reads `kbmatrix-<epoch>.txt` (crate root) for the gmOutput column.
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_dt_behavior_matrix() {
        let m = "SetGamingLedBehavior";
        let shots = vec![
            shot(
                "wave echo control (expect accepted, no visible change)",
                m,
                Payload::Bytes(vec![0x00, 0x01, 0x09, 0x03, 0x03, 0x00, 0xFF, 0x00]),
                3000,
            ),
            shot(
                "static echo (expect accepted AND wave->static red)",
                m,
                Payload::Bytes(vec![0x00, 0x01, 0x00, 0x03, 0x03, 0x00, 0xFF, 0x00]),
                3000,
            ),
            shot(
                "sel-prefixed static echo",
                m,
                Payload::Bytes(vec![0x01, 0x00, 0x01, 0x00, 0x03, 0x03, 0x00, 0xFF, 0x00]),
                3000,
            ),
        ];

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// DESKTOP (PO5-660) SHAPE PROBE — separates "Rust invoke path broken"
    /// from "behavior array has the wrong length".
    ///
    /// Shot 0 calls `GetGamingRgbSetting` (packed u64, no array involved): if
    /// it echoes the live color register, the whole Rust invoke path (connect,
    /// in-params, out-params) is proven and the 0x80041008s from the behavior
    /// matrix are purely about the array shape. If it fails too, the problem
    /// is marshaling, not length.
    ///
    /// Shots 1..N send zero-filled arrays of many lengths to
    /// `SetGamingLedBehavior`. A wrong length dies in parameter validation
    /// (0x80041008) WITHOUT touching hardware; the accepted length executes
    /// (a zeroed behavior most likely reads as static/off-ish — reversible
    /// via PredatorSense, and the gmOutput column flags exactly which shot
    /// acted). Pauses are short: only the shape matters here, not the eye.
    ///
    /// `#[ignore]` by default. Run ELEVATED:
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     rgb::tests::hw_dt_shape_probe --nocapture
    /// ```
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_dt_shape_probe() {
        let mut shots = vec![shot(
            "GET RgbSetting sel1 via Rust (expect live color register)",
            "GetGamingRgbSetting",
            Payload::Packed(1),
            500,
        )];
        for len in [4usize, 8, 9, 10, 12, 15, 16, 24, 32, 40] {
            shots.push(shot(
                &format!("SET LedBehavior zeroed len={len}"),
                "SetGamingLedBehavior",
                Payload::Bytes(vec![0u8; len]),
                300,
            ));
        }

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// DESKTOP (PO5-660) BEHAVIOR-16 MATRIX — `SetGamingLedBehavior` demands a
    /// 16-byte array (every other length dies with 0x80041008; len 16 answers
    /// with a firmware status word instead — proven by `hw_dt_shape_probe`).
    ///
    /// The 8-byte `GetGamingLedBehavior` blob is half the story. Candidates
    /// for the full 16 bytes, from wave-red start (all reversible):
    /// a. wave echo + 8 zero pad — control: accepted + no visible change.
    /// b. static echo + 8 zero pad — accepted + wave -> static red.
    /// c. 8 zero pad + wave echo — echo in the high half instead.
    /// d. wave echo doubled — both halves carry the blob.
    ///
    /// `#[ignore]` by default. Run ELEVATED:
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     rgb::tests::hw_dt_behavior16_matrix --nocapture
    /// ```
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_dt_behavior16_matrix() {
        const WAVE: [u8; 8] = [0x00, 0x01, 0x09, 0x03, 0x03, 0x00, 0xFF, 0x00];
        const STATIC: [u8; 8] = [0x00, 0x01, 0x00, 0x03, 0x03, 0x00, 0xFF, 0x00];
        const PAD: [u8; 8] = [0; 8];
        let cat = |a: &[u8], b: &[u8]| {
            let mut v = Vec::with_capacity(16);
            v.extend_from_slice(a);
            v.extend_from_slice(b);
            v
        };

        let m = "SetGamingLedBehavior";
        let shots = vec![
            shot("wave echo + pad (control: no visible change)", m, Payload::Bytes(cat(&WAVE, &PAD)), 3000),
            shot("static echo + pad (wave -> static red?)", m, Payload::Bytes(cat(&STATIC, &PAD)), 3000),
            shot("pad + wave echo (high half?)", m, Payload::Bytes(cat(&PAD, &WAVE)), 3000),
            shot("wave echo doubled", m, Payload::Bytes(cat(&WAVE, &WAVE)), 3000),
        ];

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// DESKTOP (PO5-660) BEHAVIOR-HIGH-HALF MATRIX — the low 8 bytes carry the
    /// behavior echo (high-half echo is rejected with 0x1), but echo + zero
    /// pad is accepted with NO visible change. So the high 8 bytes are NOT
    /// padding: either an area mask (zeros = apply nowhere) or a combined
    /// color slot. From wave-red start:
    /// e. static echo + 8×0xFF — mask-all-ones theory: -> static red?
    /// f. static echo + [sel1, red, on] — combined theory: -> static red?
    /// g. wave echo + [sel1, red, on] — control: no visible change either way.
    ///
    /// `#[ignore]` by default. Run ELEVATED:
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     rgb::tests::hw_dt_behavior_hi_matrix --nocapture
    /// ```
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_dt_behavior_hi_matrix() {
        const WAVE: [u8; 8] = [0x00, 0x01, 0x09, 0x03, 0x03, 0x00, 0xFF, 0x00];
        const STATIC: [u8; 8] = [0x00, 0x01, 0x00, 0x03, 0x03, 0x00, 0xFF, 0x00];
        // sel1 (u16 LE) + red + flags 09, as decoded for SetGamingRgbSetting.
        const SEL1_RED_ON: [u8; 8] = [0x01, 0x00, 0xFF, 0x00, 0x00, 0x09, 0x00, 0x00];
        let cat = |a: &[u8], b: &[u8]| {
            let mut v = Vec::with_capacity(16);
            v.extend_from_slice(a);
            v.extend_from_slice(b);
            v
        };

        let m = "SetGamingLedBehavior";
        let shots = vec![
            shot("static + FF mask (mask theory)", m, Payload::Bytes(cat(&STATIC, &[0xFF; 8])), 4000),
            shot("static + sel1/red/on (combined theory)", m, Payload::Bytes(cat(&STATIC, &SEL1_RED_ON)), 4000),
            shot("wave + sel1/red/on (control: no change)", m, Payload::Bytes(cat(&WAVE, &SEL1_RED_ON)), 4000),
        ];

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// DESKTOP (PO5-660) SINGLE STATIC SHOT — one accepted-shape write plus
    /// full before/after probes (taken separately) to map its scope.
    ///
    /// Sends static echo + zero pad (the shape the firmware accepts). Watch
    /// MOTION per zone (wave-red -> static-red keeps the color, only the
    /// animation stops), then diff the pre/post probe files: every selector
    /// whose behavior byte 2 flipped 09 -> 00 was touched by this write.
    ///
    /// `#[ignore]` by default. Run ELEVATED:
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     rgb::tests::hw_dt_one_shot --nocapture
    /// ```
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_dt_one_shot() {
        let shots = vec![shot(
            "static echo + pad (single)",
            "SetGamingLedBehavior",
            Payload::Bytes(vec![
                0x00, 0x01, 0x00, 0x03, 0x03, 0x00, 0xFF, 0x00, //
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]),
            1000,
        )];

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }

    /// DESKTOP (PO5-660) HIGH-HALF CANDIDATES — two single-purpose shots from
    /// uniform wave-red (reuse the last post-probe as baseline):
    /// 1. static echo + 8×0xFF — mask theory: all zones -> static red?
    /// 2. static echo + [sel2, blue, on] — combined theory: top fan alone ->
    ///    static blue (sel2 = top/GPU fan, proven by the color walk)?
    ///
    /// Watch MOTION + COLOR per zone on each shot, then probe once and diff
    /// against baseline: which selectors changed, and how.
    ///
    /// `#[ignore]` by default. Run ELEVATED:
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     rgb::tests::hw_dt_two_shot --nocapture
    /// ```
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_dt_two_shot() {
        const STATIC: [u8; 8] = [0x00, 0x01, 0x00, 0x03, 0x03, 0x00, 0xFF, 0x00];
        // sel2 (top/GPU fan, u16 LE) + blue + flags 09, per the RgbSetting layout.
        const SEL2_BLUE_ON: [u8; 8] = [0x02, 0x00, 0x00, 0x00, 0xFF, 0x09, 0x00, 0x00];
        let cat = |a: &[u8], b: &[u8]| {
            let mut v = Vec::with_capacity(16);
            v.extend_from_slice(a);
            v.extend_from_slice(b);
            v
        };

        let m = "SetGamingLedBehavior";
        let shots = vec![
            shot("static + FFmask (all -> static red?)", m, Payload::Bytes(cat(&STATIC, &[0xFF; 8])), 5000),
            shot("static + sel2/blue/on (top -> static blue?)", m, Payload::Bytes(cat(&STATIC, &SEL2_BLUE_ON)), 5000),
        ];

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }
}
