# P1 — Discoverable controller layer (the device descriptor)

**Goal.** Stop discarding the OpenRGB descriptor. Today `parse_controller` reads
a controller's basics + modes + zone names/counts and **throws away** the
controller type, zone types, the matrix map, per-LED names, and the current
colors — then the GUI flattens `led_count` into N identical cards. OpenRGB's SDK
is "RGBController over IP": the client object is meant to be a faithful copy of
the server's. P1 makes it one. See `docs/COMPOSITOR.md` for the surrounding
architecture and the online prior-art notes.

**P1 is the data + discovery layer only.** It retains the whole descriptor,
ships it to the GUI, and exposes it as a first-class, inspectable thing. It does
**not** change how you edit lighting yet.

## Scope

In:
- Full descriptor parse (types, zone types, matrix map, LED names, current colors).
- A serializable `DeviceDescriptor` that supersedes `engine::ControllerInfo`.
- A read-only **Devices inspector** in the GUI + a `colormemuch-svc list` dump.
- Real names everywhere (retire the `short_name()` string-munging; label zone
  cards with the descriptor's actual LED names).
- Profile keying on a **topology signature** so profiles bind to the right device.

Out (later phases):
- Capability-gated *edit* controls — hiding the Speed slider when a mode has no
  `has_speed`, etc. (**P2**).
- Widget-per-zone-type: matrix grid, strip control (**P2**).
- Topology-aware composition / canvas sampling (**P3**).
- Live hot-plug re-discovery (**P4**).
- Any change to the write path (`update_leds`, `apply_effect`) or the effect
  model. Untouched.

## Data model (new `src/model.rs`)

The serializable projection the GUI consumes. Mirrors OpenRGB's structures.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceDescriptor {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub location: String,
    pub serial: String,
    pub kind: DeviceKind,          // keyboard / mouse / dram / gpu / ledstrip / …
    pub zones: Vec<ZoneInfo>,
    pub leds: Vec<LedInfo>,        // parallel to the flat LED/color vectors
    pub modes: Vec<ModeInfo>,
    pub active_mode: usize,
    pub colors: Vec<Rgb>,          // current per-LED colors (see caveat)
    pub led_count: u16,            // == leds.len(); kept for the write path
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZoneInfo {
    pub name: String,
    pub kind: ZoneKind,            // Single | Linear | Matrix
    pub start: u32,               // start index into the flat LED vector
    pub leds_count: u32,
    pub leds_min: u32,
    pub leds_max: u32,
    pub matrix: Option<Matrix>,   // Some only when kind == Matrix
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Matrix {
    pub height: u32,
    pub width: u32,
    /// row-major, height*width; NONE (0xFFFFFFFF) marks a gap (e.g. under the
    /// spacebar). Values index into the zone's LEDs.
    pub map: Vec<Option<u32>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedInfo { pub name: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModeInfo {
    pub name: String,
    pub flags: u32,               // raw OpenRGB flags
    pub has_speed: bool,
    pub has_brightness: bool,
    pub has_direction: bool,
    pub takes_color: bool,        // per-LED OR mode-specific color
    pub can_save: bool,           // manual or automatic save
    pub speed_range: (u32, u32),
    pub brightness_range: (u32, u32),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum DeviceKind { Motherboard, Dram, Gpu, Cooler, LedStrip, Keyboard,
    Mouse, Mousemat, Headset, Gamepad, Light, Case, Storage, Accessory, Unknown }

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum ZoneKind { Single, Linear, Matrix }
```

`DeviceKind::from_i32` / `ZoneKind::from_i32` map the raw OpenRGB enum ints
(device type 0..=19, zone type 0..=2), unknown → `Unknown`/`Linear`.

## Parser changes (`src/openrgb.rs`)

Enrich `Controller` and continue the cursor past where it stops today. The bytes
are already on the wire (protocol v4, which the current parser assumes) — we're
just reading the sections we currently skip.

1. **Controller type**: the `type` i32 after `data_size` is currently read into
   `_type` and dropped. Keep it.
2. **Zones**: today each zone reads name, `_zone_type`, min, max, count, then
   *skips* `matrix_len` bytes. Instead: keep the zone type, and when it's Matrix
   (matrix_len > 0) parse `height: u32, width: u32, [u32; height*width]`
   (`0xFFFFFFFF` → `None`). Track `start` (running LED offset).
3. **LEDs section** (currently the parse ends right before this): `num_leds: u16`,
   then per LED `name: string, value: u32`. Retain names.
4. **Colors section**: `num_colors: u16`, then `[u32; num_colors]`. Decode with
   the existing `color_u32` inverse into `Rgb`.

Add `DeviceDescriptor::of(&Controller)` (mirrors today's `ControllerInfo::of`).
The engine keeps using the rich `Controller` internally for writes.

**Caveat to document in-code:** `colors` is OpenRGB's *model* (last-set state),
not a guaranteed live hardware sample — reliable for "what OpenRGB thinks is
shown," not a firmware read-back.

## Wire / IPC (`engine.rs`, `ipc.rs`, `host.rs`)

- Delete `engine::ControllerInfo`; replace all uses with `model::DeviceDescriptor`.
- `EngineEvent::Connected(Vec<Controller>)` unchanged (engine-side); hosts convert
  via `DeviceDescriptor::of`.
- `ipc::ServerMsg::Status.controllers: Option<Vec<DeviceDescriptor>>`.
- `HostEvent::Connected(Vec<DeviceDescriptor>)`. `ConnectedHost` already de-dupes
  by name signature — widen that to a topology signature (below).

The GUI reads a strict **superset** of today's `ControllerInfo` (it still has
`name`, `led_count`, mode names), so `ui.rs` compiles with minimal edits; the new
fields are additive.

## Discovery surfaces

1. **`colormemuch-svc list`** — a new daemon subcommand that connects read-only,
   enumerates, and prints the descriptor tree (controller → kind, zones →
   kind/count/matrix dims, LEDs → names, modes → flags). This is the diagnostic
   we sketched; it doubles as the golden-output source for tests. Read-only; no
   writes.
2. **In-app Devices inspector** — a read-only panel/window rendering the same
   tree from the live `DeviceDescriptor`. This is the literal "the controller
   layer is a discoverable part of the program." No editing here — it's the
   truth view. (The curated compositor stays the edit surface.)
3. **Real names** — drop `short_name()`'s `AcerHID`/`CoverLogoLED` string
   surgery in favor of the descriptor's `name`; label per-zone cards with the
   real `LedInfo.name` (e.g. "Keyboard Backlight zone 1") instead of "Zone 1".
   Cheap win, uses only retained data.

## Persistence (`ui.rs` `LightingSetup`)

Today devices are keyed by controller `name`, and restore requires
`ds.zones.len() == led_count`. Replace the match key with a **topology
signature** = hash of `(name, kind, [(zone.name, zone.kind, zone.leds_count)])`.
This binds a saved profile to the right device even if two share a name, and
cleanly refuses to restore onto a changed topology. Migration: on load, if an
old-format entry (name-keyed) matches by name + led_count, adopt it under the new
key once, then re-save.

## Testing

- **Golden parse test** (`openrgb.rs`): capture the real Acer blob bytes (the
  `list` probe already proves the layout) into a fixture; assert the parser
  yields 3 controllers, zones `[Linear/…]`, LED names
  `["Keyboard Backlight zone 1"..4, "Mode Key LED 1", "Cover Logo LED 1"]`,
  6 LEDs total, and modes with the expected flags. This locks the new sections.
- **serde round-trip** for `DeviceDescriptor` (it crosses the pipe).
- **Matrix parse** unit test against a synthetic matrix zone blob (height×width +
  a `0xFFFFFFFF` gap) — since no local device has one, this guards P3's future.
- Existing effect/engine tests untouched (write path unchanged).

## Files touched

`src/openrgb.rs` (parser + rich Controller), **new** `src/model.rs`, `src/lib.rs`
(mod), `src/engine.rs` (drop ControllerInfo), `src/ipc.rs`, `src/host.rs`,
`src/ui.rs` (inspector + real names + topology-keyed persistence),
`src/bin/colormemuch-svc.rs` (`list`).

## Risk / rollout

Additive and low-risk: the hot write path (`update_leds`/`apply_effect`) and the
effect model are untouched; the GUI consumes a superset. The one behavioral
change is persistence keying, gated by a one-time migration. Verifiable entirely
on this hardware via the golden test + the `list` dump, even though the payoff
(matrix/LED-name richness) mostly benefits other devices.

## Definition of done

- `colormemuch-svc list` prints the full descriptor tree for the 3 Acer devices.
- The GUI's Devices inspector shows the same, live.
- Zone cards carry real LED names; `short_name()` is gone.
- Profiles restore via topology signature; old profiles migrate once.
- Golden + round-trip + matrix tests green; existing suite still green.
