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

## Validation
Pending.

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
None yet.

## Regression Log
None yet.
