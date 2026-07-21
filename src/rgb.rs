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
    /// Expected effect: the keyboard breathes GREEN (mode=Breath). It's
    /// deliberately not red — the keyboard is red now, so green is an
    /// unmistakable, obviously-intentional change. Restore anytime by
    /// re-selecting a profile in PredatorSense.
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_write_keyboard_breath_green() {
        let wmi = match Wmi::connect() {
            Ok(w) => w,
            Err(e) => panic!("connect failed (elevated?): {e}"),
        };

        let payload = effect_payload(Effect::Breath, 5, 100, 1, Rgb(0x00, 0xFF, 0x00));
        let hex: Vec<String> = payload.iter().map(|b| format!("{b:02X}")).collect();
        eprintln!("SetGamingKBBacklight <- [{}]", hex.join(" "));

        match write_keyboard(&wmi, &payload) {
            Ok(status) => eprintln!("gmOutput = 0x{status:X} — watch the keyboard"),
            Err(e) => panic!("write failed: {e}"),
        }
    }
}
