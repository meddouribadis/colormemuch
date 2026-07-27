//! Where the compositor runs, from the GUI's point of view — behind one trait.
//!
//! [`EmbeddedHost`] runs the engine in-process (colors die when the GUI exits,
//! today's behavior when no daemon is installed). [`ConnectedHost`] is a thin
//! client of the daemon (colors persist — the daemon owns them). [`create`]
//! picks the daemon if it's serving, else embeds. Same core, two hosts.

#![cfg(windows)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use interprocess::local_socket::LocalSocketStream;

use crate::engine::{self, EngineCmd, EngineEvent, EngineHandle, EngineState, HwSpec, Waker};
use crate::ipc::{self, ClientMsg, ServerMsg};
use crate::model::DeviceDescriptor;

/// Events surfaced to the GUI, uniform across host kinds.
pub enum HostEvent {
    Connected(Vec<DeviceDescriptor>),
    Disconnected(String),
    OnBattery(bool),
    SaveResult(Result<bool, String>),
}

pub trait Host {
    /// Drain any pending events (non-blocking).
    fn poll(&mut self) -> Vec<HostEvent>;
    /// Apply a resolved compositor snapshot.
    fn set_state(&mut self, state: &EngineState);
    /// Persist a firmware mode; the result arrives via a later [`poll`].
    fn save_firmware(&mut self, device: usize, spec: HwSpec);
    /// Drop and re-establish the hardware connection.
    fn reconnect(&mut self);
    /// True when backed by the daemon (drives the UI's ownership label).
    fn via_service(&self) -> bool;
}

/// Connect to the daemon if it's serving; otherwise run the engine in-process.
pub fn create(wake: Waker) -> Box<dyn Host> {
    match ConnectedHost::try_connect() {
        Ok(h) => Box::new(h),
        Err(_) => Box::new(EmbeddedHost::new(wake)),
    }
}

// --- embedded: the engine lives in this process ----------------------------

pub struct EmbeddedHost {
    engine: EngineHandle,
    save_rx: Option<Receiver<Result<bool, String>>>,
}

impl EmbeddedHost {
    pub fn new(wake: Waker) -> Self {
        Self {
            engine: engine::spawn(wake),
            save_rx: None,
        }
    }
}

impl Host for EmbeddedHost {
    fn poll(&mut self) -> Vec<HostEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.engine.evt.try_recv() {
            out.push(match ev {
                EngineEvent::Connected(cs) => {
                    HostEvent::Connected(cs.iter().map(DeviceDescriptor::of).collect())
                }
                EngineEvent::Disconnected(e) => HostEvent::Disconnected(e),
                EngineEvent::OnBattery(b) => HostEvent::OnBattery(b),
            });
        }
        if let Some(rx) = &self.save_rx {
            if let Ok(res) = rx.try_recv() {
                out.push(HostEvent::SaveResult(res));
                self.save_rx = None;
            }
        }
        out
    }

    fn set_state(&mut self, state: &EngineState) {
        self.engine.send(EngineCmd::SetState(state.clone()));
    }

    fn save_firmware(&mut self, device: usize, spec: HwSpec) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.engine.send(EngineCmd::SaveFirmware {
            device,
            spec,
            reply: tx,
        });
        self.save_rx = Some(rx);
    }

    fn reconnect(&mut self) {
        self.engine.send(EngineCmd::Reconnect);
    }

    fn via_service(&self) -> bool {
        false
    }
}

// --- connected: a thin client of the daemon --------------------------------

const POLL_INTERVAL: Duration = Duration::from_millis(400);

pub struct ConnectedHost {
    stream: LocalSocketStream,
    last_poll: Instant,
    alive: bool,
    /// Controller-name signature, to emit `Connected` only on real change.
    last_ctrls: Option<Vec<String>>,
    last_batt: Option<bool>,
    pending_save: Option<Result<bool, String>>,
}

impl ConnectedHost {
    pub fn try_connect() -> std::io::Result<Self> {
        let mut stream = LocalSocketStream::connect(ipc::PIPE_NAME)?;
        ipc::write_msg(&mut stream, &ClientMsg::Hello)?;
        let _: ServerMsg = ipc::read_msg(&mut stream)?;
        Ok(Self {
            stream,
            last_poll: Instant::now() - POLL_INTERVAL,
            alive: true,
            last_ctrls: None,
            last_batt: None,
            pending_save: None,
        })
    }

    fn req(&mut self, msg: &ClientMsg) -> std::io::Result<ServerMsg> {
        ipc::write_msg(&mut self.stream, msg)?;
        ipc::read_msg(&mut self.stream)
    }

    fn dead(&mut self, e: impl std::fmt::Display) -> Vec<HostEvent> {
        self.alive = false;
        vec![HostEvent::Disconnected(format!("service connection lost: {e}"))]
    }
}

impl Host for ConnectedHost {
    fn poll(&mut self) -> Vec<HostEvent> {
        let mut out = Vec::new();
        if let Some(res) = self.pending_save.take() {
            out.push(HostEvent::SaveResult(res));
        }
        if !self.alive || self.last_poll.elapsed() < POLL_INTERVAL {
            return out;
        }
        self.last_poll = Instant::now();
        match self.req(&ClientMsg::Poll) {
            Ok(ServerMsg::Status {
                connected,
                error,
                on_battery,
                controllers,
            }) => {
                match controllers {
                    Some(cs) => {
                        let sig: Vec<String> = cs.iter().map(|c| c.name.clone()).collect();
                        if self.last_ctrls.as_ref() != Some(&sig) {
                            self.last_ctrls = Some(sig);
                            out.push(HostEvent::Connected(cs));
                        }
                    }
                    None if !connected => {
                        self.last_ctrls = None;
                        out.push(HostEvent::Disconnected(
                            error.unwrap_or_else(|| "service: no device".into()),
                        ));
                    }
                    None => {}
                }
                if self.last_batt != Some(on_battery) {
                    self.last_batt = Some(on_battery);
                    out.push(HostEvent::OnBattery(on_battery));
                }
                out
            }
            Ok(_) => out,
            Err(e) => self.dead(e),
        }
    }

    fn set_state(&mut self, state: &EngineState) {
        if self.alive && self.req(&ClientMsg::SetState(state.clone())).is_err() {
            self.alive = false;
        }
    }

    fn save_firmware(&mut self, device: usize, spec: HwSpec) {
        if !self.alive {
            return;
        }
        match self.req(&ClientMsg::SaveFirmware { device, spec }) {
            Ok(ServerMsg::SaveResult(res)) => self.pending_save = Some(res),
            Ok(_) => {}
            Err(_) => self.alive = false,
        }
    }

    fn reconnect(&mut self) {
        if self.alive {
            let _ = self.req(&ClientMsg::Reconnect);
        }
    }

    fn via_service(&self) -> bool {
        true
    }
}
