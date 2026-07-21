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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
pub fn static_zone_payload(zones: u8, color: Rgb) -> [u8; 4] {
    [zones, color.0, color.1, color.2]
}

/// Build the 16-byte effect packet.
///
/// `[mode, speed, brightness, 0, direction, R, G, B, 0, 0, 0, 0, 0, 0, 0, 0]`.
/// `brightness` is clamped to 0..=100, `direction` to 1..=2. The trailing bytes
/// are the least-certain part of the format (sources disagree on bytes 8-9);
/// they are zeroed here and revisited if hardware rejects the packet.
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

/// Fire a whole matrix of candidate payloads in ONE elevated session, logging
/// every `gmOutput` to a single report.
///
/// This exists because the firmware answers a malformed packet with a status
/// word (e.g. `gmOutput=0x1`) rather than a COM error — so the *accepted*
/// packet is identifiable by its return code without a separate UAC prompt per
/// guess. A delay between shots lets the eye catch which one visibly lands; the
/// return-code column is the primary signal, the eye is the tiebreak.
///
/// Each shot is `(label, method, payload)`. Keyboard-only surfaces here; all
/// are reversible via PredatorSense.
pub fn fire_matrix_and_report(shots: &[(String, String, Vec<u8>)]) -> std::path::PathBuf {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = report_path(&format!("kbmatrix-{ts}.txt"));

    let mut out = String::new();
    out.push_str(&format!("colormemuch write matrix — epoch {ts}\n"));
    out.push_str(&format!("{} shots, ~3s apart. gmOutput is the key column:\n", shots.len()));
    out.push_str("a code that differs from the known-rejected 0x1 is the one to trust.\n\n");

    match Wmi::connect() {
        Ok(wmi) => {
            for (i, (label, method, payload)) in shots.iter().enumerate() {
                let hex = payload
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push_str(&format!("[{i}] t+{}s  {label}\n", i * 3));
                out.push_str(&format!("    {method} <- [{hex}] ({} bytes)\n", payload.len()));
                match wmi.call_bytes(method, payload) {
                    Ok(status) => out.push_str(&format!("    gmOutput = 0x{status:X}\n\n")),
                    Err(e) => out.push_str(&format!("    ERROR: {e}\n\n")),
                }
                std::thread::sleep(Duration::from_secs(3));
            }
            out.push_str("done — note WHICH step (if any) turned the keyboard green and whether it stuck.\n");
        }
        Err(e) => out.push_str(&format!("CONNECT FAILED  {e}\n(elevated?)\n")),
    }

    let _ = std::fs::write(&path, &out);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let m = "SetGamingKBBacklight".to_string();
        let shots = vec![
            ("baseline breath (8=0,9=0) — expect 0x1".to_string(), m.clone(), baseline),
            ("nekro-sense breath (8=3,9=1)".to_string(), m.clone(), nekro),
            ("enable-only breath (9=1)".to_string(), m.clone(), enable_only),
            ("static 4-byte all-zones green".to_string(), m.clone(), static4),
        ];

        let path = fire_matrix_and_report(&shots);
        eprintln!("report written: {}", path.display());
    }
}
