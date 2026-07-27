//! OpenRGB SDK client (blocking) — the real lighting path on this hardware.
//!
//! Reverse-engineering established (see the session memory) that keyboard/lid
//! lighting on the PHN16S-71 is NOT driven by the `AcerGamingFunction` WMI class
//! — every WMI write is accepted but invisible. Acer instead runs an OpenRGB
//! server on `127.0.0.1:6742` and `AcerLightingService` is a client to it. The
//! LEDs are exposed as OpenRGB controllers:
//!
//! * `AcerHIDKeyboard Device`     — 4-zone keyboard, 10 modes incl. Direct
//! * `AcerHIDModeKeyLED Device`   — the mode-key indicator
//! * `AcerHIDCoverLogoLED Device` — the lid "shield" logo
//!
//! This is a minimal blocking client over `std::net` (no async runtime): enough
//! to enumerate controllers and push colors via Direct/Custom mode. Requires no
//! elevation — it is a localhost socket.
//!
//! Protocol: every message is a 16-byte header — magic `"ORGB"`, `u32` device
//! index, `u32` command, `u32` payload length — followed by the payload, all
//! little-endian.

#![cfg(windows)]
#![allow(dead_code)]

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::rgb::Rgb;

const MAGIC: &[u8; 4] = b"ORGB";
pub const DEFAULT_ADDR: &str = "127.0.0.1:6742";

// Command IDs from the OpenRGB SDK network protocol.
const SET_CLIENT_NAME: u32 = 50;
const REQUEST_CONTROLLER_COUNT: u32 = 0;
const REQUEST_CONTROLLER_DATA: u32 = 1;
const UPDATE_LEDS: u32 = 1050;
const UPDATE_ZONE_LEDS: u32 = 1051;
const SET_CUSTOM_MODE: u32 = 1053;
const UPDATE_MODE: u32 = 1100;
const SAVE_MODE: u32 = 1101;

// Mode flag bits (OpenRGB `RGBController.h`). We only name the ones we act on.
pub const MODE_FLAG_HAS_SPEED: u32 = 1 << 0;
pub const MODE_FLAG_HAS_BRIGHTNESS: u32 = 1 << 4;
pub const MODE_FLAG_HAS_PER_LED_COLOR: u32 = 1 << 5;
pub const MODE_FLAG_HAS_MODE_SPECIFIC_COLOR: u32 = 1 << 6;
/// The controller can persist this mode to onboard flash via `SAVE_MODE`.
pub const MODE_FLAG_MANUAL_SAVE: u32 = 1 << 8;
/// The controller persists automatically on `UPDATE_MODE` — no save needed.
pub const MODE_FLAG_AUTOMATIC_SAVE: u32 = 1 << 9;

/// Protocol version we advertise when requesting controller data. v4 matches the
/// blob layout parsed below.
const CLIENT_PROTOCOL_VERSION: u32 = 4;

pub struct OpenRgb {
    stream: TcpStream,
}

/// A controller as described by the server — enough of it to address every LED.
#[derive(Debug, Clone)]
pub struct Controller {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub location: String,
    pub serial: String,
    /// Raw OpenRGB device type (0..=19).
    pub dev_type: i32,
    pub modes: Vec<Mode>,
    pub active_mode: usize,
    pub zones: Vec<ZoneDesc>,
    /// Per-LED names, parallel to the flat LED/color vectors.
    pub leds: Vec<String>,
    /// Current per-LED colors (OpenRGB's last-set model, not a hardware read).
    pub colors: Vec<Rgb>,
    pub led_count: u16,
}

/// One zone in a controller's `zones` vector, with its topology.
#[derive(Debug, Clone)]
pub struct ZoneDesc {
    pub name: String,
    /// Raw OpenRGB zone type: 0 = single, 1 = linear, 2 = matrix.
    pub kind: i32,
    pub leds_min: u32,
    pub leds_max: u32,
    pub leds_count: u32,
    /// Start index into the controller's flat LED vector.
    pub start: u32,
    pub matrix: Option<MatrixDesc>,
}

/// A matrix zone's 2-D key map. `map` is row-major (height×width); `None` marks
/// a gap (OpenRGB's `0xFFFFFFFF` sentinel, e.g. under the spacebar).
#[derive(Debug, Clone)]
pub struct MatrixDesc {
    pub height: u32,
    pub width: u32,
    pub map: Vec<Option<u32>>,
}

impl Controller {
    pub fn mode_names(&self) -> Vec<&str> {
        self.modes.iter().map(|m| m.name.as_str()).collect()
    }
    pub fn mode(&self, name: &str) -> Option<&Mode> {
        let n = name.to_lowercase();
        self.modes.iter().find(|m| m.name.to_lowercase() == n)
    }
}

/// A hardware effect, with its tunable ranges and current parameters. Effects
/// like Breathing/Wave/Neon are firmware functions: set one via [`OpenRgb::
/// apply_effect`] and the controller animates it on its own, no host frames.
#[derive(Debug, Clone)]
pub struct Mode {
    pub index: u32,
    pub name: String,
    pub value: i32,
    pub flags: u32,
    pub speed_min: u32,
    pub speed_max: u32,
    pub brightness_min: u32,
    pub brightness_max: u32,
    pub colors_min: u32,
    pub colors_max: u32,
    pub speed: u32,
    pub brightness: u32,
    pub direction: u32,
    pub color_mode: u32,
    pub colors: Vec<u32>,
}

impl Mode {
    pub fn has_speed(&self) -> bool {
        self.speed_max > self.speed_min
    }
    pub fn has_brightness(&self) -> bool {
        self.brightness_max > self.brightness_min
    }
    pub fn takes_color(&self) -> bool {
        self.colors_max > 0
    }
    /// Whether this mode can be written to the device's onboard flash so it
    /// survives a power cycle. Manual-save needs an explicit [`OpenRgb::
    /// save_mode`]; automatic-save persists on `apply_effect` alone.
    pub fn can_save(&self) -> bool {
        self.flags & (MODE_FLAG_MANUAL_SAVE | MODE_FLAG_AUTOMATIC_SAVE) != 0
    }
    pub fn needs_manual_save(&self) -> bool {
        self.flags & MODE_FLAG_MANUAL_SAVE != 0
    }
    /// Whether the mode has any direction (LR / UD / HV — bits 1..=3).
    pub fn has_direction(&self) -> bool {
        self.flags & 0b1110 != 0
    }

    /// Serialize this mode for UPDATE_MODE (no leading size/index — the caller
    /// frames those). Mirrors OpenRGB's mode serialization order.
    fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        write_string(&mut b, &self.name);
        b.extend_from_slice(&self.value.to_le_bytes());
        b.extend_from_slice(&self.flags.to_le_bytes());
        for v in [
            self.speed_min,
            self.speed_max,
            self.brightness_min,
            self.brightness_max,
            self.colors_min,
            self.colors_max,
            self.speed,
            self.brightness,
            self.direction,
            self.color_mode,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&(self.colors.len() as u16).to_le_bytes());
        for c in &self.colors {
            b.extend_from_slice(&c.to_le_bytes());
        }
        b
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    // length includes the trailing NUL.
    buf.extend_from_slice(&((bytes.len() + 1) as u16).to_le_bytes());
    buf.extend_from_slice(bytes);
    buf.push(0);
}

/// OpenRGB packs a color as `R | G<<8 | B<<16` in a little-endian u32.
fn color_u32(c: Rgb) -> u32 {
    (c.0 as u32) | ((c.1 as u32) << 8) | ((c.2 as u32) << 16)
}

impl OpenRgb {
    /// Connect to the local Acer/OpenRGB server and register a client name.
    pub fn connect() -> io::Result<Self> {
        Self::connect_to(DEFAULT_ADDR)
    }

    pub fn connect_to(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_nodelay(true).ok();
        let mut me = Self { stream };
        me.send(0, SET_CLIENT_NAME, b"colormemuch\0")?;
        Ok(me)
    }

    fn send(&mut self, device: u32, command: u32, payload: &[u8]) -> io::Result<()> {
        let mut msg = Vec::with_capacity(16 + payload.len());
        msg.extend_from_slice(MAGIC);
        msg.extend_from_slice(&device.to_le_bytes());
        msg.extend_from_slice(&command.to_le_bytes());
        msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        msg.extend_from_slice(payload);
        self.stream.write_all(&msg)
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut header = [0u8; 16];
        self.stream.read_exact(&mut header)?;
        if &header[0..4] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad OpenRGB reply magic",
            ));
        }
        let size = u32::from_le_bytes([header[12], header[13], header[14], header[15]]) as usize;
        let mut data = vec![0u8; size];
        if size > 0 {
            self.stream.read_exact(&mut data)?;
        }
        Ok(data)
    }

    /// Number of controllers the server exposes.
    pub fn controller_count(&mut self) -> io::Result<u32> {
        self.send(0, REQUEST_CONTROLLER_COUNT, &[])?;
        let d = self.recv()?;
        if d.len() < 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short count reply"));
        }
        Ok(u32::from_le_bytes([d[0], d[1], d[2], d[3]]))
    }

    /// Fetch and parse one controller's description.
    pub fn controller(&mut self, index: u32) -> io::Result<Controller> {
        self.send(
            index,
            REQUEST_CONTROLLER_DATA,
            &CLIENT_PROTOCOL_VERSION.to_le_bytes(),
        )?;
        let blob = self.recv()?;
        parse_controller(index, &blob)
    }

    /// Enumerate every controller.
    pub fn controllers(&mut self) -> io::Result<Vec<Controller>> {
        let n = self.controller_count()?;
        (0..n).map(|i| self.controller(i)).collect()
    }

    /// Find the first controller whose name contains `needle` (case-insensitive).
    pub fn find(&mut self, needle: &str) -> io::Result<Option<Controller>> {
        let needle = needle.to_lowercase();
        Ok(self
            .controllers()?
            .into_iter()
            .find(|c| c.name.to_lowercase().contains(&needle)))
    }

    /// Set every LED of a controller to one color.
    pub fn set_all(&mut self, ctrl: &Controller, color: Rgb) -> io::Result<()> {
        let frame = vec![color; ctrl.led_count as usize];
        self.set_leds(ctrl, &frame)
    }

    /// Switch a controller into Direct/Custom mode. Do this ONCE before a run
    /// of [`update_leds`](Self::update_leds) frames — repeating it every frame
    /// makes the server re-init the device and pins the CPU.
    pub fn enter_direct(&mut self, ctrl: &Controller) -> io::Result<()> {
        self.send(ctrl.index, SET_CUSTOM_MODE, &[])
    }

    /// Push LED colors WITHOUT re-setting the mode — the hot path for
    /// animation. Assumes the controller is already in Direct mode
    /// (call [`enter_direct`](Self::enter_direct) first). If `colors` is shorter
    /// than the LED count the last color repeats; longer is truncated.
    pub fn update_leds(&mut self, ctrl: &Controller, colors: &[Rgb]) -> io::Result<()> {
        let n = ctrl.led_count as usize;
        let fallback = colors.last().copied().unwrap_or(Rgb(0, 0, 0));

        let mut inner = Vec::with_capacity(2 + n * 4);
        inner.extend_from_slice(&(n as u16).to_le_bytes());
        for i in 0..n {
            let c = colors.get(i).copied().unwrap_or(fallback);
            inner.extend_from_slice(&[c.0, c.1, c.2, 0]);
        }

        let mut payload = Vec::with_capacity(4 + inner.len());
        payload.extend_from_slice(&((inner.len() + 4) as u32).to_le_bytes());
        payload.extend_from_slice(&inner);
        self.send(ctrl.index, UPDATE_LEDS, &payload)
    }

    /// One-shot: enter Direct mode and set the colors. Convenient for a single
    /// static set; do NOT call this in an animation loop — use `enter_direct`
    /// once then `update_leds` per frame.
    pub fn set_leds(&mut self, ctrl: &Controller, colors: &[Rgb]) -> io::Result<()> {
        self.enter_direct(ctrl)?;
        self.update_leds(ctrl, colors)
    }

    /// Set the LEDs of a single zone. `zone` indexes into `ctrl.zones`.
    pub fn set_zone(&mut self, ctrl: &Controller, zone: usize, colors: &[Rgb]) -> io::Result<()> {
        self.send(ctrl.index, SET_CUSTOM_MODE, &[])?;
        let n = ctrl.zones.get(zone).map(|z| z.leds_count as usize).unwrap_or(0);
        let fallback = colors.last().copied().unwrap_or(Rgb(0, 0, 0));

        let mut inner = Vec::with_capacity(4 + 2 + n * 4);
        inner.extend_from_slice(&(zone as u32).to_le_bytes());
        inner.extend_from_slice(&(n as u16).to_le_bytes());
        for i in 0..n {
            let c = colors.get(i).copied().unwrap_or(fallback);
            inner.extend_from_slice(&[c.0, c.1, c.2, 0]);
        }
        let mut payload = Vec::with_capacity(4 + inner.len());
        payload.extend_from_slice(&((inner.len() + 4) as u32).to_le_bytes());
        payload.extend_from_slice(&inner);
        self.send(ctrl.index, UPDATE_ZONE_LEDS, &payload)
    }

    /// Invoke a firmware effect by name, with one color and (where supported)
    /// speed and brightness. The controller then animates it host-free.
    ///
    /// `speed`/`brightness` are clamped to the mode's advertised range.
    pub fn apply_effect(
        &mut self,
        ctrl: &Controller,
        mode_name: &str,
        color: Rgb,
        speed: Option<u32>,
        brightness: Option<u32>,
    ) -> io::Result<()> {
        let mode = self.resolve_mode(ctrl, mode_name, color, speed, brightness)?;
        self.send_mode(ctrl.index, UPDATE_MODE, &mode)
    }

    /// Like [`apply_effect`](Self::apply_effect) but ALSO writes the mode to the
    /// controller's onboard flash (`SAVE_MODE`) so it survives a power cycle
    /// with no host process running — the zero-CPU "Base identity" tier.
    ///
    /// Returns `Ok(false)` (without touching the device) if the mode can't be
    /// saved, so the caller can surface an honest "not supported" instead of
    /// pretending it stuck. On a savable mode it applies then saves, returning
    /// `Ok(true)`.
    pub fn save_mode(
        &mut self,
        ctrl: &Controller,
        mode_name: &str,
        color: Rgb,
        speed: Option<u32>,
        brightness: Option<u32>,
    ) -> io::Result<bool> {
        let mode = self.resolve_mode(ctrl, mode_name, color, speed, brightness)?;
        if !mode.can_save() {
            return Ok(false);
        }
        // Set it active first so the saved parameters match what's showing.
        self.send_mode(ctrl.index, UPDATE_MODE, &mode)?;
        if mode.needs_manual_save() {
            self.send_mode(ctrl.index, SAVE_MODE, &mode)?;
        }
        Ok(true)
    }

    /// Build a concrete [`Mode`] for `mode_name` with the given colour and
    /// clamped speed/brightness — shared by apply and save.
    fn resolve_mode(
        &self,
        ctrl: &Controller,
        mode_name: &str,
        color: Rgb,
        speed: Option<u32>,
        brightness: Option<u32>,
    ) -> io::Result<Mode> {
        let mut mode = ctrl
            .mode(mode_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such mode"))?
            .clone();

        if let Some(s) = speed {
            if mode.has_speed() {
                mode.speed = s.clamp(mode.speed_min, mode.speed_max);
            }
        }
        if let Some(b) = brightness {
            if mode.has_brightness() {
                mode.brightness = b.clamp(mode.brightness_min, mode.brightness_max);
            }
        }
        if mode.takes_color() {
            mode.colors = vec![color_u32(color)];
        }
        Ok(mode)
    }

    /// Frame a mode blob (size + index + serialized mode) and send it under
    /// `command` (`UPDATE_MODE` or `SAVE_MODE` — identical wire format).
    fn send_mode(&mut self, device: u32, command: u32, mode: &Mode) -> io::Result<()> {
        let mode_bytes = mode.to_bytes();
        let total = 8 + mode_bytes.len();
        let mut payload = Vec::with_capacity(total);
        payload.extend_from_slice(&(total as u32).to_le_bytes());
        payload.extend_from_slice(&mode.index.to_le_bytes());
        payload.extend_from_slice(&mode_bytes);
        self.send(device, command, &payload)
    }
}

/// Little-endian cursor over the controller-data blob.
struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn u16(&mut self) -> u16 {
        let v = u16::from_le_bytes([self.at(0), self.at(1)]);
        self.p += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes([self.at(0), self.at(1), self.at(2), self.at(3)]);
        self.p += 4;
        v
    }
    fn i32(&mut self) -> i32 {
        self.u32() as i32
    }
    fn at(&self, off: usize) -> u8 {
        self.b.get(self.p + off).copied().unwrap_or(0)
    }
    /// OpenRGB string: `u16` length (including trailing NUL) + bytes.
    fn string(&mut self) -> String {
        let len = self.u16() as usize;
        let end = (self.p + len).min(self.b.len());
        let raw = &self.b[self.p.min(self.b.len())..end];
        self.p += len;
        let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
        String::from_utf8_lossy(raw).into_owned()
    }
    fn skip(&mut self, n: usize) {
        self.p += n;
    }
}

/// Parse a v4 controller-data blob into the fields we address. Mirrors the
/// serialization order OpenRGB's `RGBController::WriteDeviceDescription` uses.
fn parse_controller(index: u32, blob: &[u8]) -> io::Result<Controller> {
    if blob.len() < 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "controller blob too short",
        ));
    }
    let mut c = Cursor { b: blob, p: 0 };

    let _data_size = c.u32();
    let dev_type = c.i32();
    let name = c.string();
    let vendor = c.string();
    let description = c.string();
    let _version = c.string();
    let serial = c.string();
    let location = c.string();

    let num_modes = c.u16();
    let active_mode = c.i32().max(0) as usize;
    let mut modes = Vec::with_capacity(num_modes as usize);
    for i in 0..num_modes {
        let name = c.string();
        let value = c.i32();
        let flags = c.u32();
        let speed_min = c.u32();
        let speed_max = c.u32();
        let brightness_min = c.u32();
        let brightness_max = c.u32();
        let colors_min = c.u32();
        let colors_max = c.u32();
        let speed = c.u32();
        let brightness = c.u32();
        let direction = c.u32();
        let color_mode = c.u32();
        let num_colors = c.u16();
        let colors = (0..num_colors).map(|_| c.u32()).collect();
        modes.push(Mode {
            index: i as u32,
            name,
            value,
            flags,
            speed_min,
            speed_max,
            brightness_min,
            brightness_max,
            colors_min,
            colors_max,
            speed,
            brightness,
            direction,
            color_mode,
            colors,
        });
    }

    let num_zones = c.u16();
    let mut zones = Vec::with_capacity(num_zones as usize);
    let mut start = 0u32;
    for _ in 0..num_zones {
        let zone_name = c.string();
        let kind = c.i32();
        let leds_min = c.u32();
        let leds_max = c.u32();
        let leds_count = c.u32();
        let matrix_len = c.u16() as usize;
        // Matrix block (when present): height u32, width u32, then h*w LED
        // indices — `matrix_len` == (2 + h*w) * 4.
        let matrix = if matrix_len >= 8 {
            let height = c.u32();
            let width = c.u32();
            let n = (height as usize).saturating_mul(width as usize);
            let map = (0..n)
                .map(|_| match c.u32() {
                    0xFFFF_FFFF => None,
                    v => Some(v),
                })
                .collect();
            let read = 8 + n * 4;
            if matrix_len > read {
                c.skip(matrix_len - read);
            }
            Some(MatrixDesc { height, width, map })
        } else {
            c.skip(matrix_len);
            None
        };
        zones.push(ZoneDesc {
            name: zone_name,
            kind,
            leds_min,
            leds_max,
            leds_count,
            start,
            matrix,
        });
        start += leds_count;
    }

    // LED section: names parallel to the flat LED/color vectors.
    let num_leds = c.u16();
    let leds = (0..num_leds)
        .map(|_| {
            let n = c.string();
            let _value = c.u32();
            n
        })
        .collect::<Vec<_>>();

    // Colors section: current per-LED colors (OpenRGB packs R | G<<8 | B<<16).
    let num_colors = c.u16();
    let colors = (0..num_colors)
        .map(|_| {
            let v = c.u32();
            Rgb((v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8)
        })
        .collect();

    let led_count = num_leds;

    Ok(Controller {
        index,
        name,
        vendor,
        description,
        location,
        serial,
        dev_type,
        modes,
        active_mode,
        zones,
        leds,
        colors,
        led_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live proof against the running Acer/OpenRGB server. No elevation needed,
    /// so unlike the WMI hardware tests this can run in a normal shell. Ignored
    /// so CI (which has no server) skips it.
    ///
    /// ```text
    /// cargo test --bin colormemuch -- --ignored --exact \
    ///     openrgb::tests::hw_enumerate_and_set --nocapture
    /// ```
    #[test]
    #[ignore = "needs the local OpenRGB server; writes hardware"]
    fn hw_enumerate_and_set() {
        let mut c = OpenRgb::connect().expect("connect to OpenRGB server");
        for ctrl in c.controllers().expect("enumerate") {
            eprintln!(
                "[{}] {} — {} LEDs, zones {:?}",
                ctrl.index, ctrl.name, ctrl.led_count, ctrl.zones
            );
        }
        let kb = c
            .find("keyboard")
            .expect("query")
            .expect("keyboard controller present");
        // Magenta — distinct from the green the PowerShell proof left.
        c.set_all(&kb, Rgb(0xFF, 0x00, 0xFF)).expect("set color");
        eprintln!("set '{}' to magenta", kb.name);
    }

    /// Golden descriptor check against the live server — proves the full parse
    /// (zones, per-LED names, modes) holds on real hardware. Read-only.
    ///
    /// ```text
    /// cargo test --lib -- --ignored --exact \
    ///     openrgb::tests::hw_descriptor_shape --nocapture
    /// ```
    #[test]
    #[ignore = "needs the local OpenRGB server; reads only"]
    fn hw_descriptor_shape() {
        let mut c = OpenRgb::connect().expect("connect");
        let ctrls = c.controllers().expect("enumerate");
        assert!(!ctrls.is_empty(), "no controllers");
        for ctrl in &ctrls {
            // The LED-names section must line up with the LED count.
            assert_eq!(
                ctrl.leds.len(),
                ctrl.led_count as usize,
                "LED names must match led_count for {}",
                ctrl.name
            );
            // Zone LED counts must sum to the total (contiguous zones).
            let zone_sum: u32 = ctrl.zones.iter().map(|z| z.leds_count).sum();
            assert_eq!(zone_sum, ctrl.led_count as u32, "zone sum for {}", ctrl.name);
        }
        let kb = ctrls
            .iter()
            .find(|c| c.name.to_lowercase().contains("keyboard"))
            .expect("keyboard controller");
        assert!(kb.modes.iter().any(|m| m.name.eq_ignore_ascii_case("static")));
        eprintln!(
            "descriptor ok: {} controllers, keyboard has {} LEDs / {} zones / {} modes",
            ctrls.len(),
            kb.led_count,
            kb.zones.len(),
            kb.modes.len()
        );
    }
}
