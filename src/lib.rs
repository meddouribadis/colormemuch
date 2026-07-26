//! colormemuch **core** — the lighting logic, shared by both binaries.
//!
//! The architecture is single-owner: the daemon ([`daemon`]) is the sole writer
//! to the hardware and the authority on lighting state; the GUI is a thin client
//! ([`host::ConnectedHost`]) that sends resolved [`engine::EngineState`]
//! snapshots and renders what the daemon reports. When no daemon is installed
//! the same core runs in-process ([`host::EmbeddedHost`]) — one implementation,
//! two hosts. See `docs/COMPOSITOR.md`.
//!
//! This crate carries no UI: no eframe, no egui. It is the reusable half.

#![cfg(windows)]

/// App identity, single source of truth for config/profile paths. The GUI's
/// window title and repo live in its own binary.
pub const APP_NAME: &str = "ColorMeMuch";

pub mod effects;
pub mod engine;
pub mod host;
pub mod ipc;
pub mod library;
pub mod openrgb;
pub mod rgb;

pub mod daemon;
pub mod wmi;
