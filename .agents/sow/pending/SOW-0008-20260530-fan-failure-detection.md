# SOW-0008 - Fan-failure detection + alerting (use the per-fan RPM data)

## Status

Status: open

Sub-state: idea captured 2026-05-30. Not started. Gate at activation.

## Requirements

### Purpose
Now that `asrock16-2t` reports per-fan tachometer RPM (SOW-0005), detect a stalled/failed fan
(`rpm ≈ 0` while it is being commanded above the floor, or RPM far below its peers) and surface it
loudly — optionally compensating by boosting the remaining fans.

### User Request
> Idea #3: a dead fan on a 24/7 box is a real failure mode; RPM detection is now basically free.

### Assistant Understanding
Facts:
- Per-fan RPM is live (e.g. FAN1/2 ~800 at 50% duty, FAN3–8 ~1600). A failed fan reads ~0 RPM while
  still commanded a non-zero duty.
- aiolos already declares faults via `status:error` and surfaces per-instance health + stderr tail.
- nvidia also reports per-fan RPM (NVML) — the same detection could cover GPU fans.

### Acceptance Criteria
- A fan commanded above the floor but reading ~0 RPM (or anomalously low vs peers) is flagged
  (status-page indicator + a declared warning/`error` with the fan label).
- An alert is delivered through a chosen channel; optional sibling-boost compensation.
- No false positives at the floor / during spin-up; verified against a real or simulated stall.

## Analysis
Sources: `anemoi/asrock16-2t` (RPM readings), `anemoi/nvidia` (RPM), `aiolos` status/health,
`anemos` status taxonomy. The detection input already exists; the new parts are the policy + alert.

## Pre-Implementation Gate
Status: blocked (open SOW; gate at activation)

Open decisions:
- **Detection rule:** absolute (`rpm==0 && duty>floor` for N consecutive ticks) vs relative
  (rpm << median of same-type fans). Spin-up grace + hysteresis to avoid flapping.
- **Where:** in the module (asrock/nvidia report a "fan_fault" reading) vs in the orchestrator
  (cross-fan comparison on the blackboard). Module-local is simplest.
- **Alerting channel:** log only / status-page badge / webhook / email / Netdata alarm (ties to
  SOW-0007 metrics — a `aiolos_fan_rpm==0` alert may be the cleanest path).
- **Compensation:** boost siblings on a confirmed failure? (more airflow, safe) or detect-only.

## Plan
1. Add fan-health evaluation (per fan: commanded vs measured RPM, with grace/hysteresis).
2. Surface it (reading + status-page indicator); wire an alert channel.
3. Optional sibling-boost; validate with a stall.

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
