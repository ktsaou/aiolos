# Spec: `nvidia` anemos

Status: design. NVIDIA GPU onboard-fan controller. Conforms to `aiolos-protocol.spec.md`.

## Purpose
Per-GPU fan control via NVML. One `run` instance per physical GPU, bound by GPU **UUID**
(stable across NVML index renumbering and across the GPU dropping/returning on the bus). When the
registry wires routed temperature inputs into `nvidia`, the GPU fans follow the maximum of this GPU's
own temperature and all routed temperature producer values. This can support watercooled GPUs whose
radiator/fan array also provides useful chassis airflow for CPU/board heat.

## detect
- Enumerate GPUs via NVML; emit one `found` per GPU:
  `{"id":"<GPU-UUID>","type":"GPU","name":"<product name>","components":[...]}` with one
  `gpu` component schema: temperature publisher, per-fan duty/RPM publishers, and a `fans`
  `fan-duty` sink.
- Persistent; re-`detect` reflects current set (a GPU that fell off the bus disappears; on return
  it reappears with the same UUID).
- **Fork-safety:** NVML is not fork-safe. The orchestrator never holds NVML; each process that
  uses NVML calls `nvmlInit` itself after being spawned.

## run <UUID>
- `nvmlInit`; resolve the device by UUID.
- On each `apply`: read this GPU's temperature; scan routed `inputs` for producer signals whose
  `type`/kind is `temperature`; drive from `max(own GPU temperature, routed temperature inputs)`;
  interpolate the curve from `/opt/aiolos/etc/nvidia.curve.json`; set the GPU's onboard fan(s) via
  NVML (`SetFanSpeed`-equivalent); report one live `gpu` component:
  ```json
  {"status":"ok","components":[{
    "id":"gpu","label":"GPU-...","class":"gpu",
    "publishers":[
      {"id":"temp","label":"Temperature","kind":"temperature","value":63,"unit":"C"},
      {"id":"fan0.duty","label":"fan0 duty","kind":"fan-duty","value":72,"unit":"%"},
      {"id":"fan0.rpm","label":"fan0 RPM","kind":"fan-rpm","value":2200,"unit":"rpm"}],
    "sinks":[{"id":"fans","label":"GPU fans","kind":"fan-duty","value":72,"unit":"%",
      "safe":"auto","needs_claim":true,"state":"claimed",
      "driven_by":[{"name":"gpu0","signal":"GPU-...:temperature:temp","value":63,"uom":"C"}],
      "driving":{"type":"temperature","raw":63,"input":63,"uom":"C","output":72,"how":"self→curve"}}]}]}
  ```
- If no routed temperature inputs are present, behavior is unchanged: `nvidia` uses only this GPU's own
  temperature.
- If routed temperature inputs are present, non-temperature signals are ignored. Fan duty/RPM signals
  must never influence the NVIDIA fan decision.
- Routed temperature inputs are opt-in. A workstation may keep `nvidia` GPU-only when the GPU
  radiator/fan array does not materially improve board or CPU cooling; board and case fans can still
  consume `nvidia` temperatures independently.
- On GPU-lost (NVML error): respond `{"status":"error","error":"gpu lost"}`; the orchestrator
  will reconcile via the next `detect`.

## Implementation note (binding)
Rust via the `nvml-wrapper` crate (≥ 0.12.1; loads `libnvidia-ml.so.1` at runtime — no link-time
dep, no raw FFI). Used: `Nvml::init`, `device_count`, `device_by_index`, `Device::uuid`/`name`/
`num_fans`/`temperature(Gpu)`/`fan_speed`/`fan_speed_rpm`/`set_fan_speed`/`set_default_fan_speed`.
Fans are set/restored **per fan index** (`0..num_fans`), not per GPU.

## Modes
`detect` · `info [id]` / `collect [id]` · `run <UUID>` · `restore` (one-shot: restore EVERY GPU's fans to firmware default and
exit; idempotent; called by `aiolos restore`).

## Fail-safe
On `shutdown`, stdin EOF, OR `SIGTERM`/`SIGINT`: restore the GPU to firmware/default fan control
(`set_default_fan_speed` per fan) and exit — the module catches the signal itself (self-sufficient).
Additionally, **any failed control tick restores to firmware**: a mid-loop `set_fan_speed` failure
reverts ALL fans to default (`apply_or_restore`), and `run_loop` restores on any tick error (e.g. a
temperature-read failure) so the GPU is never left manual-but-unregulated. **Critical:** NVML manual
fan control PERSISTS after the process exits — the driver does NOT auto-revert — so restore is
mandatory and is also wired into the `Gpu` value's `Drop` (covers panic unwinding). A direct
`SIGKILL` cannot restore in-process; `aiolos restore` (systemd ExecStopPost) is the net, and the
orchestrator otherwise respawns the instance, which re-takes control. The configured curve must be
more aggressive than firmware default so a failure degrades to the (safe, lazier) firmware curve.

**Curve loading (SOW-0012).** Two distinct cases:
- **Invalid curve at startup** (missing file, invalid JSON, or no usable points): the module
  **refuses to start** — it never takes manual control, declares `{"status":"fatal","error":"startup:
  curve …"}` on its first `apply` (so the reason surfaces on the status page), and exits non-zero.
  The GPU stays on firmware/default; aiolos respawns on the `max_backoff` cap until a valid curve
  appears.
- **Curve breaks while running** (file becomes unreadable / invalid / empty during a live edit): the
  module **keeps the last-good curve** and logs a warning **every tick** while it stays broken — an
  in-progress edit never blips the fans. (It never commands 0%.)

## Config — `/opt/aiolos/etc/nvidia.curve.json`
Temperature °C → fan %, linear-interpolated, clamped, hold-outside, plus a `sensitivity` key (the
live EMA α, not a curve point):
```json
{"30":30,"80":100,"sensitivity":0.5}
```
Default: ≤30 °C → 30 %, ≥80 °C → 100 %, linear between — a **30 % floor** so a wrong low reading can
never stop/minimise the fans. (30 % matches the board's firmware idle; lowered from the original
35 % — supersedes SOW-0001 #16.) `sensitivity` (0.5 default; lower =
smoother/less spike-sensitive, higher = more responsive) is reloaded every tick.

## Acceptance criteria
- Detects all GPUs by UUID; per-GPU `run` instances are independent processes.
- Without routed temperature inputs, fan % tracks the curve for the measured GPU temperature (±ramp lag).
- With routed temperature inputs, fan % tracks the curve for `max(GPU temperature, routed temperature
  inputs)` and reports that driving temperature in each claimed fan-duty sink's `driving` metadata.
- Routed non-temperature signals are ignored.
- A GPU falling off the bus does not affect the other GPU's instance.
- shutdown/EOF/SIGTERM each restore firmware auto; verified by observing fan policy after exit.
- A failed control tick (set-fan error or temp-read failure) reverts all fans to firmware default.
- `nvidia restore` restores every GPU to firmware default and is idempotent.
