# colormemuch — project context

Windows desktop app (Rust + eframe/egui) giving full scripted control of every
RGB feature on the **Acer Predator Helios Neo 16S AI (PHN16S-71)** — 4-zone
keyboard backlight *and* the lid "shield" logo that PredatorSense hides behind
presets.

**Read [README.md](README.md) first.** It carries the scope table, the `why`,
the prior-art links, and the numbered next steps. This file only records what
the README doesn't: how to work in here.

## Where things are

| Path | Role |
|---|---|
| `src/main.rs` | Entry point + the three identity constants (`APP_NAME`, `APP_WINDOW_TITLE`, `APP_GH_REPO`). Single source of truth — don't duplicate the app name elsewhere. |
| `src/app.rs` | egui shell: installs the theme, draws the top toolbar (status pill, ⚙ settings popover with the updater), hosts `ui::RgbControl`, tray/close-to-tray. |
| `src/ui/` | The screen. `theme.rs` (tokens, Inter type scale, egui `Style`) and `widgets.rs` (segmented control, toggle, card, pill, list row, toast…) are the design system; one file per view: `sidebar`, `toolbar`, `case` (PO5-660 tower), `zones` (per-zone / hardware editor), `library` (effect editor + inspector). `mod.rs` owns state + `setup.json` persistence. Never hard-code colors or sizes in a view — reach for `theme::tokens`. |
| `assets/fonts/` | Inter (Regular/Medium/SemiBold, OFL — `LICENSE.txt` alongside), embedded via `include_bytes!`. |
| `src/config.rs` | JSON config persisted under `%APPDATA%`. |
| `src/git_update.rs` | Self-updater — polls the GitHub releases API, downloads the `.msi`. |
| `build.rs` | Derives `APP_VERSION` from the latest `v*` git tag; embeds the winres icon. |
| `wix/main.wxs`, `scripts/build_msi.ps1` | WiX MSI packaging. |
| `.github/workflows/release.yml` | Tag `v*` → build MSI → attach to the GitHub release. |

The RGB layer does not exist yet. It is next, and it lands as new modules
(`src/wmi.rs`, `src/rgb.rs`, `src/cli.rs`) — the files above are skeleton
plumbing and should stay generic.

## Working rules

- **Build and run from this directory on Windows.** `cargo run` for dev,
  `.\scripts\build_msi.ps1` for the installer. This is a Windows-native
  project — do not route it through WSL.
- **Hardware writes are irreversible-ish.** WMI calls into
  `AcerGamingFunction` (GUID `7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56`) touch
  firmware state on the user's actual laptop. When probing undocumented bytes
  — especially the lid-shield family — propose the payload and confirm before
  firing it. Observe-then-replay beats guess-then-brick.
- **PredatorSense fights for the driver.** If a WMI call returns garbage or
  hangs, check whether PredatorSense / its services are running before
  concluding the payload is wrong.
- **Versioning is git-tag-driven.** Bumping a version means tagging `vX.Y.Z`,
  not editing `Cargo.toml` alone. `build.rs` reads the tag.
- **Default branch is `master`** (all ophiocus repos), and git identity here is
  personal: `1540596+ophiocus@users.noreply.github.com`. Remotes use the
  `github-ssdnodes` SSH alias — never rewrite one to HTTPS.

- Adopt [conventional commits](https://www.conventionalcommits.org/) for all git messages.

## Upstream / retrofit

`upstream` is [ophiocus/rust-skeleton](https://github.com/ophiocus/rust-skeleton).
Improvements to the *generic* half — build script, WiX packaging, self-updater,
egui shell, CI — get pushed back up in the same session they're learned. The
RGB/WMI half is project-specific and stays here. Skeletons never name a
sibling project; keep retrofits phrased as patterns.

See the retrofit protocol and the repo taxonomy in the user's global
`~/.claude/CLAUDE.md` before deciding where a change belongs.
