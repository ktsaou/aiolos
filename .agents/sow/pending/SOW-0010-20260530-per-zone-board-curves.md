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
Status: resolved (implementation in this branch)

Problem / root cause: one uniform `curve(max(GPU,NVMe,CPU))` over-drives the 120 mm case fans for a
CPU-only spike (and the Noctua CPU coolers for a GPU-only spike). Per-fan duty + per-fan RPM are
already supported by the board (`0xd6` per-fan bytes; `0x60..0x67` tachs), so zoning is now feasible
without new hardware access.

Evidence reviewed:
- `anemoi/asrock16-2t/src/board.rs` — `duty_payload(pct)` builds a uniform `[byte;16]`; `regulate`
  claims→sets→retries→releases; `read_fan_status` returns per-fan duty + RPM. `FAN_SENSORS`
  `0x60..0x67` = FAN1..FAN8 (FAN1/2 = Noctua CPU coolers, FAN3–8 = 120 mm case fans).
- `anemoi/asrock16-2t/src/main.rs` `apply` — computes `raw_driving = max(all inputs, CPU)`, one
  `ctrl.duty(raw)`, `set_all_fans(pct)`, then reports temps/driving/fan readings.
- `anemos/src/{controller,curve,run}.rs` — the SDK builds **one** `Controller` from
  `ModuleInfo.curve_default_path` (env override `$AIOLOS_ETC_DIR/<filename>`) and passes
  `&mut ctrl` into `apply`; `Controller::new(path)` reads ONE flat curve file (`CurveCache`). The
  SDK cannot be modified, so per-zone curves must be **separate flat curve files** loaded into our
  own `Controller`s.

Affected contracts: `anemos-asrock16-2t.spec.md` (config + run behaviour). No protocol change — a
zone duty is still 8 per-fan bytes through the existing `0xd6` path; readings keep the same shape.

### Decisions (recorded before implementation)
1. **Config schema — separate optional per-zone curve files (back-compatible).**
   - The SDK-provided controller keeps reading the single shipped `asrock16-2t.curve.json` — this is
     the **uniform / fallback** curve, unchanged. Default ships uniform.
   - Two **optional** per-zone files are derived from the SDK curve path by replacing the
     `.curve.json` suffix: `asrock16-2t.cpu.curve.json` (CPU zone) and `asrock16-2t.case.curve.json`
     (case zone). Each is a *normal* curve file (own points + optional `sensitivity`).
   - **Zone mode activates only when BOTH zone files load a non-empty curve.** Otherwise uniform.
     Rationale: `Controller` is file-path based and only parses a flat curve, so reusing it verbatim
     for each zone (Option A) is the only design that does not duplicate the SDK's curve/EMA/floor
     logic or write temp files (Option B, rejected). Requiring both zone files avoids a confusing
     half-zoned state. The env override is honoured automatically (paths derive from `ctrl.path()`).
2. **Inputs per zone.** CPU zone (FAN1/2) ← `max(CPU k10temp)`. Case zone (FAN3–8) ←
   `max(all routed inputs = GPU + NVMe + any future source)`; **CPU is excluded from the case max**
   (the SOW's explicit intent: don't blast case fans for a CPU-only spike). Each zone has its own
   `Controller` → own EMA/deadband/sensitivity.
3. **`0xd6` per-fan.** New pure builder `duty_payload_per_fan([u8;8])` → 16 bytes, bytes 0–7 =
   FAN1..FAN8 (each clamped 1..=100), bytes 8–15 = `0x01` (non-zero). New `set_per_fan` routes
   through the same `regulate` claim/retry/release policy as `set_all_fans` (which is kept for
   uniform mode).
4. **Per-zone fail-safe.** All 8 fans are written in ONE `0xd6`; we cannot release one zone and hold
   the other. So if **either** zone has no driving temp or no usable curve, release the **whole**
   board to BMC auto (identical to today's whole-board fail-safe). Restore/Drop/`restore` unchanged.

## Plan
1. Curve-config schema for zones (back-compatible default = uniform).
2. asrock apply: per-zone driving + duty; one Controller per zone.
3. Spec/docs; on-hardware validation.

## Execution Log
### 2026-05-30
- Created (open) from the server-inspection idea list. No code.
- Implemented per-zone board-fan curves, internal to the asrock anemos (SDK untouched).
  - `anemoi/asrock16-2t/src/zones.rs` (new): `ZoneControllers` holds two `anemos::Controller`s
    (CPU + case), derives their curve paths by suffix from the SDK controller's resolved main path
    (`asrock16-2t.{cpu,case}.curve.json`), and `both_present()` re-probes the two files each tick via
    throwaway `CurveCache`s (no EMA side effects). `per_fan_duties(cpu,case)` composes the 8 duties;
    `zone_of()`/`Zone` partition the fans for compensation. Unit tests for path derivation, duty
    composition, fan partition.
  - `anemoi/asrock16-2t/src/board.rs`: `duty_payload` now builds the 16 bytes from 8 per-fan duties
    (bytes 0–7 = FAN1..FAN8 clamped 1..=100, bytes 8–15 held `0x01`); `FanBus::set_duty` and
    `regulate` take `&[i32;8]`; added `set_fans_per_fan`. `set_all_fans(pct)` kept (passes `[pct;8]`)
    for the uniform path. Tests updated + a new per-fan-payload test.
  - `anemoi/asrock16-2t/src/main.rs`: `apply` decides uniform-vs-zone live each tick; zone mode drives
    FAN1/2 from CPU temp and FAN3–8 from `max(all routed inputs)` via the two zone controllers; the
    whole board releases to BMC auto if either zone is blind (one `0xd6` sets all 8). New
    `ApplyOutcome` carries the decision into a mode-tagged `driving` reading. `AsrockDevice` gained a
    lazily-built `zones` field and `reset_zone_dampers()` (mirrors the SDK `ctrl.reset()` on a
    release tick). Outcome unit tests added.
- Build/clippy(all-targets)/fmt clean; `cargo test --workspace --no-run` compiles. Not executed on
  hardware (production cooler is `nvfd`; user runs the on-board validation).

## Validation
- Acceptance — per-zone split: covered by unit tests (`per_fan_duties` → FAN1/2=cpu, FAN3–8=case;
  zone driving reading) + the live mode decision in `apply`. On-hardware check (CPU-only vs GPU/NVMe
  load raising the correct zone; `0xd6` all-manual-non-zero honoured) is **pending the user's
  hardware run** — this branch is built in an isolated worktree and must not drive the live fans.
- Back-compat: with no zone files, `both_present()==false` → the uniform path is byte-for-byte the
  prior behaviour (`set_all_fans`); shipped `asrock16-2t.curve.json` and packaging unchanged.
- `0xd6` rule: `duty_payload` floors every controllable byte to ≥1 and holds tach slots at `0x01`
  (test `duty_payload_per_fan_places_each_fan_and_floors_tach_slots`).
- Fail-safe: zone-blind / no-usable-curve paths route through the existing `release_or_error`;
  restore/Drop/`restore` untouched.
- Reviewers: not run in-worktree (parallel-agent constraint); the user consolidates review at merge.

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
None yet.

## Regression Log
None yet.
