# SOW-0010 - Per-zone board-fan curves (CPU coolers vs case fans)

## Status

Status: open

Sub-state: idea captured 2026-05-30. Not started. Gate at activation.

## Requirements

### Purpose
Drive the board's fan zones independently instead of one uniform duty: FAN1/2 (the Noctua CPU
coolers) by CPU temp, FAN3–8 (120 mm case fans) by `max(GPU, NVMe, board)`. Quieter/more efficient —
don't blast case fans for a CPU-only spike, or vice versa.

### User Request
> Idea #5: now feasible since asrock already addresses fans individually and reports per-fan RPM.

### Assistant Understanding
Facts:
- FAN1/2 are the large Noctua CPU coolers (low RPM by size — ~800 at 50%); FAN3–8 are 120 mm case
  fans (~1600). asrock sets all 8 to one uniform `curve(driving_temp)` today.
- The board OEM `0xd6` command already takes per-fan duty bytes (bytes 0–7 = FAN1..FAN8); per-fan
  RPM is read back. So per-zone duties are mechanically supported already.
- `anemos-asrock16-2t.spec.md` notes per-fan/per-zone curves as a supported future option.

### Acceptance Criteria
- Configurable zones, each with its own curve + driving inputs; asrock sets per-zone duties.
- Default remains uniform (or a sensible CPU/case split) — back-compatible with the single-curve
  config.
- Verified: a CPU-only load raises FAN1/2 without over-driving FAN3–8, and GPU/NVMe heat raises
  FAN3–8; the all-manual non-zero `0xd6` rule is still honoured.

## Analysis
Sources: `anemoi/asrock16-2t` (`board.rs` `set_all_fans`/`duty_payload`, `main.rs` apply),
`anemos` curve/controller (per-zone would need multiple controllers), the curve config format.

## Pre-Implementation Gate
Status: blocked (open SOW; gate at activation)

Open decisions:
- **Config schema:** how to express zones (fan index ranges → {curve, inputs}) while staying
  back-compatible with the current single `*.curve.json`.
- **Inputs per zone:** FAN1/2 by CPU temps only; FAN3–8 by `max(GPU, NVMe, board)`? Confirm the
  mapping. Each zone needs its own EMA/deadband Controller.
- **`0xd6` constraint:** all 16 bytes must be manual + non-zero; per-zone duties must still satisfy
  this (bytes 0–7 vary, 8–15 stay non-zero).
- **Per-fan RPM labels** already exist for verification.

## Plan
1. Curve-config schema for zones (back-compatible default = uniform).
2. asrock apply: per-zone driving + duty; one Controller per zone.
3. Spec/docs; on-hardware validation.

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
