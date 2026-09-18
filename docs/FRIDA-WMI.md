# Frida WMI tracing (PO5-660 reverse engineering)

**Goal.** Capture PredatorSense's exact `AcerGamingFunction` WMI transactions
(method + bytes) by hooking `IWbemServices::ExecMethod` — passive observation,
zero firmware risk. Every byte the driver (`src/dt.rs`) replays comes from one
of these captures; nothing is guessed. See `src/dt.rs` for what each captured
sequence proved.

## Environment

Isolated venv at `.venv-frida/` (git-ignored, never commit it):

- Base Python 3.11.9 (`C:\Users\bdjaz\AppData\Local\Programs\Python\Python311`),
  `include-system-site-packages = false`
- `frida` Python package **17.18.0** (the venv `Scripts/` also carries the
  `frida-*.exe` companions)

Recreate from scratch:

```powershell
python -m venv .venv-frida
.\.venv-frida\Scripts\Activate.ps1
pip install frida==17.18.0
```

## Tracing

Two entry points, same hook script (`scripts/frida_wmi_trace.js` hooks
`CoCreateInstance` → `IWbemLocator::ConnectServer` → `IWbemServices::ExecMethod`,
dumps the `input` params including `VT_ARRAY|VT_UI1` byte blobs and decimal
u64s with hex):

**A. Launcher** — spawns a fresh, hooked OpenRGB server:

```powershell
.\.venv-frida\Scripts\python.exe scripts\run_frida_wmi_trace.py
```

Refuses to start while an `OpenRGB.exe` already runs (kill Acer's first —
note this disturbs live lighting; PredatorSense restores it). Leave the
window open, drive PredatorSense, Ctrl+C stops.

⚠️ The launcher hardcodes the Acer-bundled server path:

```python
OPENRGB = r"C:\WINDOWS\System32\DriverStore\FileRepository\predatorservice.inf_amd64_c95114a8e07b0c8a\OpenRGB.exe"
```

The `predatorservice.inf_amd64_<hash>` suffix changes with driver versions —
after a PredatorSense/driver update, re-locate `OpenRGB.exe` under
`FileRepository\predatorservice.inf_*` and update the constant.

**B. Manual attach** — hook a live backend service instead (no restarts, no
lighting disturbance):

```powershell
.\.venv-frida\Scripts\python.exe -m frida -p <PID> -l scripts\frida_wmi_trace.js
```

Candidate PIDs: `AcerCentralService`, `AcerHardwareService`,
`AcerAgentService` (PredatorSense UWP talks named pipes to these; one of them
makes the WMI calls). Needs an elevated shell to attach to SYSTEM services.

## Capture protocol

Every capture is worthless without its label. For each one, record:

1. **Scope** — which PredatorSense zone(s): global / front / top / rear.
2. **Before → after** — exact old color + mode → new color + mode
   (e.g. "global static red → static green (60, 240, 60)").
3. **Timestamp** — the `[2026-…Z][ExecMethod]` line closest *after* your click
   (each call is logged twice — same `pInParams`, same thread — count
   transactions, not lines).

What good looks like (static red → static green, global):

```text
[2026-09-18T20:47:35.009Z][ExecMethod]
  method: SetGamingLedBehavior
  input.input vt=0x2011 value=bytes[16] 010001000f0303000000000000000000
[2026-09-18T20:47:35.060Z][ExecMethod]
  method: SetGamingRgbSetting
  input.input vt=0x8 value="854582263283713" (u64=0x0003093cf03c0001)
```

i.e. behavior array first, color u64 ~50 ms later, same thread — that ordering
is part of the transaction (`dt::apply_static_global` replicates it).

## Notes

- u64 params travel as **decimal strings** (`VT_BSTR`) — WMI convention, not a
  bug. (An early script revision had a doubled-backslash regex that suppressed
  the `(u64=0x…)` suffix; fixed.)
- `Get*` calls are never hooked output — only `ExecMethod` inputs. Pair every
  capture with `scripts/probe_dt_led.ps1` snapshots (`dt-pre.txt`/`dt-post.txt`)
  for the read-back side.
- Capture files (`dt-*.txt`, `kbmatrix-*.txt`, `Logfile*.CSV`) are git-ignored
  by policy — keep the scripts, not the noise.
