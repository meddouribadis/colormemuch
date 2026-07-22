//! colormemuch's own software effects — animations the firmware doesn't offer.
//!
//! The keyboard exposes only 4 addressable zones, so these are painterly rather
//! than pixel-dense: they treat the zones as a short 1-D strip and animate color
//! across it. Each effect is a **pure function of time**:
//!
//! ```text
//! fn(t_seconds: f32, led_count: usize) -> Vec<Rgb>
//! ```
//!
//! Purity is deliberate — an effect has no hidden state, so it's trivial to
//! test, scrub, preview at any instant, or render at any frame rate. The
//! [`play`] loop just samples the function and pushes Direct-mode frames.

#![cfg(windows)]
#![allow(dead_code)]

use std::f32::consts::PI;

use serde::{Deserialize, Serialize};

use crate::rgb::Rgb;

/// The catalogue of built-in software effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Effect {
    /// A hue that rotates over time, spread across the strip.
    Rainbow,
    /// A bright head that sweeps back and forth with a fading tail.
    Comet,
    /// The whole strip breathes one color via a smooth brightness sine.
    Breathe,
    /// Per-zone flicker in warm tones.
    Fire,
    /// A moving gradient between two colors.
    Gradient,
    /// Alternating red/blue flash.
    Police,
    /// A soft sine of brightness travelling along the strip.
    Wave,
}

impl Effect {
    pub const ALL: [Effect; 7] = [
        Effect::Rainbow,
        Effect::Comet,
        Effect::Breathe,
        Effect::Fire,
        Effect::Gradient,
        Effect::Police,
        Effect::Wave,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Effect::Rainbow => "Rainbow",
            Effect::Comet => "Comet",
            Effect::Breathe => "Breathe",
            Effect::Fire => "Fire",
            Effect::Gradient => "Gradient",
            Effect::Police => "Police",
            Effect::Wave => "Wave",
        }
    }
}

/// Parameters shared by the effects. `color`/`color_b` are used by the effects
/// that key off a chosen color; `speed` scales time (1.0 = nominal).
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub color: Rgb,
    pub color_b: Rgb,
    pub speed: f32,
    pub brightness: f32, // 0.0..=1.0
}

impl Default for Params {
    fn default() -> Self {
        Self {
            color: Rgb(0x00, 0xE5, 0xFF), // cyan, the app accent
            color_b: Rgb(0xFF, 0x00, 0x88),
            speed: 1.0,
            brightness: 1.0,
        }
    }
}

/// Sample an effect at time `t` (seconds) for `n` LEDs.
pub fn render(effect: Effect, p: Params, t: f32, n: usize) -> Vec<Rgb> {
    let t = t * p.speed;
    let raw = match effect {
        Effect::Rainbow => rainbow(t, n),
        Effect::Comet => comet(t, n, p.color),
        Effect::Breathe => breathe(t, n, p.color),
        Effect::Fire => fire(t, n),
        Effect::Gradient => gradient(t, n, p.color, p.color_b),
        Effect::Police => police(t, n),
        Effect::Wave => wave(t, n, p.color),
    };
    raw.into_iter().map(|c| scale(c, p.brightness)).collect()
}

// --- the effects ----------------------------------------------------------

fn rainbow(t: f32, n: usize) -> Vec<Rgb> {
    (0..n)
        .map(|i| {
            let hue = (t * 0.15 + i as f32 / n.max(1) as f32) % 1.0;
            hsv(hue, 1.0, 1.0)
        })
        .collect()
}

fn comet(t: f32, n: usize, color: Rgb) -> Vec<Rgb> {
    // Head bounces along [0, n-1] via a triangle wave; tail falls off behind it.
    let span = (n.max(1) - 1).max(1) as f32;
    let phase = (t * 0.9) % 2.0;
    let head = if phase < 1.0 { phase } else { 2.0 - phase } * span;
    (0..n)
        .map(|i| {
            let d = (i as f32 - head).abs();
            let fall = (1.0 - d / 1.6).max(0.0);
            scale(color, fall * fall)
        })
        .collect()
}

fn breathe(t: f32, n: usize, color: Rgb) -> Vec<Rgb> {
    // Sine in [0,1]; a soft floor so it never fully blacks out.
    let b = 0.15 + 0.85 * (0.5 - 0.5 * (t * 1.3 * PI).cos());
    vec![scale(color, b); n]
}

fn fire(t: f32, n: usize) -> Vec<Rgb> {
    (0..n)
        .map(|i| {
            // Cheap value noise from a couple of detuned sines per zone.
            let s = hash01(i as f32 * 12.9 + (t * 6.0).floor());
            let flicker = 0.55 + 0.45 * ((t * 9.0 + i as f32 * 1.7).sin() * 0.5 + 0.5) * s;
            // Warm ramp: red full, green tracks heat, almost no blue.
            let g = (0.35 * flicker * 255.0) as u8;
            scale(Rgb(255, g, 0), flicker)
        })
        .collect()
}

fn gradient(t: f32, n: usize, a: Rgb, b: Rgb) -> Vec<Rgb> {
    (0..n)
        .map(|i| {
            let x = i as f32 / n.max(1) as f32;
            // Slide the gradient origin over time.
            let m = 0.5 - 0.5 * ((x + t * 0.2) * 2.0 * PI).cos();
            lerp(a, b, m)
        })
        .collect()
}

fn police(t: f32, n: usize) -> Vec<Rgb> {
    let on_red = ((t * 4.0) as i32) % 2 == 0;
    (0..n)
        .map(|i| {
            let left = i < n / 2;
            let lit = if on_red { left } else { !left };
            if !lit {
                Rgb(0, 0, 0)
            } else if on_red {
                Rgb(255, 0, 0)
            } else {
                Rgb(0, 0, 255)
            }
        })
        .collect()
}

fn wave(t: f32, n: usize, color: Rgb) -> Vec<Rgb> {
    (0..n)
        .map(|i| {
            let phase = i as f32 / n.max(1) as f32 * 2.0 * PI;
            let b = 0.2 + 0.8 * (0.5 + 0.5 * (t * 2.2 - phase).sin());
            scale(color, b)
        })
        .collect()
}

// --- playback -------------------------------------------------------------

use crate::openrgb::{Controller, OpenRgb};

/// Animate an effect on a controller for `secs` seconds at `fps` frames/sec by
/// sampling [`render`] and pushing Direct-mode frames. Blocking; returns when
/// the duration elapses or a socket write fails.
pub fn play(
    client: &mut OpenRgb,
    ctrl: &Controller,
    effect: Effect,
    params: Params,
    secs: f32,
    fps: u32,
) -> std::io::Result<()> {
    let n = ctrl.led_count as usize;
    let frame_dt = std::time::Duration::from_secs_f32(1.0 / fps.max(1) as f32);
    let start = std::time::Instant::now();
    loop {
        let t = start.elapsed().as_secs_f32();
        if t >= secs {
            break;
        }
        let frame = render(effect, params, t, n);
        client.set_leds(ctrl, &frame)?;
        std::thread::sleep(frame_dt);
    }
    Ok(())
}

// --- per-zone sources on a shared clock -----------------------------------

/// An animated per-zone source — a built-in **Program** effect or a user
/// **Custom** one. Both render the same `(t, n) -> frame` shape, so the engine
/// handles them uniformly.
#[derive(Debug, Clone)]
pub enum Fx {
    Program(Effect),
    Custom(crate::library::CustomEffect),
}

impl Fx {
    pub fn sample(&self, p: Params, t: f32, n: usize) -> Vec<Rgb> {
        match self {
            Fx::Program(e) => render(*e, p, t, n),
            Fx::Custom(c) => c.sample(t, n, p.speed, p.brightness),
        }
    }
    /// Identity for shared-clock grouping — same key ⇒ same clock group.
    pub fn key(&self) -> String {
        match self {
            Fx::Program(e) => format!("p:{}", e.label()),
            Fx::Custom(c) => format!("c:{}", c.name),
        }
    }
    pub fn label(&self) -> String {
        match self {
            Fx::Program(e) => e.label().to_string(),
            Fx::Custom(c) => c.name.clone(),
        }
    }
    pub fn hue(&self) -> Rgb {
        match self {
            Fx::Program(e) => program_hue(*e),
            Fx::Custom(c) => c.dominant(),
        }
    }
}

/// Program-effect hue for badges (matches the mockup design tokens).
pub fn program_hue(e: Effect) -> Rgb {
    match e {
        Effect::Comet => Rgb(0x1f, 0xb7, 0xa6),
        Effect::Fire => Rgb(0xff, 0x95, 0x00),
        Effect::Rainbow => Rgb(0xd8, 0x4b, 0xff),
        Effect::Breathe => Rgb(0x5e, 0x5c, 0xe6),
        Effect::Wave => Rgb(0x34, 0xc0, 0xff),
        Effect::Police => Rgb(0xff, 0x3b, 0x4e),
        Effect::Gradient => Rgb(0x4b, 0xbf, 0x73),
    }
}

/// What drives a single zone: a fixed color, or an animated function.
#[derive(Debug, Clone)]
pub enum ZoneSource {
    Solid(Rgb),
    Function(Fx, Params),
}

impl ZoneSource {
    pub fn fx(&self) -> Option<&Fx> {
        match self {
            ZoneSource::Solid(_) => None,
            ZoneSource::Function(f, _) => Some(f),
        }
    }
    pub fn params(&self) -> Option<Params> {
        match self {
            ZoneSource::Solid(_) => None,
            ZoneSource::Function(_, p) => Some(*p),
        }
    }
}

/// How a shared-clock group of zones reads its one clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMode {
    /// Every ganged zone shows the identical color — a unified block.
    Link,
    /// The effect travels across the ganged zones (each zone is one cell of a
    /// group-length strip), still off the one shared clock.
    Spread,
}

/// Sample one zone (a single LED) at shared-clock time `t`.
pub fn render_zone(src: &ZoneSource, t: f32) -> Rgb {
    match src {
        ZoneSource::Solid(c) => *c,
        ZoneSource::Function(f, p) => f.sample(*p, t, 1)[0],
    }
}

/// Render a whole multi-zone frame from ONE shared clock value `t`.
///
/// The shared clock is the whole point: every zone samples the *same* `t`, so
/// two zones running the same function are phase-locked — they move as one, no
/// drift, regardless of when each was assigned. Give a zone a different effect
/// (or Solid) and it simply reads the same clock independently. There are no
/// per-zone timers to fall out of sync.
pub fn render_zones(zones: &[ZoneSource], t: f32) -> Vec<Rgb> {
    zones.iter().map(|z| render_zone(z, t)).collect()
}

/// A shared-clock group: ≥2 zones running the same function, phase-locked.
#[derive(Debug, Clone)]
pub struct Group {
    /// Stable identity (`Fx::key`) — also the key used in the Spread set.
    pub key: String,
    pub label: String,
    pub hue: Rgb,
    pub zones: Vec<usize>,
}

/// Which zone indices are ganged to a shared clock. Handy for the UI's "link"
/// affordance and for [`render_plan`]'s Link/Spread decision.
pub fn shared_clock_groups(zones: &[ZoneSource]) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for (i, z) in zones.iter().enumerate() {
        if let Some(f) = z.fx() {
            let key = f.key();
            match out.iter_mut().find(|g| g.key == key) {
                Some(g) => g.zones.push(i),
                None => out.push(Group {
                    key,
                    label: f.label(),
                    hue: f.hue(),
                    zones: vec![i],
                }),
            }
        }
    }
    out.retain(|g| g.zones.len() >= 2);
    out
}

/// Animate independently-assigned zones off one shared clock. Each zone can run
/// its own source; same-function zones stay phase-locked because they all read
/// this loop's single `t`.
pub fn play_zones(
    client: &mut OpenRgb,
    ctrl: &Controller,
    zones: &[ZoneSource],
    secs: f32,
    fps: u32,
) -> std::io::Result<()> {
    let frame_dt = std::time::Duration::from_secs_f32(1.0 / fps.max(1) as f32);
    let start = std::time::Instant::now();
    loop {
        let t = start.elapsed().as_secs_f32();
        if t >= secs {
            break;
        }
        let frame = render_zones(zones, t);
        client.set_leds(ctrl, &frame)?;
        std::thread::sleep(frame_dt);
    }
    Ok(())
}

/// Render zones with per-effect Link/Spread control.
///
/// Effects listed in `spread` render as one animation travelling across their
/// ganged zones (each zone is a cell of a group-length strip); all others Link
/// (every ganged zone identical). Everything still reads the single clock `t`,
/// so no mode ever drifts. `spread` naming an effect that isn't ganged is a
/// harmless no-op.
pub fn render_plan(
    sources: &[ZoneSource],
    spread: &std::collections::HashSet<String>,
    t: f32,
) -> Vec<Rgb> {
    // Base: solids and singleton effects rendered independently; same-function
    // Link groups already coincide when params match — the loop makes Link
    // exact regardless of params, and overrides Spread groups.
    let mut out = render_zones(sources, t);
    for g in shared_clock_groups(sources) {
        let i0 = g.zones[0];
        let Some(fx) = sources[i0].fx() else { continue };
        let p = sources[i0].params().unwrap_or_default();
        if spread.contains(&g.key) {
            let strip = fx.sample(p, t, g.zones.len());
            for (pos, &zi) in g.zones.iter().enumerate() {
                out[zi] = strip[pos];
            }
        } else {
            let c = fx.sample(p, t, 1)[0];
            for &zi in &g.zones {
                out[zi] = c;
            }
        }
    }
    out
}

/// Like [`play_zones`] but honoring per-effect Link/Spread.
pub fn play_plan(
    client: &mut OpenRgb,
    ctrl: &Controller,
    sources: &[ZoneSource],
    spread: &std::collections::HashSet<String>,
    secs: f32,
    fps: u32,
) -> std::io::Result<()> {
    let frame_dt = std::time::Duration::from_secs_f32(1.0 / fps.max(1) as f32);
    let start = std::time::Instant::now();
    loop {
        let t = start.elapsed().as_secs_f32();
        if t >= secs {
            break;
        }
        let frame = render_plan(sources, spread, t);
        client.set_leds(ctrl, &frame)?;
        std::thread::sleep(frame_dt);
    }
    Ok(())
}

// --- color helpers --------------------------------------------------------

/// Scale an RGB by a 0..=1 factor (gamma-naive, good enough for LEDs).
pub fn scale(c: Rgb, f: f32) -> Rgb {
    let f = f.clamp(0.0, 1.0);
    Rgb(
        (c.0 as f32 * f) as u8,
        (c.1 as f32 * f) as u8,
        (c.2 as f32 * f) as u8,
    )
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// HSV → RGB with h, s, v in 0..=1.
pub fn hsv(h: f32, s: f32, v: f32) -> Rgb {
    let h = (h.rem_euclid(1.0)) * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Deterministic pseudo-random in [0,1) from a float seed — no rng crate, and
/// `Math.random`-free so effects stay reproducible for a given time.
fn hash01(x: f32) -> f32 {
    let s = (x.sin() * 43758.547).fract();
    s.abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_are_pure_and_sized() {
        // Every effect returns exactly n colors and is stable for a given t.
        for e in Effect::ALL {
            let a = render(e, Params::default(), 1.234, 4);
            let b = render(e, Params::default(), 1.234, 4);
            assert_eq!(a.len(), 4, "{} wrong length", e.label());
            assert_eq!(a, b, "{} not pure", e.label());
        }
    }

    #[test]
    fn brightness_scales_output() {
        let full = render(Effect::Breathe, Params { brightness: 1.0, ..Default::default() }, 0.0, 1);
        let dim = render(Effect::Breathe, Params { brightness: 0.25, ..Default::default() }, 0.0, 1);
        assert!(dim[0].0 <= full[0].0 && dim[0].1 <= full[0].1 && dim[0].2 <= full[0].2);
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), Rgb(255, 0, 0));
        assert_eq!(hsv(1.0 / 3.0, 1.0, 1.0), Rgb(0, 255, 0));
        assert_eq!(hsv(2.0 / 3.0, 1.0, 1.0), Rgb(0, 0, 255));
    }

    fn prog(e: Effect) -> Fx {
        Fx::Program(e)
    }

    #[test]
    fn same_function_zones_share_one_clock() {
        // Zones 0 and 2 both run Rainbow: on the shared clock they must be
        // byte-identical at every instant (phase-locked). Zone 1 differs.
        let p = Params::default();
        let zones = [
            ZoneSource::Function(prog(Effect::Rainbow), p),
            ZoneSource::Function(prog(Effect::Comet), p),
            ZoneSource::Function(prog(Effect::Rainbow), p),
            ZoneSource::Solid(Rgb(10, 20, 30)),
        ];
        for &t in &[0.0f32, 0.37, 1.9, 5.5] {
            let f = render_zones(&zones, t);
            assert_eq!(f[0], f[2], "same-effect zones must be phase-locked at t={t}");
        }
        assert_eq!(render_zone(&zones[3], 99.0), Rgb(10, 20, 30));

        let groups = shared_clock_groups(&zones);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "Rainbow");
        assert_eq!(groups[0].key, "p:Rainbow");
        assert_eq!(groups[0].zones, vec![0, 2]);
    }

    #[test]
    fn link_vs_spread() {
        use std::collections::HashSet;
        let p = Params::default();
        let zones = [
            ZoneSource::Function(prog(Effect::Rainbow), p),
            ZoneSource::Function(prog(Effect::Rainbow), p),
            ZoneSource::Function(prog(Effect::Rainbow), p),
            ZoneSource::Function(prog(Effect::Rainbow), p),
        ];

        // Link (empty spread set): all four identical.
        let linked = render_plan(&zones, &HashSet::new(), 2.0);
        assert!(linked.iter().all(|&c| c == linked[0]), "Link must be uniform");

        // Spread: the group renders as a 4-cell rainbow, so not all equal.
        let spread: HashSet<String> = ["p:Rainbow".to_string()].into_iter().collect();
        let spread_frame = render_plan(&zones, &spread, 2.0);
        assert!(
            !spread_frame.iter().all(|&c| c == spread_frame[0]),
            "Spread must vary across zones"
        );
        // Still drift-free / deterministic.
        assert_eq!(spread_frame, render_plan(&zones, &spread, 2.0));
    }

    /// Live demo against the running OpenRGB server — plays a few software
    /// effects on the keyboard. No elevation. Ignored so CI skips it.
    ///
    /// ```text
    /// cargo test --bin colormemuch -- --ignored --exact \
    ///     effects::tests::hw_play_demo --nocapture
    /// ```
    #[test]
    #[ignore = "needs the local OpenRGB server; animates the keyboard"]
    fn hw_play_demo() {
        use crate::openrgb::OpenRgb;
        let mut c = OpenRgb::connect().expect("connect");
        let kb = c.find("keyboard").expect("query").expect("keyboard");
        for e in [Effect::Rainbow, Effect::Comet, Effect::Wave, Effect::Fire] {
            eprintln!("playing {}", e.label());
            play(&mut c, &kb, e, Params::default(), 4.0, 30).expect("play");
        }
    }

    /// Live demo of INDEPENDENT per-zone control on a shared clock: zones 0 & 2
    /// run Rainbow (phase-locked, identical), zone 1 runs Comet, zone 3 is a
    /// solid color. No elevation.
    ///
    /// ```text
    /// cargo test --bin colormemuch -- --ignored --exact \
    ///     effects::tests::hw_play_zones_demo --nocapture
    /// ```
    #[test]
    #[ignore = "needs the local OpenRGB server; animates the keyboard"]
    fn hw_play_zones_demo() {
        use crate::openrgb::OpenRgb;
        let mut c = OpenRgb::connect().expect("connect");
        let kb = c.find("keyboard").expect("query").expect("keyboard");
        let p = Params::default();
        let zones = [
            ZoneSource::Function(prog(Effect::Rainbow), p),
            ZoneSource::Function(prog(Effect::Comet), p),
            ZoneSource::Function(prog(Effect::Rainbow), p),
            ZoneSource::Solid(Rgb(0xFF, 0x30, 0x00)),
        ];
        eprintln!(
            "zones: [Rainbow, Comet, Rainbow, Solid] — 0&2 share a clock: {:?}",
            shared_clock_groups(&zones)
        );
        play_zones(&mut c, &kb, &zones, 10.0, 30).expect("play zones");
    }

    /// Live: all 4 zones on Rainbow, SPREAD — one rainbow travels across the
    /// keyboard on the shared clock. Then Link would make them a single block.
    ///
    /// ```text
    /// cargo test --bin colormemuch -- --ignored --exact \
    ///     effects::tests::hw_play_spread_demo --nocapture
    /// ```
    #[test]
    #[ignore = "needs the local OpenRGB server; animates the keyboard"]
    fn hw_play_spread_demo() {
        use crate::openrgb::OpenRgb;
        use std::collections::HashSet;
        let mut c = OpenRgb::connect().expect("connect");
        let kb = c.find("keyboard").expect("query").expect("keyboard");
        let zones = vec![ZoneSource::Function(prog(Effect::Rainbow), Params::default()); 4];

        let spread: HashSet<String> = ["p:Rainbow".to_string()].into_iter().collect();
        eprintln!("SPREAD: one rainbow travels across all 4 zones");
        play_plan(&mut c, &kb, &zones, &spread, 8.0, 30).expect("spread");

        eprintln!("LINK: all 4 zones move as one block");
        play_plan(&mut c, &kb, &zones, &HashSet::<String>::new(), 6.0, 30).expect("link");
    }
}
