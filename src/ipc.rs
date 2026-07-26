//! The IPC contract between the GUI (client) and the daemon (server).
//!
//! Length-prefixed JSON over a named pipe. The wire format is the already
//! **resolved** [`EngineState`] — the GUI does authoring/resolution, the daemon
//! just renders and holds. Request/response only; the client polls for status.

#![cfg(windows)]

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::engine::{ControllerInfo, EngineState, HwSpec};

/// The pipe the daemon serves and the GUI connects to. A bare name — on Windows
/// `interprocess` places it in the `\\.\pipe\` namespace on both ends.
pub const PIPE_NAME: &str = "colormemuch-svc";

/// GUI → daemon.
#[derive(Serialize, Deserialize)]
pub enum ClientMsg {
    /// Handshake on connect.
    Hello,
    /// Apply a resolved compositor snapshot (also persisted by the daemon).
    SetState(EngineState),
    /// Persist a firmware mode to a device's flash.
    SaveFirmware { device: usize, spec: HwSpec },
    /// Drop and re-establish the hardware connection.
    Reconnect,
    /// Ask for current status (controllers, battery, connection).
    Poll,
}

/// Daemon → GUI.
#[derive(Serialize, Deserialize)]
pub enum ServerMsg {
    Status {
        connected: bool,
        error: Option<String>,
        on_battery: bool,
        /// Current controller list (the client de-dupes to detect real changes).
        controllers: Option<Vec<ControllerInfo>>,
    },
    SaveResult(Result<bool, String>),
    Ok,
}

/// Write a length-prefixed JSON frame.
pub fn write_msg<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let buf = serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(&(buf.len() as u32).to_le_bytes())?;
    w.write_all(&buf)?;
    w.flush()
}

/// Read a length-prefixed JSON frame.
pub fn read_msg<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
