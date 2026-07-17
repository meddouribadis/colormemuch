# colormemuch

**Full control of every RGB feature on the Acer Predator Helios Neo 16S AI (PHN16S-71)** — from the command line, from a script, and from a tray app. What PredatorSense won't expose, this will.

## Scope

| Light | Target |
|---|---|
| 4-zone RGB keyboard backlight | Full — effect + per-zone color + speed + direction + brightness |
| **Predator "shield" lid logo** | Full — on/off + color + brightness (and any effect the firmware supports) |
| Power/status LEDs | Read-only (not user-controllable in firmware) |

## Why

PredatorSense is the sanctioned UI but it (a) can't be scripted, (b) fights other services for the sensor driver, and (c) hides the lid shield behind its own opinionated presets. `colormemuch` talks to the same **`AcerGamingFunction` WMI interface** PredatorSense uses (GUID `7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56`) and exposes it as first-class knobs — for automation, WSL session hooks, focus modes, game launches, whatever.

## Approach

The RGB protocol has already been reverse-engineered by the Linux community — this project ports that work to Windows and wraps it in a clean Rust binary + tray UI.

**Prior art / references:**

- [JafarAkhondali/acer-predator-turbo-and-rgb-keyboard-linux-module](https://github.com/JafarAkhondali/acer-predator-turbo-and-rgb-keyboard-linux-module) — canonical Linux kernel module + `facer_rgb.py` CLI; decoded 4-zone keyboard protocol
- [FelipeFMA/nekro-sense](https://github.com/FelipeFMA/nekro-sense) — closest sibling (PHN16-72), most likely byte-for-byte compatible
- [cleyton1986/predator-sense](https://github.com/cleyton1986/predator-sense) — Rust + GTK4 reimplementation (Linux), transferable design patterns
- [InvertedOwl/Acer-SenSe](https://github.com/InvertedOwl/Acer-SenSe) — Predator + Nitro coverage

## Upstream

This project was scaffolded from **[ophiocus/rust-skeleton](https://github.com/ophiocus/rust-skeleton)** and tracks it as its `upstream` remote. Skeleton improvements (build script, WiX installer, self-updater, egui shell) flow down via:

```powershell
git fetch upstream
git merge upstream/master   # resolve conflicts in project-owned files
```

Distilled learnings from this project — packaging, WMI-call patterns, tray-app boilerplate — should get retrofitted back upstream when they're generic (see the retrofit rule in the user's global CLAUDE.md).

## Status

🚧 Scaffolded. Next steps:

1. Bind `AcerGamingFunction` WMI methods (`SetGamingRgbKb`, `SetGamingLED`, `SetGamingLEDColor`, `SetGamingLEDBehavior`, `SetGamingKBBacklight`) via `windows` / `wmi` crates.
2. Port `facer_rgb.py`'s payload builder to Rust (4-byte-per-zone static + 16-byte effect packet).
3. Observe-then-replay the **lid shield** byte (SetGamingLED family — the piece the Linux drivers didn't map).
4. CLI first (`colormemuch set --keyboard breath --color #ff00aa --speed 5`), then a minimal egui tray.

## Build

Standard rust-skeleton flow:

```powershell
cd D:\colormemuch
cargo run           # dev
cargo build --release
.\scripts\build_msi.ps1   # produces the WiX installer
```
