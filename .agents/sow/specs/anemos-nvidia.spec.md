# Spec: `nvidia` anemos

Status: design. NVIDIA GPU onboard-fan controller. Conforms to `aiolos-protocol.spec.md`.

## Purpose
Per-GPU fan control via NVML. One `run` instance per physical GPU, bound by GPU **UUID**
(stable across NVML index renumbering and across the GPU dropping/returning on the bus).

## detect
- Enumerate GPUs via NVML; emit one `found` per GPU:
  `{"id":"<GPU-UUID>","type":"GPU","name":"<product name>"}`.
- Persistent; re-`detect` reflects current set (a GPU that fell off the bus disappears; on return
  it reappears with the same UUID).
- **Fork-safety:** NVML is not fork-safe. The orchestrator never holds NVML; each process that
  uses NVML calls `nvmlInit` itself after being spawned.

## run <UUID>
- `nvmlInit`; resolve the device by UUID.
- On each `apply`: read this GPU's temperature; interpolate the curve from
  `/opt/aiolos/etc/nvidia.curve.json`; set the GPU's onboard fan(s) via NVML
  (`SetFanSpeed`-equivalent); report:
  ```json
  {"status":"ok","readings":[
    {"type":"temp","label":"GPU","temp":63},
    {"type":"fan","label":"fan0","pwm":72,"rpm":2200},
    {"type":"fan","label":"fan1","pwm":72,"rpm":2210}]}
  ```
- `inputs` are ignored (nvidia uses its own GPU temperature).
- On GPU-lost (NVML error): respond `{"status":"error","error":"gpu lost"}`; the orchestrator
  will reconcile via the next `detect`.

## Implementation note (binding)
Rust via the `nvml-wrapper` crate (≥ 0.12.1; loads `libnvidia-ml.so.1` at runtime — no link-time
dep, no raw FFI). Used: `Nvml::init`, `device_count`, `device_by_index`, `Device::uuid`/`name`/
`num_fans`/`temperature(Gpu)`/`fan_speed`/`fan_speed_rpm`/`set_fan_speed`/`set_default_fan_speed`.
Fans are set/restored **per fan index** (`0..num_fans`), not per GPU.

## Fail-safe
On `shutdown` or stdin EOF: restore the GPU to firmware/default fan control
(`set_default_fan_speed` per fan) and exit. **Critical:** NVML manual fan control PERSISTS after
the process exits — the driver does NOT auto-revert — so restore is mandatory and is also wired
into the `Gpu` value's `Drop` (covers panic unwinding). A direct `SIGKILL` of the run process
cannot restore in-process (same gap as the C `nvfd`); the orchestrator respawns the instance,
which re-takes control. The configured curve must be more aggressive than firmware default so a
failure degrades to the (safe, lazier) firmware curve. If the curve is missing/empty the module
holds firmware/default control rather than commanding 0%.

## Config — `/opt/aiolos/etc/nvidia.curve.json`
Temperature °C → fan %, linear-interpolated, clamped, hold-outside:
```json
{"0":0,"80":100}
```
(Current production value: linear 0–80 °C → 0–100 %.)

## Acceptance criteria
- Detects all GPUs by UUID; per-GPU `run` instances are independent processes.
- Fan % tracks the curve for the measured temperature (±ramp lag).
- A GPU falling off the bus does not affect the other GPU's instance.
- shutdown/EOF restores firmware auto; verified by reading fan policy after exit.
