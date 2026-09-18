//! The daemon — the single owner of lighting state and the sole writer to the
//! hardware. Runs as a Windows service (or in the foreground for testing),
//! hosts the engine, and serves the GUI over the named-pipe IPC contract.
//!
//! It persists the last applied [`EngineState`] to a machine-wide store so it
//! can restore lighting at boot before any GUI connects — the reason a service
//! exists at all.

#![cfg(windows)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, ERROR_PIPE_CONNECTED, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::engine::{self, EngineCmd, EngineEvent, EngineHandle, EngineState};
use crate::ipc::{self, ClientMsg, ServerMsg};
use crate::model::DeviceDescriptor;

pub const SERVICE_NAME: &str = "colormemuch-svc";
const DISPLAY_NAME: &str = "colormemuch lighting service";

/// Cached status the connection handlers answer `Poll` from.
#[derive(Default)]
struct Snapshot {
    connected: bool,
    error: Option<String>,
    on_battery: bool,
    controllers: Option<Vec<DeviceDescriptor>>,
    dt_available: bool,
    dt_error: Option<String>,
}

// --- the serve loop, shared by service + console ---------------------------

/// Run until `stop` is set: host the engine, restore the saved profile, and
/// serve GUI connections.
pub fn serve(stop: Arc<AtomicBool>) {
    let wake = Arc::new(|| {}) as engine::Waker;
    let EngineHandle { cmd, evt } = engine::spawn(wake);

    // Restore the machine-wide profile so lighting comes up at boot.
    if let Some(state) = load_state() {
        let _ = cmd.send(EngineCmd::SetState(state));
    }

    let snapshot = Arc::new(Mutex::new(Snapshot::default()));

    // Pump engine events into the shared snapshot.
    {
        let snapshot = snapshot.clone();
        thread::spawn(move || {
            while let Ok(ev) = evt.recv() {
                let mut s = snapshot.lock().unwrap();
                match ev {
                    EngineEvent::Connected(cs) => {
                        s.connected = true;
                        s.error = None;
                        s.controllers = Some(cs.iter().map(DeviceDescriptor::of).collect());
                    }
                    EngineEvent::Disconnected(e) => {
                        s.connected = false;
                        s.error = Some(e);
                    }
                    EngineEvent::OnBattery(b) => s.on_battery = b,
                    EngineEvent::DtStatus { available, error } => {
                        s.dt_available = available;
                        s.dt_error = error;
                    }
                }
            }
        });
    }

    // A raw named-pipe server (not `interprocess`) so we can attach a security
    // descriptor that lets a user-session GUI connect to a LocalSystem service.
    let name = wide(&pipe_path());
    let sa = make_security_attributes();
    let sa_ptr = sa.as_ref().map(|s| s as *const SECURITY_ATTRIBUTES);

    // Unblock the accept when asked to stop by poking our own pipe.
    {
        let stop = stop.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(150));
            }
            poke_pipe();
        });
    }

    while !stop.load(Ordering::SeqCst) {
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                sa_ptr,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            break;
        }
        let ready = match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => true,
            Err(e) => e.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0),
        };
        if stop.load(Ordering::SeqCst) {
            unsafe {
                let _ = CloseHandle(pipe);
            }
            break;
        }
        if ready {
            let cmd = cmd.clone();
            let snapshot = snapshot.clone();
            let stream = PipeStream(pipe);
            thread::spawn(move || handle_conn(stream, cmd, snapshot));
        } else {
            unsafe {
                let _ = CloseHandle(pipe);
            }
        }
    }
}

/// One GUI connection: synchronous request/response until it hangs up.
fn handle_conn<S: Read + Write>(mut conn: S, cmd: Sender<EngineCmd>, snapshot: Arc<Mutex<Snapshot>>) {
    loop {
        let msg: ClientMsg = match ipc::read_msg(&mut conn) {
            Ok(m) => m,
            Err(_) => return,
        };
        let reply = match msg {
            ClientMsg::Hello => ServerMsg::Ok,
            ClientMsg::Poll => {
                let s = snapshot.lock().unwrap();
                ServerMsg::Status {
                    connected: s.connected,
                    error: s.error.clone(),
                    on_battery: s.on_battery,
                    controllers: s.controllers.clone(),
                    dt_available: s.dt_available,
                    dt_error: s.dt_error.clone(),
                }
            }
            ClientMsg::SetState(state) => {
                save_state(&state);
                let _ = cmd.send(EngineCmd::SetState(state));
                ServerMsg::Ok
            }
            ClientMsg::SaveFirmware { device, spec } => {
                let (tx, rx) = std::sync::mpsc::channel();
                let _ = cmd.send(EngineCmd::SaveFirmware {
                    device,
                    spec,
                    reply: tx,
                });
                let res = rx
                    .recv()
                    .unwrap_or_else(|_| Err("engine unavailable".into()));
                ServerMsg::SaveResult(res)
            }
            ClientMsg::Reconnect => {
                let _ = cmd.send(EngineCmd::Reconnect);
                ServerMsg::Ok
            }
        };
        if ipc::write_msg(&mut conn, &reply).is_err() {
            return;
        }
    }
}

/// Foreground run for development: `colormemuch-svc console`. Ctrl-C to stop.
pub fn run_console() {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc_stop(stop);
    }
    println!("colormemuch-svc serving on {} — Ctrl-C to stop", ipc::PIPE_NAME);
    serve(stop);
}

/// Minimal Ctrl-C handler without pulling a crate: set the flag on SIGINT.
fn ctrlc_stop(stop: Arc<AtomicBool>) -> std::io::Result<()> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_C_EVENT};
    // A process-wide static the C callback can reach.
    static FLAG: AtomicBool = AtomicBool::new(false);
    unsafe extern "system" fn handler(kind: u32) -> BOOL {
        if kind == CTRL_C_EVENT {
            FLAG.store(true, Ordering::SeqCst);
            return true.into();
        }
        false.into()
    }
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handler), true);
    }
    thread::spawn(move || loop {
        if FLAG.load(Ordering::SeqCst) {
            stop.store(true, Ordering::SeqCst);
            return;
        }
        thread::sleep(Duration::from_millis(150));
    });
    Ok(())
}

// --- read-only discovery dump ----------------------------------------------

/// `colormemuch-svc list` — connect read-only, enumerate, and print the full
/// device descriptor tree. Writes nothing to the hardware.
pub fn run_list() {
    let mut client = match crate::openrgb::OpenRgb::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect failed: {e} (is an OpenRGB server on :6742?)");
            return;
        }
    };
    let controllers = match client.controllers() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("enumerate failed: {e}");
            return;
        }
    };
    println!(
        "{} controller(s) on {}",
        controllers.len(),
        crate::openrgb::DEFAULT_ADDR
    );
    for c in &controllers {
        let d = DeviceDescriptor::of(c);
        println!("\n[{}] {}  ({})", d.index, d.name, d.kind.label());
        if !d.vendor.is_empty() || !d.description.is_empty() {
            println!("    {} — {}", d.vendor, d.description);
        }
        println!("    zones ({}):", d.zones.len());
        for z in &d.zones {
            let m = z
                .matrix
                .as_ref()
                .map(|m| format!(" matrix {}x{}", m.height, m.width))
                .unwrap_or_default();
            println!(
                "      - {} [{}] {} LEDs{}",
                z.name,
                z.kind.label(),
                z.leds_count,
                m
            );
        }
        let names: Vec<&str> = d.leds.iter().map(|l| l.name.as_str()).collect();
        println!("    LEDs ({}): {}", d.leds.len(), names.join(", "));
        println!("    modes ({}):", d.modes.len());
        for md in &d.modes {
            let mut caps = Vec::new();
            if md.has_speed {
                caps.push("speed");
            }
            if md.has_brightness {
                caps.push("brightness");
            }
            if md.has_direction {
                caps.push("direction");
            }
            if md.takes_color {
                caps.push("color");
            }
            if md.can_save {
                caps.push("save");
            }
            println!("      - {} [{}]", md.name, caps.join(","));
        }
    }
}

// --- machine-wide profile store --------------------------------------------

fn state_path() -> Option<PathBuf> {
    std::env::var_os("ProgramData")
        .map(|p| PathBuf::from(p).join(crate::APP_NAME).join("daemon-state.json"))
}

fn load_state() -> Option<EngineState> {
    let p = state_path()?;
    let s = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&s).ok()
}

fn save_state(state: &EngineState) {
    let Some(p) = state_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(p, s);
    }
}

// --- raw named-pipe server (with a security descriptor) --------------------

/// The full pipe path `interprocess` maps our bare name to, so the GUI client
/// (on `interprocess`) and this raw server agree.
fn pipe_path() -> String {
    format!(r"\\.\pipe\{}", ipc::PIPE_NAME)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A security descriptor granting LocalSystem + Administrators full control and
/// Authenticated Users read/write — so a user-session GUI can reach the pipe a
/// LocalSystem service creates. Leaked for the process lifetime (one alloc).
fn make_security_attributes() -> Option<SECURITY_ATTRIBUTES> {
    const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)";
    let w = wide(SDDL);
    let mut psd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(w.as_ptr()), 1, &mut psd, None)
            .ok()?;
    }
    Some(SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: BOOL(0),
    })
}

/// Open+close the pipe once to release a blocked `ConnectNamedPipe` at shutdown.
fn poke_pipe() {
    let name = wide(&pipe_path());
    unsafe {
        if let Ok(h) = CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) {
            let _ = CloseHandle(h);
        }
    }
}

/// A `Read + Write` over a connected named-pipe instance.
struct PipeStream(HANDLE);
unsafe impl Send for PipeStream {}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        unsafe { ReadFile(self.0, Some(buf), Some(&mut n), None) }
            .map(|_| n as usize)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        unsafe { WriteFile(self.0, Some(buf), Some(&mut n), None) }
            .map(|_| n as usize)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        unsafe {
            let _ = FlushFileBuffers(self.0);
        }
        Ok(())
    }
}

impl Drop for PipeStream {
    fn drop(&mut self) {
        unsafe {
            let _ = FlushFileBuffers(self.0);
            let _ = DisconnectNamedPipe(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

// --- Windows service wrapper -----------------------------------------------

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

define_windows_service!(ffi_service_main, service_main);

fn service_main(_args: Vec<OsString>) {
    let _ = run_service();
}

fn run_service() -> windows_service::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let handler_stop = stop.clone();
    let event_handler = move |control| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                handler_stop.store(true, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;
    let running = |state, accept| ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };

    status_handle.set_service_status(running(ServiceState::Running, ServiceControlAccept::STOP))?;
    serve(stop);
    status_handle.set_service_status(running(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

/// Entry when the SCM launches us (`colormemuch-svc run`).
pub fn run_service_dispatch() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

pub fn install() -> windows_service::Result<()> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)?;
    let exe = std::env::current_exe().unwrap();
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments: vec![OsString::from("run")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
    let _ = service.set_description("Holds colormemuch lighting against Acer/PredatorSense.");
    Ok(())
}

pub fn uninstall() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    )?;
    let _ = service.stop();
    service.delete()
}
