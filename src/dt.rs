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
//! * `X` = per-effect trailer, `0x02` or `0x03`. It is **not** simply "uses
//!   RGB": Snake sends black with `0x03`, Stack sends RGB with `0x02`. Treated
//!   as an opaque captured parameter, replayed per effect from
//!   [`DtEffect::spec`].
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
//! `[sel_lo][sel_hi][01][EFFECT][0f][SPEED][03][TAIL]` + 8 zero pad. Two
//! bytes move across a capture of every effect in PredatorSense's menu
//! (`effects.md` + `effects2.md`, 2026-09-19, global scope): byte 3 (effect
//! id) and byte 7 (`0x01` for Snake and Stack, else `0x00`). The color word
//! then varies in whether PS sends the picked RGB or black, and in the
//! trailer — independently:
//!
//! | id | PS (FR) | EN | byte 7 | color word |
//! |---|---|---|---|---|
//! | `0x00` | Statique | Static | 00 | RGB · 03 |
//! | `0x01` | Clignotement | Flashing | 00 | RGB · 03 |
//! | `0x02` | Battement | Breathing | 00 | RGB · 03 |
//! | `0x03` | Scintillement | Twinkling | 00 | RGB · 03 |
//! | `0x06` | Arc-en-ciel | Rainbow | 00 | black · 02 |
//! | `0x09` | Onde | Wave | 00 | RGB · 03 |
//! | `0x0a` | Levée | Rising | 00 | black · 02 |
//! | `0x0b` | Pile | Stack | **01** | RGB · **02** |
//! | `0x0c` | Extension | Expansion | 00 | black · 02 |
//! | `0x0d` | Météorite | Meteor | 00 | black · 02 |
//! | `0x0e` | Magique | Magic | 00 | black · 02 |
//! | `0x0f` | Serpent | Snake | **01** | black · **03** |
//!
//! Byte 5 is the **speed slider** (`speed.txt`, 2026-09-19: dragging PS's
//! slider under Snake, global scope, walks byte 5 through
//! `01 02 03 04 05 06 07 08 0a` with every other byte — and the color word —
//! unchanged). The effect captures were all made at PS's default position,
//! `0x03`. Bytes 4 and 6 (`0f`, `03`) never moved in any capture and stay
//! opaque. Each effect's (id, byte 7, trailer, RGB-or-black) tuple is
//! replayed exactly as captured — see [`DtEffect::spec`].
//!
//! The slider capture is Snake-only; the speed byte is applied to every
//! animated effect on the strength of the shared layout, and pinned to the
//! captured default for Static (which has nothing to animate). Position 9
//! was skipped in the drag and is interpolated between captured 8 and 10.
//!
//! ## What we deliberately do NOT do
//!
//! * No guessed behavior writes: non-captured 16-byte payloads **stall the
//!   lighting controller** (LEDs freeze until PredatorSense re-applies). Only
//!   the effect ids in the table above are ever written.
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

/// Firmware effect for a DT zone — PredatorSense's full menu, every entry
/// captured (Frida, global scope) and replayed byte-exact. English names
/// follow Acer's; the French labels the capture was made under are in the
/// module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DtEffect {
    Static,
    /// Clignotement.
    Flashing,
    /// Battement.
    Breathing,
    /// Scintillement.
    Twinkling,
    /// Arc-en-ciel — built-in palette, color ignored.
    Rainbow,
    /// Onde.
    Wave,
    /// Levée — built-in palette, color ignored.
    Rising,
    /// Pile.
    Stack,
    /// Extension — built-in palette, color ignored.
    Expansion,
    /// Météorite — built-in palette, color ignored.
    Meteor,
    /// Magique — built-in palette, color ignored.
    Magic,
    /// Serpent — built-in palette, color ignored.
    Snake,
}

/// The captured wire parameters of one effect (see the module table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectSpec {
    /// Behavior byte 3.
    pub id: u8,
    /// Behavior byte 7 — `0x01` for Snake / Stack, else `0x00`. Meaning
    /// unknown; replayed verbatim.
    pub tail: u8,
    /// Color-word byte 6 (`0x02` / `0x03`). Meaning unknown; replayed verbatim.
    pub trailer: u8,
    /// Whether PredatorSense sends the picked RGB (else black).
    pub takes_color: bool,
}

impl Default for DtEffect {
    fn default() -> Self {
        Self::Static
    }
}

impl DtEffect {
    pub fn label(self) -> &'static str {
        match self {
            Self::Static => "Static",
            Self::Flashing => "Flashing",
            Self::Breathing => "Breathing",
            Self::Twinkling => "Twinkling",
            Self::Rainbow => "Rainbow",
            Self::Wave => "Wave",
            Self::Rising => "Rising",
            Self::Stack => "Stack",
            Self::Expansion => "Expansion",
            Self::Meteor => "Meteor",
            Self::Magic => "Magic",
            Self::Snake => "Snake",
        }
    }

    /// The captured parameters for this effect — the module table as code.
    pub fn spec(self) -> EffectSpec {
        const fn s(id: u8, tail: u8, trailer: u8, takes_color: bool) -> EffectSpec {
            EffectSpec { id, tail, trailer, takes_color }
        }
        match self {
            Self::Static => s(effect::STATIC, 0x00, TRAILER_COLOR, true),
            Self::Flashing => s(effect::FLASHING, 0x00, TRAILER_COLOR, true),
            Self::Breathing => s(effect::BREATHING, 0x00, TRAILER_COLOR, true),
            Self::Twinkling => s(effect::TWINKLING, 0x00, TRAILER_COLOR, true),
            Self::Rainbow => s(effect::RAINBOW, 0x00, TRAILER_PALETTE, false),
            Self::Wave => s(effect::WAVE, 0x00, TRAILER_COLOR, true),
            Self::Rising => s(effect::RISING, 0x00, TRAILER_PALETTE, false),
            Self::Stack => s(effect::STACK, 0x01, TRAILER_PALETTE, true),
            Self::Expansion => s(effect::EXPANSION, 0x00, TRAILER_PALETTE, false),
            Self::Meteor => s(effect::METEOR, 0x00, TRAILER_PALETTE, false),
            Self::Magic => s(effect::MAGIC, 0x00, TRAILER_PALETTE, false),
            Self::Snake => s(effect::SNAKE, 0x01, TRAILER_COLOR, false),
        }
    }

    /// Behavior byte 3 — straight from the capture table.
    pub fn id(self) -> u8 {
        self.spec().id
    }

    /// Whether PredatorSense sends the picked color for this effect. When
    /// false the RGB bytes go out as black, exactly as captured.
    pub fn takes_color(self) -> bool {
        self.spec().takes_color
    }

    /// Whether the speed byte means anything for this effect. Static has
    /// nothing to animate, so it always ships the captured default and the UI
    /// hides the slider; every other entry animates.
    pub fn has_speed(self) -> bool {
        self != Self::Static
    }

    /// The speed byte that actually goes on the wire for this effect: the
    /// request clamped into [`speed`]'s range, or the captured default when
    /// the effect has no speed. The single normalisation point — the engine
    /// diffs on it and [`behavior_for`] writes it, so they can't drift.
    pub fn wire_speed(self, speed: u8) -> u8 {
        if self.has_speed() {
            speed::clamp(speed)
        } else {
            speed::DEFAULT
        }
    }

    /// `true` when a Frida capture backs this effect's behavior bytes for
    /// `area`. All twelve are captured at global scope (`area::ALL`). Static is
    /// additionally proven per-area (front capture, patched selector). Any
    /// other effect with a patched selector is a byte combination
    /// PredatorSense has never been observed sending — uncaptured behavior
    /// payloads stall the controller, so it's refused until captured.
    pub fn is_supported_for(self, area: u16) -> bool {
        area == area::ALL || self == Self::Static
    }

    /// Whether the effect is captured at all (global scope). Today: every one.
    pub fn is_supported(self) -> bool {
        true
    }

    pub fn all() -> [Self; 12] {
        [
            Self::Static,
            Self::Breathing,
            Self::Flashing,
            Self::Twinkling,
            Self::Wave,
            Self::Stack,
            Self::Rainbow,
            Self::Rising,
            Self::Expansion,
            Self::Meteor,
            Self::Magic,
            Self::Snake,
        ]
    }
}

/// Global static case state (v1 scope: the whole case minus RAM, one color).
/// `None` in [`crate::engine::EngineState`] means "don't touch the case".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DtState {
    pub color: Rgb,
    pub on: bool,
    #[serde(default)]
    pub effect: DtEffect,
    /// Animation speed, [`speed::MIN`]`..=`[`speed::MAX`] (behavior byte 5).
    /// Whole-case only: per-area writes are Static-only, and Static has no
    /// speed.
    #[serde(default = "speed::default")]
    pub speed: u8,
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
    #[serde(default)]
    pub effect: DtEffect,
}

impl Default for DtState {
    fn default() -> Self {
        Self {
            color: Rgb(0x00, 0xE5, 0xFF),
            on: true,
            effect: DtEffect::Static,
            speed: speed::DEFAULT,
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

/// Speed slider — behavior byte 5. Captured positions (`speed.txt`, Snake,
/// global): 1–8 and 10; 9 is interpolated. Every effect capture was taken at
/// [`DEFAULT`](speed::DEFAULT).
pub mod speed {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 10;
    /// PredatorSense's slider position during every effect capture.
    pub const DEFAULT: u8 = 3;

    /// serde default hook.
    pub fn default() -> u8 {
        DEFAULT
    }

    /// Clamp into the captured range.
    pub fn clamp(v: u8) -> u8 {
        v.clamp(MIN, MAX)
    }
}

/// Flags byte values observed in the color register.
pub mod flags {
    /// Output on (static/breathing/wave/rainbow states).
    pub const ON: u8 = 0x09;
    /// Output off (PredatorSense "OFF" flips only this byte, color kept).
    pub const OFF: u8 = 0x01;
}

/// Color-word trailer (byte 6) seen with static and most color effects.
/// Named for the first pattern observed; Snake pairs it with black, so it is
/// not strictly "color mode" — an opaque per-effect value ([`EffectSpec`]).
pub const TRAILER_COLOR: u8 = 0x03;
/// The other observed trailer (Rainbow, Rising, Meteor, Expansion, Magic —
/// and Stack, which sends RGB with it).
pub const TRAILER_PALETTE: u8 = 0x02;

/// Captured `SetGamingLedBehavior` template for static-global (PredatorSense
/// static color change, global scope). Bytes 0-1 are the sel u16, byte 5 is
/// the speed slider at its default; the rest is replayed verbatim. Only
/// sel = 1 is captured — do not patch other selectors in without a matching
/// capture.
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
    behavior_for(area, DtEffect::Static, speed::DEFAULT)
}

/// Behavior template for any captured effect: the static template with the
/// selector (bytes 0-1), the effect id (byte 3), the speed (byte 5) and the
/// tail (byte 7) patched in. Every effect capture is byte-identical to static
/// apart from those, so this reproduces each of them exactly (see tests).
/// `speed` is clamped to [`speed`]'s range and ignored for effects without
/// one ([`DtEffect::has_speed`]), which keep the captured default. Callers
/// gate on [`DtEffect::is_supported_for`] — non-static ids are captured for
/// `area::ALL` only.
pub fn behavior_for(area: u16, fx: DtEffect, speed: u8) -> [u8; 16] {
    let spec = fx.spec();
    let mut b = BEHAVIOR_STATIC_GLOBAL;
    b[0..2].copy_from_slice(&area.to_le_bytes());
    b[3] = spec.id;
    b[5] = fx.wire_speed(speed);
    b[7] = spec.tail;
    b
}

/// Effect ids — behavior byte 3, one per PredatorSense menu entry (captured
/// 2026-09-19). Written only through [`behavior_for`].
pub mod effect {
    pub const STATIC: u8 = 0x00;
    /// Clignotement.
    pub const FLASHING: u8 = 0x01;
    /// Battement.
    pub const BREATHING: u8 = 0x02;
    /// Scintillement.
    pub const TWINKLING: u8 = 0x03;
    /// Arc-en-ciel (palette).
    pub const RAINBOW: u8 = 0x06;
    /// Onde.
    pub const WAVE: u8 = 0x09;
    /// Levée (palette).
    pub const RISING: u8 = 0x0a;
    /// Pile.
    pub const STACK: u8 = 0x0b;
    /// Extension (palette).
    pub const EXPANSION: u8 = 0x0c;
    /// Météorite (palette).
    pub const METEOR: u8 = 0x0d;
    /// Magique (palette).
    pub const MAGIC: u8 = 0x0e;
    /// Serpent (palette).
    pub const SNAKE: u8 = 0x0f;
}

/// Pack a `SetGamingRgbSetting` word:
/// `[sel_lo][sel_hi][R][G][B][flags][X][00]`.
///
/// `flags` must come from [`flags`]; the trailer is [`TRAILER_COLOR`] (the
/// static shape). Effects go through [`pack_setting_for`].
pub fn pack_setting(area: u16, color: Rgb, flags: u8) -> u64 {
    pack_word(area, color, flags, TRAILER_COLOR)
}

/// The color word for an effect, exactly as PredatorSense sends it: the
/// picked RGB or black, and the effect's own trailer ([`EffectSpec`]).
pub fn pack_setting_for(area: u16, color: Rgb, on: bool, fx: DtEffect) -> u64 {
    let spec = fx.spec();
    let flags = if on { flags::ON } else { flags::OFF };
    let rgb = if spec.takes_color { color } else { Rgb(0, 0, 0) };
    pack_word(area, rgb, flags, spec.trailer)
}

fn pack_word(area: u16, color: Rgb, flags: u8, trailer: u8) -> u64 {
    (area as u64)
        | ((color.0 as u64) << 16)
        | ((color.1 as u64) << 24)
        | ((color.2 as u64) << 32)
        | ((flags as u64) << 40)
        | ((trailer as u64) << 48)
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
    apply_effect(wmi, area, color, on, DtEffect::Static, speed::DEFAULT)
}

/// Apply any captured effect to one area: behavior template (selector,
/// effect id and speed patched), ~60 ms, then the matching color word —
/// PredatorSense's exact sequence and timing. `color` is ignored by palette
/// effects (sent as black, like PredatorSense does); `speed` by Static.
pub fn apply_effect(
    wmi: &Wmi,
    area: u16,
    color: Rgb,
    on: bool,
    fx: DtEffect,
    speed: u8,
) -> Result<u64> {
    wmi.call_bytes("SetGamingLedBehavior", &behavior_for(area, fx, speed))?;
    std::thread::sleep(std::time::Duration::from_millis(60));
    wmi.call_packed("SetGamingRgbSetting", pack_setting_for(area, color, on, fx))
}

/// Read back an area's color register (selector echo reads zero — see
/// [`unpack_echo`]). `GetGamingRgbSetting` takes a `UInt32` selector (the
/// probe toolkit sweeps it as `[uint32]`), so this goes through the u32
/// reader, not the u64-as-string packer the setters use. Read-only.
///
/// Whether the echo is truly per-area (sel 4 → front's register) or always
/// the last-written register is not yet confirmed on hardware; the engine's
/// hold re-assert self-calibrates for that (see `engine::DtTracker`).
pub fn get_color(wmi: &Wmi, area: u16) -> Result<(Rgb, u8)> {
    let word = wmi.call_u32("GetGamingRgbSetting", area as u32)?;
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

    /// `effects.md` (2026-09-19): every PredatorSense effect, global scope.
    /// Behavior differs from static only in byte 3; the color word is either
    /// the RGB shape (`…03 09 RR GG BB 00 01`) or black + `0x02`.
    #[test]
    fn effects_reproduce_predatorsense_captures() {
        let hex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };
        let cases: [(DtEffect, &str, u64); 7] = [
            (DtEffect::Rising, "0100010a0f0303000000000000000000", 0x0002090000000001),
            (DtEffect::Rainbow, "010001060f0303000000000000000000", 0x0002090000000001),
            (DtEffect::Meteor, "0100010d0f0303000000000000000000", 0x0002090000000001),
            (DtEffect::Breathing, "010001020f0303000000000000000000", 0x000309ffe85f0001),
            (DtEffect::Wave, "010001090f0303000000000000000000", 0x000309ffe85f0001),
            (DtEffect::Flashing, "010001010f0303000000000000000000", 0x000309ffe85f0001),
            (DtEffect::Twinkling, "010001030f0303000000000000000000", 0x000309ffe85f0001),
        ];
        // The color PredatorSense had selected during the capture.
        let picked = Rgb(0x5f, 0xe8, 0xff);
        for (fx, behavior, word) in cases {
            assert_eq!(
                behavior_for(area::ALL, fx, speed::DEFAULT).to_vec(),
                hex(behavior),
                "{fx:?} behavior"
            );
            assert_eq!(pack_setting_for(area::ALL, picked, true, fx), word, "{fx:?} color word");
        }
        // `effects2.md`: the four remaining effects. Byte 7 of the behavior
        // moves for Snake/Stack, and trailer vs RGB-or-black vary
        // independently — Snake is black + 0x03, Stack is RGB + 0x02.
        let cases2: [(DtEffect, &str, u64); 4] = [
            (DtEffect::Snake, "0100010f0f0303010000000000000000", 0x0003090000000001),
            (DtEffect::Stack, "0100010b0f0303010000000000000000", 0x000209ffe85f0001),
            (DtEffect::Expansion, "0100010c0f0303000000000000000000", 0x0002090000000001),
            (DtEffect::Magic, "0100010e0f0303000000000000000000", 0x0002090000000001),
        ];
        for (fx, behavior, word) in cases2 {
            assert_eq!(
                behavior_for(area::ALL, fx, speed::DEFAULT).to_vec(),
                hex(behavior),
                "{fx:?} behavior"
            );
            assert_eq!(pack_setting_for(area::ALL, picked, true, fx), word, "{fx:?} color word");
        }
        // Static stays the original shape.
        assert_eq!(
            behavior_for(area::ALL, DtEffect::Static, speed::DEFAULT),
            BEHAVIOR_STATIC_GLOBAL
        );
        assert_eq!(
            pack_setting_for(area::ALL, Rgb(0x3C, 0xF0, 0x3C), true, DtEffect::Static),
            0x0003093CF03C0001
        );
    }

    /// `speed.txt` (2026-09-19): PredatorSense's speed slider dragged under
    /// Snake, global scope. Only byte 5 moves; the color word is constant.
    #[test]
    fn speed_reproduces_predatorsense_slider_capture() {
        let captured: [u8; 9] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0a];
        for sp in captured {
            let expect = [
                0x01, 0x00, 0x01, 0x0f, 0x0f, sp, 0x03, 0x01, //
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ];
            assert_eq!(behavior_for(area::ALL, DtEffect::Snake, sp), expect, "speed {sp}");
        }
        // (The color word stays 0x0003090000000001 throughout — covered by
        // `effects_reproduce_predatorsense_captures`.)
        // Out-of-range values clamp into the slider's range.
        assert_eq!(behavior_for(area::ALL, DtEffect::Wave, 0)[5], speed::MIN);
        assert_eq!(behavior_for(area::ALL, DtEffect::Wave, 0xff)[5], speed::MAX);
        // Static has no speed: whatever is asked, it ships the captured
        // default and stays byte-identical to the static capture.
        assert_eq!(behavior_for(area::ALL, DtEffect::Static, 9), BEHAVIOR_STATIC_GLOBAL);
        assert_eq!(
            behavior_for(area::FRONT, DtEffect::Static, 1),
            behavior_static_for(area::FRONT)
        );
        assert!(!DtEffect::Static.has_speed());
        assert_eq!(DtEffect::all().iter().filter(|f| f.has_speed()).count(), 11);
    }

    #[test]
    fn palette_effects_ignore_color_and_off_flips_only_flags() {
        // Whatever the user picked, a palette effect ships black + 0x02.
        assert_eq!(
            pack_setting_for(area::ALL, Rgb(0xFF, 0x00, 0x88), true, DtEffect::Rainbow),
            0x0002090000000001
        );
        // OFF keeps the shape and flips only the flags byte (0x09 -> 0x01).
        assert_eq!(
            pack_setting_for(area::ALL, Rgb(0x5f, 0xe8, 0xff), false, DtEffect::Wave),
            0x000301ffe85f0001
        );
        assert_eq!(
            pack_setting_for(area::ALL, Rgb(0, 0, 0), false, DtEffect::Meteor),
            0x0002010000000001
        );
    }

    #[test]
    fn effect_ids_are_unique_and_round_trip() {
        let ids: Vec<u8> = DtEffect::all().iter().map(|f| f.id()).collect();
        let mut dedup = ids.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(ids.len(), dedup.len(), "duplicate effect id");
        for fx in DtEffect::all() {
            let b = behavior_for(area::FRONT, fx, speed::DEFAULT);
            assert_eq!(b[3], fx.id());
            assert_eq!(b[7], fx.spec().tail);
            assert_eq!(&b[0..2], &[0x04, 0x00]);
            // At default speed only bytes 0-1, 3 and 7 differ from the
            // static template.
            assert_eq!(&b[4..7], &BEHAVIOR_STATIC_GLOBAL[4..7]);
            assert!(b[8..].iter().all(|&x| x == 0));
        }
        assert_eq!(DtEffect::all().len(), 12);
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
