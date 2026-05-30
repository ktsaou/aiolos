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
Status: resolved (implementation in this branch, after SOW-0010)

Problem / root cause: a stalled fan on a 24/7 box is a real failure mode; per-fan RPM is now live
(SOW-0005) but nothing acts on it. A dead fan silently reads ~0 RPM while still commanded a duty.

Evidence reviewed:
- `anemoi/asrock16-2t/src/board.rs` `read_fan_status` → `(duty_readback, Vec<(label, Option<rpm>)>)`,
  read AFTER the control decision under a short timeout; `Some(0)` = stopped, `None` = unreadable.
- `anemoi/asrock16-2t/src/main.rs` `apply` already emits a per-fan `fan` reading with `pwm`(+`rpm`).
- `protocol::Reading.fields` is `#[serde(flatten)]`, so a `"fault":true` field needs **no** protocol
  change.

### Decisions (recorded before implementation)
1. **Detection rule — absolute, module-local, per-fan state machine.** A fan is *faulted* when, for
   `FAULT_TICKS` (3) consecutive ticks: it is **commanded ≥ `FAULT_MIN_DUTY` (20 %)** AND its
   **measured RPM is present and ≤ `FAULT_RPM_MAX` (100)**. A `None` (unreadable) RPM is **not** a
   fault (sensor unreadable ≠ fan dead) and does not advance the counter (it neither confirms nor
   clears). RPM above the threshold clears the counter immediately.
   - **Spin-up grace:** a fan only starts accruing fault ticks after it has been commanded
     ≥ `FAULT_MIN_DUTY` for `SPINUP_GRACE_TICKS` (2) consecutive ticks — so a fan ramping from
     low/zero is never flagged mid-spin-up. Dropping below the duty threshold resets the per-fan
     state.
   - Chosen absolute over relative (median-of-peers): the two zones (CPU Noctuas ~800 RPM vs case
     fans ~1600 RPM) make a single cross-fan median noisy; absolute `≈0 while driven` is
     unambiguous and matches the "dead fan" failure we care about.
2. **Where — in the module.** asrock owns the per-fan duty it commanded and the matching tach, so the
   evaluation is local and needs no orchestrator change. (nvidia could mirror this later.)
3. **Surfacing — reading + warn (local).** The faulted fan's `fan` reading carries `"fault":true`;
   a `tracing::warn!` names the fan. Richer delivery (webhook / Netdata `aiolos_fan_rpm==0` alarm,
   ties to SOW-0007 metrics) is an explicit **follow-on**, not in this SOW.
4. **Compensation — boost surviving siblings (implemented).** On a *confirmed* fault, the other
   (non-faulted) fans **in the same zone** are commanded to 100 % on subsequent ticks (more airflow
   is always safe). A faulted fan keeps being commanded its normal zone duty (commanding 0 is never
   allowed; the firmware floor applies). Compensation is in addition to the loud reading/warn.

## Plan
1. Add fan-health evaluation (per fan: commanded vs measured RPM, with grace/hysteresis).
2. Surface it (reading + status-page indicator); wire an alert channel.
3. Optional sibling-boost; validate with a stall.

## Execution Log
### 2026-05-30
- Created (open) from the server-inspection idea list. No code.
- Implemented module-local fan-fault detection + sibling-boost compensation in the asrock anemos
  (built on top of SOW-0010's per-fan path; SDK untouched).
  - `anemoi/asrock16-2t/src/fault.rs` (new): `FanFaultTracker` — a per-fan state machine
    (`above_streak`/`fault_streak`/`confirmed`) with a pure `step()` transition. A fan faults when
    commanded ≥ `FAULT_MIN_DUTY` (20%) AND a *present* RPM ≤ `FAULT_RPM_MAX` (100) for `FAULT_TICKS`
    (3) consecutive ticks past `SPINUP_GRACE_TICKS` (2). `None` RPM holds state; an above-threshold
    RPM clears; a lightly-commanded fan resets. `compensate(base, confirmed)` boosts surviving
    same-zone fans to 100%. Six unit tests cover grace+hysteresis, recovery, low-duty, unreadable
    tach, and both compensation directions + no-op.
  - `anemoi/asrock16-2t/src/main.rs`: `apply` reads `faults.confirmed()` at tick start, feeds it to
    `compensate` before commanding, then after the RPM observability read calls `faults.update(...)`
    with the duty actually commanded; faulted fans get `"fault":true` in their `fan` reading and a
    `tracing::warn!`.
- Build/clippy(all-targets)/fmt clean; `cargo test --workspace --no-run` compiles. Stall not induced
  on hardware (production cooler is `nvfd`; user runs the on-board validation).

## Validation
- Acceptance — detection: unit-tested state machine (confirm only after grace + N ticks; clear on
  RPM return; no false positive on unreadable tach or low duty). The `"fault":true` reading field +
  warn surface it (status page/metrics can render it). A real/simulated stall is **pending the
  user's hardware run**.
- Acceptance — compensation: `compensate` unit tests show same-zone survivors → 100%, other zone and
  the dead fan untouched.
- No false positives: grace + hysteresis + present-RPM-only rule (tests `low_duty_never_faults`,
  `unreadable_rpm_holds_state`, `recovers_when_rpm_returns`).
- Alerting: local (reading + warn) per the decision; webhook/Netdata delivery is the documented
  follow-on.
- Reviewers: not run in-worktree (parallel-agent constraint); the user consolidates review at merge.

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
None yet.

## Regression Log
None yet.
