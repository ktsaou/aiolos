# SOW-0006 - `ipmi-temps` sensor anemos (board/DIMM/NIC/card-side temps → board fans)

## Status

Status: open

Sub-state: idea captured from the 2026-05-30 server inspection. Not started. Full Pre-Implementation
Gate to be written at activation.

## Requirements

### Purpose
Feed the BMC's analog temperature sensors (motherboard, DIMM, card-side, LAN/NIC) into the
`asrock16-2t` board-fan driving max, so the chassis fans respond to board/memory/NIC heat — not just
GPU + NVMe + CPU.

### User Request
> "come up with ideas on what else we could use to improve aiolos" → idea #1 (highest value / lowest
> effort): use the BMC temps aiolos currently ignores.

### Assistant Understanding
Facts (read-only inspection, 2026-05-30, host `nova`):
- BMC exposes these analog temps via IPMI (`Get Sensor Reading`): `TEMP_MB1/2` (26/35 °C),
  `TEMP_CARD_SIDE1` (28 °C, next to the GPUs), `TEMP_LAN` (**47 °C**, the X550 NIC), `TEMP_DDR4_*`
  (populated DIMMs read up to **47 °C**; unpopulated read "ns").
- aiolos uses only GPU (NVML), CPU (k10temp), NVMe (sysfs) today.
- The IPMI sensor-read + factor-decode code already exists (`tech/ipmi`, SOW-0005) and the
  sensor-only anemos pattern + multi-input routing exist (`anemoi/nvme`, SOW-0004).
- `anemos-asrock16-2t.spec.md` already lists these board temps as a "planned enhancement."
- The X550 NIC has documented thermal/PHY link-drop issues (`~/ethernet-issues/ETHERNET-ISSUES.md`);
  spinning chassis fans on `TEMP_LAN` could directly mitigate them.

### Acceptance Criteria
- A new sensor-only `ipmi-temps` anemos reports the readable BMC temps (per-sensor labels); routed
  `input=ipmi-temps` into `asrock16-2t`; the board driving max includes them.
- "ns"/unavailable sensors are skipped (never reported as 0/garbage).
- Verified on hardware: readings match `ipmitool sdr type Temperature`; a warm DIMM/NIC raises the
  board fans.
- Isolation preserved (sensor-only, own process); no secrets in artifacts.

## Analysis
Sources: live IPMI SDR (this host), `tech/ipmi` (`read_sensor`/`read_sensor_factors`),
`anemoi/nvme` (sensor-only pattern), `aiolos` multi-input routing, `anemos-asrock16-2t.spec.md`.

Reuse: nearly everything is built — the `Get Sensor Reading` path (SOW-0005), the sensor-only
module shape (SOW-0004), `module:id` routing. The new work is mostly enumerating/identifying the
temp sensors and reporting them.

## Pre-Implementation Gate
Status: blocked (open SOW; gate written at activation)

Open decisions (resolve at activation):
- **Sensor discovery:** walk the SDR repository to find temp sensors + their numbers/labels
  dynamically, OR hardcode this board's known sensor numbers (like `board.rs` does for fans). The
  former is portable; the latter is simpler and board-specific (the module is board-specific anyway).
- **Which sensors:** all readable temps, or a curated set (MB, card-side, LAN, DIMM)? `TEMP_LAN`
  (NIC) and `TEMP_CARD_SIDE1` (near GPUs) are the most valuable.
- **Module identity:** one `ipmi-temps` module exposing all, vs folding into `asrock16-2t` (which
  already holds the IPMI handle). A separate module preserves isolation + reuse but opens a second
  `/dev/ipmi0` handle.
- **Labels/units:** stable labels; conversion via `Get Sensor Reading Factors` (same as RPM).

## Plan
1. `tech/ipmi`: SDR-repository scan (or a sensor enumerator) to list temp sensors (if dynamic).
2. `anemoi/ipmi-temps`: sensor-only module reporting the temps.
3. Registry: `input=...,ipmi-temps` into asrock; asrock already maxes all routed temps.
4. Spec/docs; on-hardware validation.

## Execution Log
### 2026-05-30
- Created (open) from the server-inspection idea list. No code.

### 2026-05-30 — implementation
Implemented the sensor-only `ipmi-temps` anemos. Open design decisions (from the Pre-Implementation
Gate) were resolved with conventional choices, matching the existing codebase patterns, and recorded
here:

- **Sensor discovery → HARDCODE (board-specific).** Mirrors `anemoi/asrock16-2t/src/board.rs`
  `FAN_SENSORS: [u8; 8] = [0x60..0x67]`. The module is board-specific by name (`ipmi-temps`, wired
  only on this ROME2D16-2T host's registry), so walking the SDR repository every detect cycle would
  add IPMI round-trips and parsing for no portability gain here. Sensor numbers + labels hardcoded
  per the activation brief: `TEMP_CPU1=0x28`, `TEMP_CPU2=0x29`, `TEMP_MB1=0x2A`, `TEMP_MB2=0x2B`,
  `TEMP_CARD_SIDE1=0x2C`, `TEMP_LAN=0x2E`, `TEMP_DDR4_A..P=0x48..0x57`. (0x2D is intentionally NOT in
  the set — the brief lists `CARD_SIDE1=0x2C` then `LAN=0x2E`, skipping 0x2D.)
- **Which sensors → read ALL hardcoded, report the AVAILABLE ones.** Cannot probe live (production
  safety: the module must not be run here), so the module reads every hardcoded sensor each tick and
  reports only those whose `Get Sensor Reading` status byte is `available` (the `read_sensor`
  `available` flag — same skip-logic asrock uses for `FAN*_2`/`FAN_PSU*`). Unpopulated DIMM slots
  read "ns"/unavailable and are silently skipped (never reported as 0/garbage). Acceptance criterion:
  "ns"/unavailable sensors are skipped.
- **Module identity → ONE separate `ipmi-temps` module, ONE detect instance.** Separate module
  (not folded into asrock) preserves isolation + reuse. One `run` instance reads all sensors in one
  process: the IPMI handle is opened ONCE per process (protocol rule: init device libs once per
  process), and these are register-style BMC sensor reads — NOT blocking admin commands like NVMe —
  so per-sensor process isolation is unnecessary. Mirrors asrock, which opens one IPMI handle and
  reads all 8 fans in one instance. `detect` emits one entry `{id:"ipmi-temps", type:"board",
  name:"ROME2D16-2T BMC temps"}`.
  - Note: this opens a SECOND `/dev/ipmi0` handle alongside asrock's (the brief's accepted
    trade-off for isolation/reuse). The Linux IPMI char interface multiplexes per-fd by msgid, so
    two independent fds coexist safely.
- **Labels/units → stable labels, °C via `Get Sensor Reading Factors`** (same linear path as the
  RPM tachs). Labels: `CPU1`,`CPU2`,`MB1`,`MB2`,`CARD_SIDE`,`LAN`,`DDR4_A`..`DDR4_P`.
- **No `tech/ipmi` helper added.** The per-sensor cache+convert loop lives in the module
  (`anemoi/ipmi-temps/src/sensors.rs`), mirroring `board.rs::read_fan_rpms`, keeping the shared
  `tech/ipmi` crate untouched (lowest blast radius; existing tests unchanged). `read_sensor` /
  `read_sensor_factors` are used as-is. A pure `temp_celsius(factors, reading)` helper (analogous to
  `board.rs::fan_rpm`) makes the conversion unit-testable without hardware.
- **Short observability timeout (100 ms)** per sensor read/factor, identical rationale to asrock's
  `OBS_TIMEOUT`: a slow/unresponsive BMC degrades a reading to "absent" rather than blowing the apply
  deadline. Factors are fetched lazily and cached (constant for linear sensors); a sensor whose
  factor fetch failed reports no temp that tick and is retried next tick. Bounded to ≤1 IPMI call per
  sensor per tick (22 sensors → ≤22 calls × 100 ms ≈ 2.2 s worst case; a healthy BMC is single-digit
  ms, ~10-20× headroom under the 2 s default apply deadline; the worst-case note is recorded as a
  follow-up to consider raising `timeout` or splitting if a future BMC is pathologically slow).
- **Sensor-only**: `ModuleInfo.curve_* = None`; `apply` ignores the `Controller`; `restore` /
  `restore_all` are no-ops; the `restore` one-shot exits 0. Identical to `nvme`.
- **Registry**: added `ipmi-temps` and appended `input=ipmi-temps` to the asrock line; asrock already
  maxes ALL routed `type:temp` inputs source-agnostically (`input_temps`), so no asrock code change.

Files: `anemoi/ipmi-temps/{Cargo.toml,src/main.rs,src/sensors.rs}` (new),
`Cargo.toml` (workspace member), `packaging/aiolos.conf` (registry),
`.agents/sow/specs/anemos-ipmi-temps.spec.md` (new).

## Validation

Static gates (all clean; the production server forbids running the binary here — on-hardware checks
are deferred to the user's combined test):
- `cargo build --release` — clean (full workspace, incl. `anemoi-ipmi-temps`).
- `cargo clippy --all-targets` — clean (no warnings).
- `cargo fmt --all -- --check` — clean (exit 0).
- `cargo test --workspace --no-run` — compiles; the new `ipmi_temps` unit-test binary builds. **Not
  run** (production server; the user runs the suite as a whole). Unit tests added:
  `sensor_table_is_the_verified_set` (22 sensors, exact numbers, 0x2D absent, unique labels),
  `temp_celsius_converts_only_a_valid_reading` (available/unavailable/missing factors→skip),
  `temp_celsius_allows_negative_and_offset_factors` (no fan-style non-negativity clamp for temps).

Protocol conformance (by construction; reviewed against `project-anemos-protocol`):
- stdout = protocol only; all logging via the SDK to stderr (module emits no `println!`).
- `detect` → one stable board id; `apply` → one `readings` line within timeout, `status:error` when
  nothing readable (never crash/empty/silent); `restore`/`restore_all`/EOF/SIGTERM are no-ops
  (sensor-only) — inherited from the `anemos` SDK exactly like `nvme`.
- IPMI handle opened ONCE per process (in `open`), not per detect cycle (no fd leak).
- Each sensor read bounded by a 100 ms timeout; ≤ 1 IPMI call per sensor per tick.

Same-failure search: the only other sensor-only anemos is `nvme`; this mirrors its shape (curve
None, no-op restore, `Applied::error` on empty) and the asrock `read_fan_rpms` factor-cache pattern.
No existing tests changed; `tech/ipmi` untouched (no shared-helper added).

Acceptance criteria mapping:
- "sensor-only `ipmi-temps`, per-sensor labels, routed `input=ipmi-temps`, board max includes them"
  → module + registry + spec done; asrock `input_temps` already folds in all routed temps
  source-agnostically (no asrock code change). ✔ (build-verified)
- "'ns'/unavailable sensors skipped (never 0/garbage)" → `read_sensor` `available` flag honoured;
  unit-tested via `temp_celsius`. ✔
- "verified on hardware: readings match `ipmitool sdr type Temperature`; a warm DIMM/NIC raises the
  board fans" → **deferred to the user's on-hardware test** (cannot run on this production server). ⏳
- "isolation preserved (sensor-only, own process); no secrets in artifacts" → separate module/own
  process; sensor numbers are board constants, inband IPMI needs no IP/credentials → no secrets. ✔

Sensitive-data gate: none written (sensor numbers/labels are board constants; no BMC IP/credentials,
serials, or host identifiers in any artifact).

Artifact-maintenance gate: spec `anemos-ipmi-temps.spec.md` added; registry `aiolos.conf` updated;
SOW execution log + validation updated. Skill update: not needed — `project-create-anemos` already
documents the sensor-only pattern (curve None, no-op restore) this module follows verbatim; no new
convention introduced.

## Outcome
Implemented and statically validated; on-hardware validation pending the user's combined test run.
SOW left in `pending/` per the activation brief (not moved to `done/`).

## Lessons Extracted
Pending.

## Followup
- **Worst-case tick budget:** 22 sensors × 100 ms OBS_TIMEOUT = up to ~2.2 s if the BMC is
  pathologically slow on every sensor, which exceeds the default 2 s apply `timeout`. A healthy BMC
  reads in single-digit ms (~10-20× headroom), and the orchestrator just drops a timed-out tick's
  input (isolation), so this is not a correctness risk — but if a future BMC is consistently laggy,
  consider raising the registry `timeout`, lowering OBS_TIMEOUT, or batching/striping the reads
  across ticks. (asrock's batch is ≤ 9 calls and stays well under; this larger DIMM table is the
  reason to track it.)
- On-hardware validation (readings vs `ipmitool sdr type Temperature`; warm DIMM/NIC raising the
  board fans) is deferred to the user's combined cut-over test.

## Regression Log
None yet.
