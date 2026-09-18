//! The discoverable device model — a serializable projection of an OpenRGB
//! controller's full descriptor.
//!
//! The engine keeps the rich [`crate::openrgb::Controller`] for writes; this is
//! the faithful copy the GUI consumes to *discover* what a device is (type,
//! zones, matrix, per-LED names, modes + capabilities, current colors) and drive
//! its own structure from it. "RGBController over IP" — nothing assumed.

#![cfg(windows)]

use serde::{Deserialize, Serialize};

use crate::openrgb::{Controller, Mode, ZoneDesc};
use crate::rgb::Rgb;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceDescriptor {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub location: String,
    pub serial: String,
    pub kind: DeviceKind,
    pub zones: Vec<ZoneInfo>,
    pub leds: Vec<LedInfo>,
    pub modes: Vec<ModeInfo>,
    pub active_mode: usize,
    /// Current per-LED colors (OpenRGB's model, not a guaranteed hardware read).
    pub colors: Vec<Rgb>,
    /// == `leds.len()`; retained for the write path.
    pub led_count: u16,
}

impl DeviceDescriptor {
    pub fn of(c: &Controller) -> Self {
        // Acer's desktop tower DIMM controller reports type 0 (motherboard);
        // fix the label from the name so the UI shows "RAM".
        let mut kind = DeviceKind::from_i32(c.dev_type);
        if kind == DeviceKind::Motherboard && c.name.to_uppercase().contains("DIMM") {
            kind = DeviceKind::Dram;
        }
        Self {
            index: c.index,
            name: c.name.clone(),
            vendor: c.vendor.clone(),
            description: c.description.clone(),
            location: c.location.clone(),
            serial: c.serial.clone(),
            kind,
            zones: c.zones.iter().map(ZoneInfo::of).collect(),
            leds: c.leds.iter().map(|n| LedInfo { name: n.clone() }).collect(),
            modes: c.modes.iter().map(ModeInfo::of).collect(),
            active_mode: c.active_mode,
            colors: c.colors.clone(),
            led_count: c.led_count,
        }
    }

    /// Mode names minus the raw "Direct" mode (which the UI never surfaces).
    pub fn effect_mode_names(&self) -> Vec<String> {
        self.modes
            .iter()
            .filter(|m| !m.name.eq_ignore_ascii_case("direct"))
            .map(|m| m.name.clone())
            .collect()
    }

    /// A stable key over the device's *shape* (name + zone topology), so a saved
    /// profile binds to the right device and refuses a changed topology.
    pub fn topology_sig(&self) -> String {
        let zones: Vec<String> = self
            .zones
            .iter()
            .map(|z| format!("{}:{}:{}", z.name, z.kind as u8, z.leds_count))
            .collect();
        format!("{}|{}|{}", self.name, self.led_count, zones.join(","))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZoneInfo {
    pub name: String,
    pub kind: ZoneKind,
    pub start: u32,
    pub leds_count: u32,
    pub leds_min: u32,
    pub leds_max: u32,
    pub matrix: Option<Matrix>,
}

impl ZoneInfo {
    fn of(z: &ZoneDesc) -> Self {
        Self {
            name: z.name.clone(),
            kind: ZoneKind::from_i32(z.kind),
            start: z.start,
            leds_count: z.leds_count,
            leds_min: z.leds_min,
            leds_max: z.leds_max,
            matrix: z.matrix.as_ref().map(|m| Matrix {
                height: m.height,
                width: m.width,
                map: m.map.clone(),
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Matrix {
    pub height: u32,
    pub width: u32,
    /// Row-major, height×width; `None` marks a gap.
    pub map: Vec<Option<u32>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedInfo {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModeInfo {
    pub name: String,
    pub flags: u32,
    pub has_speed: bool,
    pub has_brightness: bool,
    pub has_direction: bool,
    pub takes_color: bool,
    pub can_save: bool,
    pub speed_range: (u32, u32),
    pub brightness_range: (u32, u32),
}

impl ModeInfo {
    fn of(m: &Mode) -> Self {
        Self {
            name: m.name.clone(),
            flags: m.flags,
            has_speed: m.has_speed(),
            has_brightness: m.has_brightness(),
            has_direction: m.has_direction(),
            takes_color: m.takes_color(),
            can_save: m.can_save(),
            speed_range: (m.speed_min, m.speed_max),
            brightness_range: (m.brightness_min, m.brightness_max),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceKind {
    Motherboard,
    Dram,
    Gpu,
    Cooler,
    LedStrip,
    Keyboard,
    Mouse,
    Mousemat,
    Headset,
    Gamepad,
    Light,
    Case,
    Storage,
    Accessory,
    Unknown,
}

impl DeviceKind {
    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Motherboard,
            1 => Self::Dram,
            2 => Self::Gpu,
            3 => Self::Cooler,
            4 => Self::LedStrip,
            5 => Self::Keyboard,
            6 => Self::Mouse,
            7 => Self::Mousemat,
            8 => Self::Headset,
            10 => Self::Gamepad,
            11 => Self::Light,
            14 => Self::Storage,
            15 => Self::Case,
            17 => Self::Accessory,
            _ => Self::Unknown,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Motherboard => "Motherboard",
            Self::Dram => "RAM",
            Self::Gpu => "GPU",
            Self::Cooler => "Cooler",
            Self::LedStrip => "LED strip",
            Self::Keyboard => "Keyboard",
            Self::Mouse => "Mouse",
            Self::Mousemat => "Mousemat",
            Self::Headset => "Headset",
            Self::Gamepad => "Gamepad",
            Self::Light => "Light",
            Self::Case => "Case",
            Self::Storage => "Storage",
            Self::Accessory => "Accessory",
            Self::Unknown => "Device",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZoneKind {
    Single,
    Linear,
    Matrix,
}

impl ZoneKind {
    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Single,
            2 => Self::Matrix,
            _ => Self::Linear,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Linear => "linear",
            Self::Matrix => "matrix",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DeviceDescriptor {
        DeviceDescriptor {
            index: 0,
            name: "Test KB".into(),
            vendor: "acme".into(),
            description: "d".into(),
            location: "l".into(),
            serial: "s".into(),
            kind: DeviceKind::Keyboard,
            zones: vec![ZoneInfo {
                name: "z".into(),
                kind: ZoneKind::Linear,
                start: 0,
                leds_count: 4,
                leds_min: 4,
                leds_max: 4,
                matrix: None,
            }],
            leds: vec![LedInfo { name: "a".into() }, LedInfo { name: "b".into() }],
            modes: vec![ModeInfo {
                name: "Static".into(),
                flags: 0,
                has_speed: false,
                has_brightness: true,
                has_direction: false,
                takes_color: true,
                can_save: false,
                speed_range: (0, 0),
                brightness_range: (0, 100),
            }],
            active_mode: 1,
            colors: vec![Rgb(1, 2, 3)],
            led_count: 4,
        }
    }

    #[test]
    fn descriptor_round_trips() {
        let d = sample();
        let s = serde_json::to_string(&d).unwrap();
        let back: DeviceDescriptor = serde_json::from_str(&s).unwrap();
        assert_eq!(d.topology_sig(), back.topology_sig());
        assert_eq!(back.leds.len(), 2);
        assert_eq!(back.colors[0], Rgb(1, 2, 3));
        assert_eq!(back.effect_mode_names(), vec!["Static".to_string()]);
    }

    #[test]
    fn topology_sig_tracks_shape_not_color() {
        let mut d = sample();
        let a = d.topology_sig();
        d.colors[0] = Rgb(9, 9, 9); // color changes: same shape
        assert_eq!(a, d.topology_sig());
        d.zones[0].leds_count = 5; // shape changes
        assert_ne!(a, d.topology_sig());
    }

    #[test]
    fn kind_mapping() {
        assert_eq!(DeviceKind::from_i32(5), DeviceKind::Keyboard);
        assert_eq!(DeviceKind::from_i32(11), DeviceKind::Light);
        assert_eq!(DeviceKind::from_i32(999), DeviceKind::Unknown);
        assert_eq!(ZoneKind::from_i32(0), ZoneKind::Single);
        assert_eq!(ZoneKind::from_i32(2), ZoneKind::Matrix);
        assert_eq!(ZoneKind::from_i32(7), ZoneKind::Linear);
    }
}
