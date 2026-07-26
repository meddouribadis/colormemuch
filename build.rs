fn main() {
    // Derive the full version from the latest git tag (supports 4-part tags like v0.1.0.003).
    // Falls back to CARGO_PKG_VERSION when not in a git checkout or no tag is present.
    let cargo_version = env!("CARGO_PKG_VERSION");
    let full_version = std::process::Command::new("git")
        .args(["describe", "--tags", "--match", "v*", "--abbrev=0"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() { String::from_utf8(o.stdout).ok() } else { None })
        .map(|s| s.trim().trim_start_matches('v').to_string())
        .unwrap_or_else(|| cargo_version.to_string());

    println!("cargo:rustc-env=APP_VERSION={full_version}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");
    println!("cargo:rerun-if-changed=assets/colormemuch.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(target_os = "windows")]
    {
        // Generous stack for audio/WinRT callbacks downstream.
        println!("cargo:rustc-link-arg=/STACK:8388608");

        let mut res = winres::WindowsResource::new();

        // The manifest requests requireAdministrator, because the Acer WMI
        // class is admin-only. Embed it in release builds only.
        //
        // winres links its resource into *every* binary target, test harnesses
        // included — so embedding unconditionally makes `cargo test` die with
        // "requires elevation" (os error 740) before running a single test,
        // and makes the suite unrunnable in CI, which cannot elevate.
        //
        // Nothing is lost by skipping it in debug: the manifest only requests
        // *auto*-elevation. A binary without one still runs with full rights
        // when launched from an already-elevated shell, which is how you drive
        // real hardware during development anyway.
        let is_release = std::env::var("PROFILE").as_deref() == Ok("release");
        if is_release {
            res.set_manifest_file("assets/colormemuch.manifest");
        }

        res.set("ProductName", env!("CARGO_PKG_NAME"));
        res.set("FileDescription", env!("CARGO_PKG_DESCRIPTION"));
        res.set("FileVersion", &full_version);
        res.set("ProductVersion", &full_version);
        let ico = std::path::Path::new("assets/icon.ico");
        if ico.exists() {
            res.set_icon("assets/icon.ico");
        }
        let _ = res.compile();
    }
}
