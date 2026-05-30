# Spec: `gpu-powercap` anemos

Status: design (SOW-0009). GPU power-limit reactor — a **curve-less CONTROL** module. Conforms to
`aiolos-protocol.spec.md`. It is the first aiolos module that controls something other than fans,
and the worked example of a module driven by **routed signal** (not a temperature curve).

## Purpose
React to a utility-power event by capping each GPU's NVML power-management limit to extend UPS
runtime, then lift the cap when AC returns. One `run` instance per physical GPU, bound by GPU
**UUID** (stable across NVML index renumbering). It consumes `power-state` readings routed from the
`nut` sensor (`input=nut`).

## Curve-less control (why `curve = None`)
`ModuleInfo.curve_* = None`: there is **no temperature curve** — the decision is driven by routed
power state, so `apply` ignores the SDK `Controller`. But unlike a sensor-only module, this one DOES
control a device (set/restore the power limit) and has a real fail-safe. The SDK's
`None`-means-sensor handling simply skips the curve checks; the module supplies its own control and
restore.

## detect
- Enumerate GPUs via NVML; emit one `found` per GPU:
  `{"id":"<GPU-UUID>","type":"GPU","name":"<product name>"}` (same ids as `nvidia`).
- **Fork-safety:** NVML is not fork-safe; each process calls `nvmlInit` itself after being spawned.

## open <UUID>
- `nvmlInit`; resolve the device by UUID; **record the firmware default power limit NOW** (the
  restore target) plus the device-accepted `[min,max]`. A GPU that cannot report a power limit is not
  power-cappable → `open` fails (the SDK declares `fatal`; supervisor retries on a long backoff). We
  never half-manage.
- The GPU handle opts out of the tech crate's fan-restore-on-drop (this module never touches fans).

## run <UUID>
- Each `apply`: reduce the routed `power-state` readings to one aggregate signal (on-battery if ANY
  UPS is; low-battery if ANY raised `LB`; the SMALLEST runtime among **on-battery** UPSes — a mains
  UPS's runtime is ignored so it can't mask a draining one), then decide and act:
  - **Cap** when on-battery AND a trigger fires (see policy); set the limit to `cap_pct`% of the
    recorded default (clamped to the device min).
  - **Lift** otherwise (AC present, or on-battery but healthy) → restore the firmware default.
  - NVML is issued only when the effective limit CHANGES (transition, or a live `cap_pct` edit), not
    every tick.
- Report:
  ```json
  {"status":"ok","readings":[
    {"type":"powercap","label":"GPU","capped":true,"limit_mw":420000,"default_mw":600000,
     "min_mw":100000,"draw_mw":410000,"reason":"low-runtime","on_battery":true,"runtime_s":250}]}
  ```
- `reason` ∈ {`none`,`on-battery`,`low-runtime`,`low-battery-flag`}. A failed control tick restores
  the firmware default and responds `{"status":"error","error":"…"}`.

## Policy (conservative by default) — `$AIOLOS_ETC_DIR/gpu-powercap.conf` else `/opt/aiolos/etc/gpu-powercap.conf`
`key=value`, `#` comments; unknown keys / unparseable values are ignored (a typo never disarms the
fail-safe). Reloaded every tick (live tuning). Keys (defaults):
- `cap_pct=70` — cap target as a % of each GPU's firmware default limit (1..100; device min enforced).
- `cap_on_battery=false` — cap as soon as ANY UPS is on battery (off by default).
- `runtime_floor_s=300` — cap when an on-battery UPS's estimated runtime ≤ this (0 disables).

**Trigger (only ever caps while ON BATTERY):** `low_battery` flag → cap; else on-battery with
`runtime ≤ runtime_floor_s` → cap; else cap iff `cap_on_battery`. On AC the module ALWAYS lifts, so a
misread runtime on mains can never throttle a job. The default is therefore *monitor + log* while on
a healthy battery, capping only once the battery is genuinely draining — extending runtime when it
matters without disrupting a running job on a brief outage. No secrets in this file (thresholds only).

## Modes
`detect` · `run <UUID>` · `restore` (one-shot: restore EVERY GPU's power limit to firmware default
and exit; idempotent; called by `aiolos restore`).

## Fail-safe
On `shutdown`, stdin EOF, OR `SIGTERM`/`SIGINT`: restore the GPU's power limit to the recorded
firmware default and exit — the module catches the signal itself (self-sufficient, via the SDK).
Additionally: a failed control tick restores the default, and the GPU handle's `Drop` restores the
default if a cap is still owed (panic backstop). **Critical:** NVML power limits PERSIST after the
process exits — the driver does NOT auto-revert — so restore is mandatory. A direct `SIGKILL` cannot
restore in-process; `aiolos restore` (systemd ExecStopPost) is the net, and `restore_all` (the
one-shot) restores every GPU to its firmware default independent of the recorded handle. The capped
state is always MORE restrictive than the default, so "module dies → firmware default reclaims" is
the safe direction (more power, never less).

## Acceptance criteria
- Detects all GPUs by UUID; per-GPU `run` instances are independent processes.
- On a (real or simulated) on-battery event meeting the trigger, each GPU's power limit is capped to
  `cap_pct`% of default; on AC restore the firmware default is restored.
- On AC the module never caps, regardless of config (verified by the decision unit tests).
- shutdown/EOF/SIGTERM and `gpu-powercap restore` each restore the firmware default; verified by
  reading the power limit after exit. A failed control tick reverts to default.
- No secrets in committed artifacts (config holds thresholds only; UPS host lives in `nut.conf`).
