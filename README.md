# colormemuch

**Full control of every RGB feature on the Acer Predator Helios Neo 16S AI (PHN16S-71)** — from the command line, from a script, and from a tray app. What PredatorSense won't expose, this will.

## Scope

| Light | Target |
|---|---|
| 4-zone RGB keyboard backlight | Full — effect + per-zone color + speed + direction + brightness |
| **Predator "shield" lid logo** | Full — on/off + color + brightness (and any effect the firmware supports) |
| Power/status LEDs | Read-only (not user-controllable in firmware) |

## Why

PredatorSense is the sanctioned UI but it (a) can't be scripted, (b) fights other services for the driver, and (c) hides the lid shield behind its own opinionated presets. `colormemuch` drives the same lighting hardware directly and exposes it as first-class knobs — for automation, WSL session hooks, focus modes, game launches, whatever.

## Approach — how the lighting is actually driven

The original plan was the `AcerGamingFunction` WMI interface (GUID
`7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56`). **That turned out to be a dead end for
lighting** — every WMI colour write is *accepted* but never reaches the LEDs;
that class governs fans/thermals/profiles, not the backlight. Reverse
engineering (see `scripts/*.ps1` and the session memory) established the real
path:

**Acer bundles OpenRGB.** The PredatorSense package ships `OpenRGB.exe` and runs
it as a local SDK server on `127.0.0.1:6742`; `AcerLightingService` is itself an
OpenRGB *client*. The LEDs are three OpenRGB controllers — `AcerHIDKeyboard`
(4 zones), `AcerHIDCoverLogoLED` (the lid "shield"), and `AcerHIDModeKeyLED`.

So colormemuch speaks the **OpenRGB SDK** directly (`src/openrgb.rs`, a small
blocking `std::net` client — no elevation needed). This is *more* capable than
the WMI route: the lid shield the Linux drivers never mapped is a first-class
controller here.

**Prior art that got us there:**

- [JafarAkhondali/acer-predator-turbo-and-rgb-keyboard-linux-module](https://github.com/JafarAkhondali/acer-predator-turbo-and-rgb-keyboard-linux-module) — decoded the 4-zone keyboard payload
- [FelipeFMA/nekro-sense](https://github.com/FelipeFMA/nekro-sense) — the PHN16-72 sibling; confirmed the enable-flag byte
- [OpenRGB](https://openrgb.org) — the actual mechanism Acer ships and colormemuch talks to

## Upstream

This project was scaffolded from **[ophiocus/rust-skeleton](https://github.com/ophiocus/rust-skeleton)** and tracks it as its `upstream` remote. Skeleton improvements (build script, WiX installer, self-updater, egui shell) flow down via:

```powershell
git fetch upstream
git merge upstream/master   # resolve conflicts in project-owned files
```

Distilled learnings from this project — packaging, WMI-call patterns, tray-app boilerplate — should get retrofitted back upstream when they're generic (see the retrofit rule in the user's global CLAUDE.md).

## Status

✅ **Working desktop app.** Keyboard (4 zones) and the lid shield are both under
full control via a native egui screen. Feature set:

- **Three effect types.** *Core* — firmware effects (Neon/Wave/Meteor/…),
  whole-device, **zero host CPU**. *Program* — built-in `fn(t,n)` effects
  (Rainbow, Comet, Fire, …), per zone. *Custom* — user-built palette + motion
  effects, per zone, **fully editable in-app** and saved to
  `%APPDATA%/ColorMeMuch/effects.json`.
- **Per-zone independent control** on one **shared clock**: two zones running
  the same function are phase-locked with no drift; **Link** (identical) or
  **Spread** (the effect travels across the zones) per group.
- **Persistence** — your whole setup (per-device mode, per-zone assignments,
  master brightness, Link/Spread) is restored on launch.
- **Efficient** — adaptive frame rate per effect; static zones and idle devices
  send nothing. Full animation costs ≈0.5% CPU.

### Source map

| File | Role |
|---|---|
| `src/openrgb.rs` | OpenRGB SDK client — enumerate controllers, set LEDs, invoke firmware effects |
| `src/effects.rs` | `Fx` (Program/Custom), pure `fn(t,n)` effects, shared-clock render pipeline |
| `src/library.rs` | Custom effect schema (palette + motion) + on-disk library |
| `src/ui.rs` | The egui control screen: device panel, per-zone cards, effect editor |
| `src/rgb.rs`, `src/wmi.rs` | The WMI layer — retained for fan/thermal/profile reads (not lighting) |
| `scripts/*.ps1` | The reverse-engineering toolkit that found the OpenRGB path |

### Not yet built

- CLI (`colormemuch set --keyboard rainbow …`) for scripting.
- System-tray + global hotkeys / automation triggers.
- Optional: move animation to a worker thread so the keyboard keeps animating
  while the window is minimized.

## Build

Standard rust-skeleton flow:

```powershell
cd D:\colormemuch
cargo run           # dev
cargo build --release
.\scripts\build_msi.ps1   # produces the WiX installer
```
