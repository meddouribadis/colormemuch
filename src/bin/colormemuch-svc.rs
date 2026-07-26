//! The daemon binary — a thin CLI over `colormemuch::daemon`.
//!
//! * `run`        — entry the SCM invokes (registered by `install`).
//! * `console`    — run the serve loop in the foreground (development).
//! * `install` / `uninstall` — register/unregister the Windows service.

#[cfg(windows)]
fn main() {
    use colormemuch::daemon;

    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "run" => {
            if let Err(e) = daemon::run_service_dispatch() {
                eprintln!("service dispatch failed: {e}");
            }
        }
        "console" => daemon::run_console(),
        "install" => match daemon::install() {
            Ok(()) => println!("installed {} (auto-start)", daemon::SERVICE_NAME),
            Err(e) => eprintln!("install failed: {e}"),
        },
        "uninstall" => match daemon::uninstall() {
            Ok(()) => println!("uninstalled {}", daemon::SERVICE_NAME),
            Err(e) => eprintln!("uninstall failed: {e}"),
        },
        other => {
            eprintln!("colormemuch-svc: unknown mode {other:?}");
            eprintln!("usage: colormemuch-svc [run|console|install|uninstall]");
            std::process::exit(2);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("colormemuch-svc is Windows-only");
}
