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
const SET_CUSTOM_MODE: u32 = 1053;

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
    pub modes: Vec<String>,
    /// `(zone name, led count)`.
    pub zones: Vec<(String, u32)>,
    pub led_count: u16,
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

    /// Set every LED of a controller to one color, via Direct/Custom mode so it
    /// persists rather than being overridden by an active effect.
    pub fn set_all(&mut self, ctrl: &Controller, color: Rgb) -> io::Result<()> {
        self.send(ctrl.index, SET_CUSTOM_MODE, &[])?;

        let n = ctrl.led_count;
        let mut inner = Vec::with_capacity(2 + n as usize * 4);
        inner.extend_from_slice(&n.to_le_bytes());
        for _ in 0..n {
            // OpenRGB color is R,G,B,0 little-endian.
            inner.extend_from_slice(&[color.0, color.1, color.2, 0]);
        }

        let mut payload = Vec::with_capacity(4 + inner.len());
        payload.extend_from_slice(&((inner.len() + 4) as u32).to_le_bytes());
        payload.extend_from_slice(&inner);
        self.send(ctrl.index, UPDATE_LEDS, &payload)
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
    let _type = c.i32();
    let name = c.string();
    let vendor = c.string();
    let description = c.string();
    let _version = c.string();
    let _serial = c.string();
    let _location = c.string();

    let num_modes = c.u16();
    let _active_mode = c.i32();
    let mut modes = Vec::with_capacity(num_modes as usize);
    for _ in 0..num_modes {
        let mode_name = c.string();
        let _value = c.i32();
        let _flags = c.u32();
        let _speed_min = c.u32();
        let _speed_max = c.u32();
        let _bright_min = c.u32();
        let _bright_max = c.u32();
        let _colors_min = c.u32();
        let _colors_max = c.u32();
        let _speed = c.u32();
        let _brightness = c.u32();
        let _direction = c.u32();
        let _color_mode = c.u32();
        let num_colors = c.u16();
        c.skip(num_colors as usize * 4);
        modes.push(mode_name);
    }

    let num_zones = c.u16();
    let mut zones = Vec::with_capacity(num_zones as usize);
    for _ in 0..num_zones {
        let zone_name = c.string();
        let _zone_type = c.i32();
        let _leds_min = c.u32();
        let _leds_max = c.u32();
        let leds_count = c.u32();
        let matrix_len = c.u16();
        c.skip(matrix_len as usize);
        zones.push((zone_name, leds_count));
    }

    let led_count = c.u16();

    Ok(Controller {
        index,
        name,
        vendor,
        description,
        modes,
        zones,
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
}
