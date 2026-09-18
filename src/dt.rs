//! Desktop-tower (PO5-660) LED driver over `AcerGamingFunction` WMI.
//!
//! The OpenRGB server on these towers is a showroom with no engine behind it:
//! every write is accepted and nothing reaches the LEDs. PredatorSense drives
//! the hardware through WMI instead, and so do we — by replaying its exact
//! transactions, captured with a Frida hook on `IWbemServices::ExecMethod`
//! (see `scripts/frida_wmi_trace.js`).
//!
//! ## The static-color transaction (captured, replayable)
//!
//! Changing a color in PredatorSense (static red -> static green, global)
//! fires, ~50 ms apart on the same thread:
//!
//! 1. `SetGamingLedBehavior` ← 16 bytes:
//!    `[01 00 01 00 0f 03 03 00 | 00 00 00 00 00 00 00 00]`
//! 2. `SetGamingRgbSetting` ← u64 (decimal string over the wire):
//!    `0x0003093CF03C0001` = `[01][00][3C][F0][3C][09][03][00]`
//!
//! ### Color word layout (SET)
//!
//! 8 bytes, little-endian: `[sel_lo][sel_hi][R][G][B][flags][X][00]`, i.e.
//! `sel | R<<16 | G<<24 | B<<32 | flags<<40 | X<<48`.
//!
//! * `sel` is a **u16** area id (1 = global broadcast, 2 = top/GPU fan,
//!   4 = front, 16 = rear, 8 = unknown). Proof: the captured green
//!   `(60, 240, 60)` only lands on green when R starts at byte 2.
//! * `flags`: `0x09` = output on, `0x01` = output off (PredatorSense OFF flips
//!   only this byte, color kept).
//! * `X` = `0x03`, constant across captures, meaning unknown — replayed
//!   verbatim, never invented.
//!
//! Anchors: static green `0x0003093CF03C0001`, lime pick
//! `0x00030944FF7B0001` (`(123, 255, 68)`, same shape).
//!
//! ### Color word layout (GET echo — DIFFERENT!)
//!
//! `GetGamingRgbSetting` returns 5 significant bytes `[00][R][G][B][flags]`
//! (selector zeroed, like the laptop provider): solid red `0x090000FF00`,
//! template teal `0x09C7AE0000`, OFF `0x010000FF00`, rainbow-black
//! `0x0900000000`. So the getter drops the sel_lo byte — pack and unpack are
//! deliberately asymmetric.
//!
//! ### Behavior array (SET)
//!
//! `[sel_lo][sel_hi]` + 6 opaque bytes + 8 zero pad. For static-global the
//! captured template is replayed verbatim; bytes 2..7 likely carry PS's
//! speed/brightness slider state (`0f …`), adjustable once captured at other
//! slider positions. Effect ids (from read-only `GetGamingLedBehavior` byte-2
//! A/B): `0x00` static, `0x02` battement, `0x06` rainbow, `0x09` wave.
//!
//! ## What we deliberately do NOT do
//!
//! * No guessed behavior writes: non-captured 16-byte payloads **stall the
//!   lighting controller** (LEDs freeze until PredatorSense re-applies).
//! * No lone color writes: a lone `SetGamingRgbSetting` blanks zones
//!   unpredictably — the firmware cross-checks the color register against
//!   companion state, so only the full transaction ships.
//! * Per-area behavior writes: only sel = 1 (global) is captured. Per-area
//!   colors through the same transaction shape with patched sel bytes are
//!   **uncaptured — do not ship** until a per-area Frida capture confirms.
//!
//! Elevation required, like all of [`crate::wmi`].

#![cfg(windows)]
#![allow(dead_code)]

use crate::rgb::Rgb;
use crate::wmi::{Result, Wmi};

use serde::{Deserialize, Serialize};

/// Global static case state (v1 scope: the whole case minus RAM, one color).
/// `None` in [`crate::engine::EngineState`] means "don't touch the case".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DtState {
    pub color: Rgb,
    pub on: bool,
    /// Per-area overrides, applied after the global (if any), in order.
    /// FRONT is capture-backed; TOP/REAR/AUX are experimental until their
    /// own Frida captures land (same shape, unconfirmed sel semantics).
    #[serde(default)]
    pub areas: Vec<AreaCmd>,
}

/// One per-area static command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AreaCmd {
    pub area: u16,
    pub color: Rgb,
    pub on: bool,
}

impl Default for DtState {
    fn default() -> Self {
        Self {
            color: Rgb(0x00, 0xE5, 0xFF),
            on: true,
            areas: Vec::new(),
        }
    }
}

/// Area selectors for the color word (u16, little-endian bytes 0-1). Mapped
/// on hardware by walking one color per selector and watching the case.
pub mod area {
    /// Broadcast: every area at once (RAM excluded — it rides another channel).
    pub const ALL: u16 = 1;
    /// Top fan / above the GPU.
    pub const TOP: u16 = 2;
    /// Front face.
    pub const FRONT: u16 = 4;
    /// Unknown — nothing identified yet (and not the RAM).
    pub const AUX: u16 = 8;
    /// Rear fan.
    pub const REAR: u16 = 16;
}

/// Flags byte values observed in the color register.
pub mod flags {
    /// Output on (static/breathing/wave/rainbow states).
    pub const ON: u8 = 0x09;
    /// Output off (PredatorSense "OFF" flips only this byte, color kept).
    pub const OFF: u8 = 0x01;
}

/// Trailer byte (byte 6 of the color word): `0x03` in every capture, meaning
/// unknown. Replayed verbatim.
pub const TRAILER: u8 = 0x03;

/// Captured `SetGamingLedBehavior` template for static-global (PredatorSense
/// static color change, global scope). Bytes 0-1 are the sel u16; the rest is
/// replayed verbatim (likely PS slider state). Only sel = 1 is captured —
/// do not patch other selectors in without a matching capture.
pub const BEHAVIOR_STATIC_GLOBAL: [u8; 16] = [
    0x01, 0x00, 0x01, 0x00, 0x0f, 0x03, 0x03, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Static behavior template with the area selector patched in.
///
/// Proven by a second Frida capture: PredatorSense changing only the front
/// ("zone 1" in its UI) red → teal sends the global template byte-identical
/// except bytes 0-1 = `[04 00]`, followed by the rgb word with sel = 4.
/// Per-area transactions are therefore the global shape with patched sel —
/// for the color word (captured) and, by the same capture, the behavior word.
/// Selectors beyond global + front are the same shape but await their own
/// capture before shipping (see [`area`]).
pub fn behavior_static_for(area: u16) -> [u8; 16] {
    let mut b = BEHAVIOR_STATIC_GLOBAL;
    b[0..2].copy_from_slice(&area.to_le_bytes());
    b
}

/// Effect ids from read-only `GetGamingLedBehavior` byte-2 A/B. Knowledge
/// only — the write path replays [`BEHAVIOR_STATIC_GLOBAL`], never these.
pub mod effect {
    pub const STATIC: u8 = 0x00;
    /// "Battement" as labeled by PredatorSense FR.
    pub const HEARTBEAT: u8 = 0x02;
    pub const RAINBOW: u8 = 0x06;
    pub const WAVE: u8 = 0x09;
}

/// Pack a `SetGamingRgbSetting` word:
/// `[sel_lo][sel_hi][R][G][B][flags][X][00]`.
///
/// `flags` must come from [`flags`]; the trailer is always [`TRAILER`].
pub fn pack_setting(area: u16, color: Rgb, flags: u8) -> u64 {
    (area as u64)
        | ((color.0 as u64) << 16)
        | ((color.1 as u64) << 24)
        | ((color.2 as u64) << 32)
        | ((flags as u64) << 40)
        | ((TRAILER as u64) << 48)
}

/// Unpack a `GetGamingRgbSetting` echo: `[00][R][G][B][flags]`. Asymmetric
/// with [`pack_setting`] by firmware design (getter drops the selector).
pub fn unpack_echo(word: u64) -> (Rgb, u8) {
    let r = ((word >> 8) & 0xFF) as u8;
    let g = ((word >> 16) & 0xFF) as u8;
    let b = ((word >> 24) & 0xFF) as u8;
    let flags = ((word >> 32) & 0xFF) as u8;
    (Rgb(r, g, b), flags)
}

/// Apply a static global color: the captured transaction in PredatorSense's
/// order (behavior first, color ~60 ms later). Only [`area::ALL`] is
/// capture-backed. Reversible via PredatorSense.
pub fn apply_static_global(wmi: &Wmi, color: Rgb) -> Result<u64> {
    apply_static(wmi, area::ALL, color, true)
}

/// Apply a static color to one area: behavior template with patched sel,
/// then the color word, PredatorSense order.
///
/// `on = false` mirrors PredatorSense OFF (behavior untouched in its
/// snapshots — but our template targets a static state already, so the
/// template write is a no-op semantically; only the flags flip the output).
/// FRONT is capture-backed; TOP/REAR/AUX share the proven shape but are
/// marked experimental until their own captures land.
pub fn apply_static(wmi: &Wmi, area: u16, color: Rgb, on: bool) -> Result<u64> {
    wmi.call_bytes("SetGamingLedBehavior", &behavior_static_for(area))?;
    std::thread::sleep(std::time::Duration::from_millis(60));
    wmi.call_packed(
        "SetGamingRgbSetting",
        pack_setting(area, color, if on { flags::ON } else { flags::OFF }),
    )
}

/// Read back an area's color register (selector echo reads zero — see
/// [`unpack_echo`]).
pub fn get_color(wmi: &Wmi, area: u16) -> Result<(Rgb, u8)> {
    let word = wmi.call_packed("GetGamingRgbSetting", area as u64)?;
    Ok(unpack_echo(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_setting_reproduces_predatorsense_captures() {
        // Frida captures of real PredatorSense static-global color changes:
        // (60, 240, 60) green and (123, 255, 68) lime. Byte-exact replay or
        // the packer is wrong.
        assert_eq!(
            pack_setting(area::ALL, Rgb(0x3C, 0xF0, 0x3C), flags::ON),
            0x0003093CF03C0001
        );
        assert_eq!(
            pack_setting(area::ALL, Rgb(0x7B, 0xFF, 0x44), flags::ON),
            0x00030944FF7B0001
        );
    }

    #[test]
    fn unpack_echo_matches_getter_snapshots() {
        // `probe_dt_led.ps1` echoes (selector zeroed in all).
        assert_eq!(unpack_echo(0x090000FF00), (Rgb(0xFF, 0, 0), flags::ON));
        assert_eq!(unpack_echo(0x09C7AE0000), (Rgb(0x00, 0xAE, 0xC7), flags::ON));
        assert_eq!(unpack_echo(0x010000FF00), (Rgb(0xFF, 0, 0), flags::OFF));
        assert_eq!(unpack_echo(0x0900000000), (Rgb(0, 0, 0), flags::ON));
    }

    #[test]
    fn behavior_template_shape() {
        // 16 bytes (every other length dies with 0x80041008), sel = 1
        // broadcast up front, zero pad behind.
        assert_eq!(BEHAVIOR_STATIC_GLOBAL.len(), 16);
        assert_eq!(&BEHAVIOR_STATIC_GLOBAL[0..2], &[0x01, 0x00]);
        assert!(BEHAVIOR_STATIC_GLOBAL[8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn behavior_sel_patch_matches_front_capture() {
        // Second Frida capture: PredatorSense changing only the front
        // ("zone 1" in its UI) red -> teal sends the global template
        // byte-identical except sel = [04 00].
        let front = behavior_static_for(area::FRONT);
        let mut expect = BEHAVIOR_STATIC_GLOBAL;
        expect[0..2].copy_from_slice(&[0x04, 0x00]);
        assert_eq!(front, expect);
    }

    #[test]
    fn pack_setting_reproduces_front_capture() {
        // Same capture's color word: sel 4 + teal (0, 174, 199) + ON + X.
        assert_eq!(
            pack_setting(area::FRONT, Rgb(0x00, 0xAE, 0xC7), flags::ON),
            0x000309C7AE000004
        );
    }

    /// Proves the per-area transaction on real hardware: front static blue
    /// (unmistakably ours), then read-back on sel 4. From any PredatorSense
    /// static state; restore via PredatorSense afterwards. FRONT is the only
    /// capture-backed area — TOP/REAR/AUX wait for their own captures.
    ///
    /// `#[ignore]` by default. Run ELEVATED:
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     dt::tests::hw_apply_front_blue --nocapture
    /// ```
    #[test]
    #[ignore = "writes real hardware; run explicitly and elevated"]
    fn hw_apply_front_blue() {
        let wmi = Wmi::connect().expect("elevated WMI connect");
        let blue = Rgb(0x00, 0x00, 0xFF);
        let status = apply_static(&wmi, area::FRONT, blue, true).expect("static front blue tx");
        eprintln!("tx status: 0x{status:X} — front should be BLUE now, rest untouched");
        let (back, fl) = get_color(&wmi, area::FRONT).expect("read back");
        assert_eq!((back, fl), (blue, flags::ON));
        eprintln!("read-back OK: front blue + ON. Restore via PredatorSense.");
    }
}
