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

## Fail-safe
On `shutdown` or stdin EOF: restore the GPU to firmware/default fan control
(`SetDefaultFanSpeed`-equivalent) and exit. The configured curve must be more aggressive than
firmware default so a failure degrades to the (safe, lazier) firmware curve.

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
