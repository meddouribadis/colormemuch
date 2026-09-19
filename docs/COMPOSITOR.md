# The lighting compositor — architecture

colormemuch's answer to two coupled problems:

1. **Multi-layered color strategy.** Composing lighting from independent layers
   (identity → zones → context → moments) that flatten top-down into one frame.
2. **Permanence.** Making a profile *survive* — the app closing, a reboot, and
   Acer's `AcerLightingService` repainting its own profile on top of ours.

Both are the same design: a **compositor** that produces frames, and a
**persistence spine** that decides where each layer lives and how hard it fights
to stay.

## What we're fighting (see the session memory)

`AcerLightingService` (auto-start, SYSTEM) launches Acer's own bundled
`OpenRGB.exe` as a **child process**, listening on `127.0.0.1:6742` — the exact
server colormemuch connects to. The service reasserts the PredatorSense profile
on discrete events (resume, AC/battery, its own periodic repaint). OpenRGB
DIRECT mode is a live *stream*, never written to firmware — so when our client
disconnects, the last frame lingers only until the service's next repaint.

**Consequence that shapes everything:** because the OpenRGB server is a *child*
of the service, we cannot "just stop `AcerLightingService`" — that kills our
transport. We win by out-timing the reassert, or by moving state into hardware.

## Persistence tiers

Every layer declares where it lives. This is the spine.

| Tier | Mechanism | Survives app close | Survives reboot | Cost |
|---|---|---|---|---|
| **Live** | DIRECT stream from the GUI thread | ❌ | ❌ | frames only while visible |
| **Daemon** | engine thread holds + re-pushes | ✅ (tray process) | ❌ | one resident thread |
| **Firmware** | `SaveMode` to keyboard flash | ✅ | ✅ (until Acer's next event-repaint) | 0 CPU |

The three effect tiers the app already has map straight onto these:
**Core/Hardware → Firmware**, **Program/Custom (animated) → Daemon**,
**transient previews → Live**.

## Modules

| File | Role |
|---|---|
| `src/engine.rs` | **new.** `LightingEngine` on its own thread. Owns the `OpenRgb` connection. Message-driven (`EngineCmd`). Runs the compositor loop, the **hold** reassert timer, and firmware saves. Decouples lighting cadence from egui repaints — the reason CPU drops *and* colors survive a hidden window. |
| `src/openrgb.rs` | add `save_mode()` (protocol cmd 1101) + mode-flag constants, so a Core/Static profile can be written to the keyboard's flash. |
| `src/app.rs` | owns the engine + the tray icon; routes window-close to hide-to-tray so the engine keeps holding; forwards UI state to the engine over a channel instead of pushing frames itself. |
| `src/ui.rs` | reshaped into the compositor: per-zone cards (unchanged core) **+** the persistence spine (ownership, Hold toggle, tier badges, Save-to-keyboard) **+** one Reactive rule. |
| `src/tray.rs` | tray icon + menu (Show / Quit). Polled from `app::update`. |

## The engine loop (Feature 1 — daemon)

```
loop {
  drain EngineCmd channel        // SetState, SetHold, SaveFirmware, Reconnect, Shutdown
  t = clock.elapsed()
  for each device:
     frame = compositor.flatten(device, t)      // render_plan, already exists
     if frame != last || hold_due:              // hold_due every ~3s when Hold on
         if leaving hardware mode -> enter_direct once
         update_leds(frame)                      // re-push beats Acer's repaint
  sleep to the active frame rate (adaptive, existing active_hz logic)
}
```

- **Hold** = re-push every ~3 s even when the frame is unchanged, so within 3 s
  of any Acer reassert (including resume-from-sleep) our profile is back. Cheap:
  three 15-byte writes. Off → we only push on change (today's behavior).
  **Exception — the tower case (WMI, `dt`):** hold does *not* replay it. Each
  WMI transaction blanks its zone while the firmware re-applies the mode, so a
  3 s replay was a 3 s blink. The case is firmware state and keeps itself; the
  engine's `DtTracker` debounces requests and writes only zones whose resolved
  state changed. A read-back-based re-assert (`GetGamingRgbSetting` per area,
  rewrite only on divergence) is the intended follow-up once the per-area echo
  is confirmed on hardware.
- Runs on a worker thread, so a hidden/minimized window keeps holding and the
  GUI thread can idle. **This is what makes colors not vanish when the window
  is closed** — closing hides to tray; only tray-Quit stops the engine.
- Power/session event hooks (`RegisterSuspendResumeNotification`) are a
  refinement; the 3 s hold already recovers from resume. Noted, not v1.

## Firmware-save (Feature 2)

`UPDATE_MODE` (1100) already runs a firmware effect host-free. Adding
**`SAVE_MODE` (1101)** writes the active mode to the keyboard's onboard flash so
a Static color or a hardware Breathe/Wave survives a full power cycle with **no
process running at all**. Gated on the mode's `MODE_FLAG_MANUAL_SAVE` flag — if
the Acer controller doesn't advertise save, the UI says so instead of lying.
This is the true "set and forget" for the Base-identity layer. (Acer may still
repaint on its own next event; pair with Hold if that matters.)

## Compositor UI (Feature 3)

> **Layout note (2026-09):** the right-hand "persistence spine" panel described
> below was folded into the toolbar — a status pill (Held · Service / Holding /
> Live / Offline) plus the Hold and Battery-saver toggles in the ⚙ settings
> popover. The concepts are unchanged; only where they live.

- **Persistence spine** (right panel): connection + ownership line
  (`Owner: colormemuch` / `Acer`), the **Hold against Acer** toggle, a tier
  legend, and per-selection **Save to keyboard** when the mode supports it.
- **Tier badges** on each zone/device: Live / Daemon / Firmware, so the cost and
  permanence of every choice is visible at a glance.
- **Reactive layer (one rule in v1):** *On battery → cap brightness / shift to
  amber.* Proves the context-driven layer above the base without a keyboard
  hook. Read AC vs battery via `GetSystemPowerStatus`.
- **Exclusive mode:** shown but **deferred** — labelled advanced, explains that
  true takeover needs colormemuch's own OpenRGB server (because Acer's is the
  service's child). No footgun that kills our own transport ships in v1.

## Realized architecture: core + daemon + thin client (built 2026-07-25)

The GUI was inverted from "primary, owns a helper thread" to "thin client of an
authoritative daemon", with the core factored into a library both binaries share.
One Cargo package, three targets:

```
  Acer OpenRGB :6742              ← hardware transport (theirs)
        ▲
  colormemuch-svc  (daemon)       ← SINGLE owner: engine, hold, ProgramData store   [service / console]
        ▲  named pipe  \\.\pipe\colormemuch-svc   (length-prefixed JSON, ipc.rs)
  colormemuch      (GUI)          ← thin client: authoring + resolved EngineState    [user session]
```

- **`src/lib.rs` — the core lib** (`colormemuch`): `rgb`, `wmi`, `openrgb`,
  `effects`, `library`, `engine`, plus `ipc`, `host`, `daemon`. No egui — the
  reusable half. The daemon binary links **none** of eframe (1.8 MB vs the GUI's
  18 MB), proving the separation even inside one package.
- **`host::Host`** — the seam. `EmbeddedHost` runs the engine in-process (colors
  die on close — the no-daemon default); `ConnectedHost` is a pipe client
  (colors persist — the daemon owns them). `host::create()` picks the daemon if
  it's serving, else embeds. **Same core, two hosts** — verified: with the
  daemon up the GUI holds no `:6742`; without it the GUI owns `:6742` directly.
- **Wire format = resolved `EngineState`.** The GUI does authoring/resolution
  and ships a rendered snapshot; the daemon just holds it. So the daemon needs
  no library/ZoneUi knowledge, and there is exactly one hardware writer by
  construction — the double-push contention is gone, not arbitrated.
- **Daemon store** — the last applied `EngineState` persists to
  `%ProgramData%/ColorMeMuch/daemon-state.json`, restored at boot so lighting
  comes up before any GUI connects. The reason a service exists.
- **Lifecycle** — `colormemuch-svc [console|run|install|uninstall]`. `console`
  serves in the foreground (dev); `install` registers the auto-start Windows
  service (LocalSystem) via `windows-service`.

**Cross-session pipe (done).** The daemon serves a **raw Win32 named pipe**
(not `interprocess`, which can't set security) created with an SDDL descriptor —
`D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)`: full for LocalSystem/Admins,
read/write for Authenticated Users — so a user-session GUI can reach the pipe a
LocalSystem service owns. The GUI stays on the `interprocess` client; a
`PipeStream` (ReadFile/WriteFile over the HANDLE) feeds the same `ipc` framing,
and a self-connect poke releases the blocked `ConnectNamedPipe` at shutdown.
Verified via `console` mode; the elevated `install` path uses the same server.

**Still deferred:** verifying the installed LocalSystem service end to end (needs
an elevated `install`), and a live connected→embedded failover (today a dropped
daemon shows "connection lost" + Retry re-runs the factory).

## What v1 delivers vs. defers

**v1:** engine/daemon with Hold, hide-to-tray permanence, `SaveMode` firmware
persistence, the spine + tier badges, the battery Reactive rule.

**Deferred (roadmap):**

- **Next iteration — persist *user* functions.** Today "Keep my lighting" holds
  only firmware modes; non-firmware (Program/Custom, streamed) effects gray out
  while it's on. The next step is holding those too — a resident re-push of the
  animated frame that survives close, so a custom effect can be a permanent
  profile, not just firmware statics.
- **Next iteration — advanced user functions:** keystroke ripples and
  game-light / reactive custom functionality (the **Moment** layer). Needs a
  low-level keyboard/input hook feeding the compositor's top layer.
- suspend/resume event hooks (tighter than the 3 s hold), and true Exclusive
  mode (colormemuch's own bundled OpenRGB server, since Acer's is a child of its
  lighting service).

Each is additive on this spine.
