# Spec: `nvme` anemos

Status: design (SOW-0004). NVMe SSD temperature **sensor** — read-only; controls NO device.
Conforms to `aiolos-protocol.spec.md`. Its readings are routed (`input=nvme`) into a fan
controller (e.g. `asrock16-2t`) so hot disks raise the board fans.

## Purpose
Report each NVMe drive's temperatures for routing. One `run` instance per physical drive, bound by
the controller **serial** (stable across reboots and across `nvmeN`/`hwmonM` renumbering). This is
a **sensor-only** anemos: it has no curve, sets nothing, and its fail-safe is a no-op (there is
nothing to hand back to firmware).

## detect
- Enumerate `/sys/class/nvme/nvme*` (controllers exposing a non-empty `serial`); emit one `found`
  per drive: `{"id":"<serial>","type":"NVMe","name":"<model>"}`. Serial-sorted for stable ordering.
- Empty `found` is a real result (no NVMe drives). Non-controller entries (no `serial`) are skipped.

## run <serial>
- Re-resolve the controller by serial each tick (so a drive that dropped and returned as a
  different `nvmeN` is still tracked by its stable serial), then read every temperature from that
  controller's own `hwmonM/` node (`tempK_input` milli-°C → °C, labelled by `tempK_label`).
- Report one `temp` reading per sensor:
  ```json
  {"status":"ok","readings":[
    {"type":"temp","label":"Composite","temp":33},
    {"type":"temp","label":"Sensor 1","temp":33},
    {"type":"temp","label":"Sensor 2","temp":43}]}
  ```
- `inputs` are ignored (a pure sensor). If the drive is no longer present, or no temperature is
  readable, respond `{"status":"error","error":"…"}` (transient; the orchestrator reconciles via
  the next `detect`).

## Why a separate process (isolation)
NVMe `hwmon` temperature reads go through the NVMe driver (a SMART-log **admin command**), so they
can block while a controller is resetting/timing out — unlike CPU `k10temp` register reads. Reading
them in their own process means a wedged disk only stalls its own `nvme` instance (killed +
respawned at the tick deadline); the fan controller keeps cooling on its other inputs (GPU + CPU)
and simply drops the stale NVMe input for that tick. This is the project's process-isolation
guarantee applied to the one local sensor that can genuinely block.

## Implementation note (binding)
Rust via the level-1 `nvme` tech crate (sysfs only; no external deps). Enumeration reads
`serial`/`model` (cached controller identify attrs — instant, no admin command); only the
`tempK_input` reads issue the SMART-log admin command. Sysfs root overridable via
`AIOLOS_SYSFS_NVME` for off-hardware testing. Sensor-only is expressed by `ModuleInfo.curve_* =
None` in the SDK: `run_loop` then skips the curve-empty warning and `Device::apply` ignores the
controller.

## Modes
`detect` · `run <serial>` · `restore` (one-shot: **no-op** — a sensor controls nothing — exits 0;
idempotent; still implemented so `aiolos restore` can call it uniformly).

## Fail-safe
None required: the module controls no device, so `shutdown`/EOF/`SIGTERM`/`restore` simply exit.
There is no manual state to revert and no thermal risk from the module stopping.

## Config
None. A sensor-only module has no curve file.

## Acceptance criteria
- `detect` lists one entry per NVMe drive, id = serial, name = model; stable across re-detect.
- `run <serial>` reports that drive's per-sensor temps within timeout; absent drive / unreadable
  temps → `status:error` (never a crash, never silent).
- A wedged drive's instance is killed + respawned without affecting sibling instances or the fan
  controller (isolation).
- `nvme restore` exits 0 and is idempotent (no-op).
- No secrets in committed artifacts (serials are runtime ids only).
