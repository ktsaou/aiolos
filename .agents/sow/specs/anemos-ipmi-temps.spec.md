# Spec: `ipmi-temps` anemos

Status: design (SOW-0006). ASRockRack ROME2D16-2T BMC analog temperature **sensors** — read-only;
controls NO device. Conforms to `aiolos-protocol.spec.md`. Its readings are routed
(`input=ipmi-temps`) into a fan controller (`asrock16-2t`) so a hot board/DIMM/NIC raises the board
fans.

## Purpose
Report this board's BMC analog temperature sensors (CPU package, motherboard, card-side, LAN/NIC,
and the DDR4 DIMM sensors) for routing. One `run` instance (the board) reads all sensors over a
single inband IPMI handle. This is a **sensor-only** anemos: it has no curve, sets nothing, and its
fail-safe is a no-op (there is nothing to hand back to firmware). It complements `nvme` (NVMe temps)
and the asrock module's own `k10temp` CPU reads — the asrock driving max already folds in **all**
routed `type:temp` sources source-agnostically.

## Hardware facts
- BMC: ASPEED AST2500 (MegaRAC) on the ROME2D16-2T; inband interface `/dev/ipmi0`. Same BMC the
  asrock fan module drives.
- Analog temperature sensors are read with **standard** IPMI (netfn `0x04`): `Get Sensor Reading`
  (`0x2d`) for the raw byte + availability, and `Get Sensor Reading Factors` (`0x23`) for the linear
  conversion — the identical mechanism the asrock module uses for fan-tach RPM (SOW-0005), here
  applied to temperatures.
- Sensor numbers (board-specific, hardcoded — like asrock's `FAN_SENSORS`):
  `TEMP_CPU1=0x28`, `TEMP_CPU2=0x29`, `TEMP_MB1=0x2A`, `TEMP_MB2=0x2B`, `TEMP_CARD_SIDE1=0x2C`,
  `TEMP_LAN=0x2E`, `TEMP_DDR4_A..P=0x48..0x57`. (`0x2D` is intentionally not in the set.)
  Reported labels: `CPU1`, `CPU2`, `MB1`, `MB2`, `CARD_SIDE`, `LAN`, `DDR4_A`..`DDR4_P`.
- Unpopulated DIMM slots and absent sensors report "ns"/unavailable; they are skipped (per-sensor
  `available` flag from the `Get Sensor Reading` status byte).
- `TEMP_LAN` is the Intel X550 NIC, which has documented thermal/PHY link-drop issues on this host;
  routing it into the board fans lets the chassis fans respond to NIC heat.

## detect
- Emit exactly one board ID: `{"id":"ipmi-temps","type":"board","name":"ROME2D16-2T BMC temps"}`.
  Stable across re-detect. One instance reads all sensors because the IPMI handle is opened ONCE per
  process (protocol rule) and these are register-style BMC reads — not blocking admin commands like
  NVMe — so per-sensor process isolation is unnecessary.

## run <board>
- Read every hardcoded sensor and report one `temp` reading per sensor whose reading is
  **available**; unavailable/"ns" sensors are skipped (never reported as 0/garbage):
  ```json
  {"status":"ok","readings":[
    {"type":"temp","label":"MB1","temp":26},
    {"type":"temp","label":"LAN","temp":47},
    {"type":"temp","label":"DDR4_A","temp":45}]}
  ```
- `inputs` are ignored (a pure sensor). If **no** sensor is readable this tick, respond
  `{"status":"error","error":"…"}` (transient; the orchestrator reconciles next tick) — never a
  crash, never silent.
- Conversion: linear `((m·raw)+(b·10^b_exp))·10^r_exp` from `Get Sensor Reading Factors` (IPMI v2.0
  §35.5), same path as the RPM tachs. Unlike a fan tach, a temperature may legitimately be negative,
  so **no** non-negativity clamp is applied.
- Per-sensor factors are **prefetched at instance open** and cached (constant for these linear
  sensors); a sensor whose prefetch failed is retried lazily on later ticks and reports no
  temperature until its factors are known. At most **one** IPMI call per sensor per tick.
- All reads use a **short timeout** (100 ms) so a slow/unresponsive BMC degrades a sensor to "absent"
  rather than blowing the apply deadline (same policy as the asrock observability reads).

## Why a separate process (isolation)
Inband IPMI sensor reads go to the BMC over `/dev/ipmi0`; a wedged/laggy BMC could stall them. Doing
them in their own process means a slow BMC only stalls this `ipmi-temps` instance (killed + respawned
at the tick deadline); the fan controller keeps cooling on its other inputs (GPU + NVMe + CPU) and
simply drops the stale BMC-temp input for that tick. The short per-read timeout bounds each call so a
single laggy sensor degrades to "absent" rather than dominating the tick. A separate module (vs
folding the temps into the asrock module that already holds an IPMI handle) preserves the
project's isolation + reuse guarantees; the trade-off is a second `/dev/ipmi0` handle, which the
Linux IPMI char interface multiplexes safely per-fd by msgid.

## Implementation note (binding)
Rust via the level-1 `ipmi` tech crate (`read_sensor` / `read_sensor_factors`, used as-is; no new
helper added to the shared crate). The board-specific sensor table + the cache-and-convert loop live
in the module (`anemoi/ipmi-temps/src/sensors.rs`), mirroring `anemoi/asrock16-2t/src/board.rs`. A
pure `temp_celsius(factors, reading)` helper makes the conversion unit-testable without hardware.
Sensor-only is expressed by `ModuleInfo.curve_* = None` in the SDK: `run_loop` then skips the
curve-empty warning and `Device::apply` ignores the controller.

## Modes
`detect` · `run <board>` · `restore` (one-shot: **no-op** — a sensor controls nothing — exits 0;
idempotent; still implemented so `aiolos restore` can call it uniformly).

## Fail-safe
None required: the module controls no device, so `shutdown`/EOF/`SIGTERM`/`restore` simply exit.
There is no manual state to revert and no thermal risk from the module stopping.

## Config
None. A sensor-only module has no curve file. The sensor numbers are hardcoded (board-specific);
no secrets or BMC credentials are involved (inband `/dev/ipmi0` needs no IP/credentials).

## Acceptance criteria
- `detect` lists exactly one board entry; stable across re-detect.
- `run` reports one `temp` reading per **available** BMC sensor within timeout, with the labels
  above; unavailable/"ns" sensors are skipped (never 0/garbage); a tick where nothing is readable
  → `status:error` (never a crash, never silent).
- Readings match `ipmitool sdr type Temperature` (on-hardware validation).
- Routed `input=ipmi-temps` into `asrock16-2t`: a warm DIMM/NIC raises the board fans (the asrock
  driving max already folds in all routed temps source-agnostically — no asrock code change).
- A slow/wedged BMC only stalls this instance (killed + respawned), never the fan controller or
  sibling instances (isolation).
- `ipmi-temps restore` exits 0 and is idempotent (no-op).
- No secrets in committed artifacts (sensor numbers are board constants, not sensitive).
