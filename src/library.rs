//! User-editable **Custom** effects and their on-disk library.
//!
//! A custom effect is pure data — a cyclic color **palette** plus a **motion**
//! model — so it can be built and tuned in the app, saved to
//! `%APPDATA%/ColorMeMuch/effects.json`, and reloaded. It renders the same
//! `fn(t, n) -> Vec<Rgb>` shape as the built-in Program effects, so the engine
//! treats both uniformly.

#![cfg(windows)]

use std::f32::consts::PI;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::effects::scale;
use crate::rgb::Rgb;

/// A color anchored at a position along the palette (0..=1, cyclic).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColorStop {
    pub pos: f32,
    pub rgb: [u8; 3],
}

/// How a palette animates across the zones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Motion {
    /// Palette laid across the zones, still.
    Static,
    /// Palette scrolls along the strip.
    Scroll,
    /// Palette scrolls back and forth.
    Bounce,
    /// Whole strip one color, cycling the palette while it breathes.
    Pulse,
    /// Per-zone random sparkle drawn from the palette.
    Twinkle,
}

impl Motion {
    pub const ALL: [Motion; 5] = [
        Motion::Static,
        Motion::Scroll,
        Motion::Bounce,
        Motion::Pulse,
        Motion::Twinkle,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Motion::Static => "Static",
            Motion::Scroll => "Scroll",
            Motion::Bounce => "Bounce",
            Motion::Pulse => "Pulse",
            Motion::Twinkle => "Twinkle",
        }
    }
}

/// A fully user-defined effect. `speed`/`brightness` are its own baseline; the
/// per-zone sliders multiply them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomEffect {
    pub name: String,
    pub palette: Vec<ColorStop>,
    pub motion: Motion,
    pub speed: f32,      // baseline rate
    pub brightness: f32, // 0..=1
}

impl CustomEffect {
    /// A blank effect to start editing from (cyan→magenta scroll).
    pub fn new_default(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            palette: vec![
                ColorStop { pos: 0.0, rgb: [0x00, 0xE5, 0xFF] },
                ColorStop { pos: 1.0, rgb: [0xFF, 0x00, 0x88] },
            ],
            motion: Motion::Scroll,
            speed: 1.0,
            brightness: 1.0,
        }
    }

    /// Cyclic gradient lookup: sample the palette at `p` (any real; wrapped to
    /// 0..1). Stops are treated as a loop, so the last wraps back to the first.
    pub fn palette_at(&self, p: f32) -> Rgb {
        if self.palette.is_empty() {
            return Rgb(0, 0, 0);
        }
        if self.palette.len() == 1 {
            let c = self.palette[0].rgb;
            return Rgb(c[0], c[1], c[2]);
        }
        let p = p.rem_euclid(1.0);
        // Stops sorted by position, plus a wrap sentinel (first stop at +1.0).
        let mut stops: Vec<(f32, [u8; 3])> =
            self.palette.iter().map(|s| (s.pos.clamp(0.0, 1.0), s.rgb)).collect();
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let first = stops[0];
        stops.push((first.0 + 1.0, first.1));

        for w in stops.windows(2) {
            let (p0, c0) = w[0];
            let (p1, c1) = w[1];
            if p >= p0 && p <= p1 {
                let span = (p1 - p0).max(1e-4);
                return lerp_rgb(c0, c1, (p - p0) / span);
            }
            // Handle the wrap segment where p is below the first stop.
            if p1 > 1.0 && p + 1.0 >= p0 && p + 1.0 <= p1 {
                let span = (p1 - p0).max(1e-4);
                return lerp_rgb(c0, c1, (p + 1.0 - p0) / span);
            }
        }
        let c = first.1;
        Rgb(c[0], c[1], c[2])
    }

    /// Render `n` zones at time `t`, with external speed/brightness multipliers
    /// (the per-zone sliders).
    pub fn sample(&self, t: f32, n: usize, speed_mul: f32, bright_mul: f32) -> Vec<Rgb> {
        let sp = self.speed * speed_mul;
        let bmaster = (self.brightness * bright_mul).clamp(0.0, 1.0);
        (0..n)
            .map(|i| {
                let base = i as f32 / n.max(1) as f32;
                let (color, b) = match self.motion {
                    Motion::Static => (self.palette_at(base), 1.0),
                    Motion::Scroll => (self.palette_at(base + t * sp * 0.15), 1.0),
                    Motion::Bounce => {
                        let ph = triangle(t * sp * 0.15);
                        (self.palette_at(base + ph), 1.0)
                    }
                    Motion::Pulse => {
                        let color = self.palette_at(t * sp * 0.08);
                        let b = 0.15 + 0.85 * (0.5 - 0.5 * (t * sp * 1.4 * PI * 0.5).cos());
                        (color, b)
                    }
                    Motion::Twinkle => {
                        let step = (t * sp * 2.0).floor();
                        let pos = hash01(i as f32 * 7.1 + step);
                        let flick = 0.35 + 0.65 * hash01(i as f32 * 3.3 + step * 1.7);
                        (self.palette_at(pos), flick)
                    }
                };
                scale(color, b * bmaster)
            })
            .collect()
    }

    /// A representative color for badges / hue keying.
    pub fn dominant(&self) -> Rgb {
        self.palette
            .first()
            .map(|s| Rgb(s.rgb[0], s.rgb[1], s.rgb[2]))
            .unwrap_or(Rgb(0x88, 0x88, 0x88))
    }
}

/// The on-disk collection of custom effects.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EffectLibrary {
    pub effects: Vec<CustomEffect>,
}

impl EffectLibrary {
    fn dir() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join(crate::APP_NAME))
    }
    fn path() -> Option<PathBuf> {
        Self::dir().map(|p| p.join("effects.json"))
    }

    /// Load the library, seeding a few starter effects on first run.
    pub fn load() -> Self {
        let existing = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<EffectLibrary>(&s).ok());
        match existing {
            Some(lib) => lib,
            None => {
                let lib = Self { effects: seed() };
                lib.save();
                lib
            }
        }
    }

    pub fn save(&self) {
        let Some(dir) = Self::dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        if let Some(p) = Self::path() {
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&CustomEffect> {
        self.effects.iter().find(|e| e.name == name)
    }

    /// Insert or replace by name; returns the name for convenience.
    pub fn upsert(&mut self, e: CustomEffect) -> String {
        let name = e.name.clone();
        match self.effects.iter_mut().find(|x| x.name == name) {
            Some(slot) => *slot = e,
            None => self.effects.push(e),
        }
        self.save();
        name
    }

    pub fn remove(&mut self, name: &str) {
        self.effects.retain(|e| e.name != name);
        self.save();
    }

    /// A unique "Custom N" name not already taken.
    pub fn fresh_name(&self) -> String {
        for i in 1.. {
            let n = format!("Custom {i}");
            if self.get(&n).is_none() {
                return n;
            }
        }
        unreachable!()
    }
}

/// Starter effects so the library isn't empty on first run.
fn seed() -> Vec<CustomEffect> {
    vec![
        CustomEffect {
            name: "Sunset".into(),
            palette: vec![
                ColorStop { pos: 0.0, rgb: [0xFF, 0x6A, 0x00] },
                ColorStop { pos: 0.5, rgb: [0xFF, 0x00, 0x66] },
                ColorStop { pos: 1.0, rgb: [0x5A, 0x00, 0xFF] },
            ],
            motion: Motion::Scroll,
            speed: 0.8,
            brightness: 1.0,
        },
        CustomEffect {
            name: "Ocean".into(),
            palette: vec![
                ColorStop { pos: 0.0, rgb: [0x00, 0x2A, 0xFF] },
                ColorStop { pos: 1.0, rgb: [0x00, 0xFF, 0xC8] },
            ],
            motion: Motion::Bounce,
            speed: 0.6,
            brightness: 1.0,
        },
        CustomEffect {
            name: "Ember".into(),
            palette: vec![
                ColorStop { pos: 0.0, rgb: [0xFF, 0x30, 0x00] },
                ColorStop { pos: 1.0, rgb: [0xFF, 0xC4, 0x00] },
            ],
            motion: Motion::Twinkle,
            speed: 1.0,
            brightness: 1.0,
        },
    ]
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Rgb(m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2]))
}

fn triangle(x: f32) -> f32 {
    let p = x.rem_euclid(2.0);
    if p < 1.0 {
        p
    } else {
        2.0 - p
    }
}

fn hash01(x: f32) -> f32 {
    (x.sin() * 43758.547).fract().abs()
}
