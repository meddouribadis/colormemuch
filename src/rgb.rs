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
/// that produced it. This is the read half of observe-then-replay: capture the
/// current hardware state so a later write can be diffed against it.
pub fn snapshot(wmi: &Wmi, surface: Surface, max_selector: u32) -> Result<Vec<(u32, RawPacket)>> {
    let mut live = Vec::new();
    for sel in 0..=max_selector {
        if let Some(pkt) = read(wmi, surface, sel)? {
            live.push((sel, pkt));
        }
    }
    Ok(live)
}
