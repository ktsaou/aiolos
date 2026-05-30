# SOW-0012 - Curve-loading robustness (keep last-good + complain on reload; fail-to-start on invalid)

## Status

Status: open

Sub-state: requested 2026-05-30. Not started. Gate at activation. Behavioral change to the SDK
fail-safe — needs the user's edge-case decisions before implementation.

## Requirements

### Purpose
Make curve loading robust and loud:
1. **Live reload:** if the curve JSON is broken/partial, keep the previously-loaded curve and
   **complain (warn), do not fail.**
2. **Startup:** if a control module's curve is invalid / cannot be parsed, the anemos should
   **fail to start** (not run blind on firmware fallback).

### User Request
> "when re-reading curves, if the json is broken, it should keep the old curves loaded, not fail. It
> should complain, but not fail. When starting, if the curves are invalid / cannot be parsed, the
> anemos should fail. Not start."

### Assistant Understanding
Facts (current behavior, `anemos/src/curve.rs` + `run.rs`):
- **Reload already keeps last-good** on missing / partial / invalid / empty (`reload()` returns
  `false` and does not touch the active curve — `curve.rs:90-101`). **But it is silent** — no
  warning. So the "keep old" half is DONE; the "complain" half is MISSING.
- **Startup does NOT fail:** `CurveCache::new` loads once; if invalid/missing the curve starts empty;
  `run_loop` logs an error but **continues**, running the device on firmware/auto fallback (decision
  SOW-0001: "missing/empty curve → hold firmware control rather than command 0%").
- Sensor-only modules (no curve, `ModuleInfo.curve_* = None`) must remain exempt (they never have a
  curve).

### Acceptance Criteria
- Live reload of a broken/partial/empty curve: active curve unchanged + a **warning** logged
  (rate-limited / on transition, not every tick).
- Startup of a control module with an invalid curve: the module **exits/fails** (device falls to
  firmware — safe; the orchestrator surfaces the failure and retries with backoff). Sensor-only
  modules unaffected.
- Unit tests for: reload-broken-keeps-old-and-warns; startup-invalid-fails; sensor-only-unaffected.

## Analysis
Sources: `anemos/src/curve.rs` (`CurveCache::new`/`reload`, `Curve::from_json`/`is_empty`),
`anemos/src/run.rs` (`run_loop` empty-curve handling, `curve_path` Option), `anemos/src/controller.rs`.

Tension to reconcile: the existing fail-safe "missing/empty → firmware fallback (don't command 0%)"
vs the new "invalid at startup → fail." Failing to start is itself safe (firmware cools), so the two
are compatible — but the exact boundary of "invalid at startup" must be decided.

## Pre-Implementation Gate
Status: needs-user-decision (edge cases below) → then ready

Open decisions (need the user):
- **"Invalid at startup" boundary** — which of these FAIL at startup vs fall back to firmware?
  - (a) curve file present but **unparseable JSON** → FAIL (clearly the user's intent).
  - (b) curve file **missing** → fail, or keep the "no curve yet → firmware fallback"? (an operator
    who hasn't deployed a curve yet vs a misconfig).
  - (c) valid JSON but **no usable points / empty `{}`** → fail, or fallback?
  - Recommendation: fail on (a) and (c) (a configured-but-unusable curve is an error); treat (b)
    missing as fail too for consistency, UNLESS we want a "not yet deployed" grace.
- **Reload warning cadence:** warn once on the good→broken transition (and again on recovery), not
  every tick, to avoid log spam.
- **Failure mechanism:** the run process exits non-zero on a bad startup curve → orchestrator sees it
  die and respawns with crash-loop backoff (loud + safe), OR declare it `fatal` (long backoff).
  Confirm the desired supervisor behavior.
- **Reconcile SOW-0001:** this supersedes the "startup empty curve → firmware fallback" part of that
  decision (the runtime keep-last-good fail-safe is retained).

## Plan
1. `CurveCache::reload`: distinguish "valid & unchanged" from "present-but-broken" so a broken read
   can warn (on transition) while still keeping last-good.
2. `CurveCache::new` / `run_loop`: for a control module (curve configured), a failed initial load →
   fail (exit/fatal) per the decided boundary. Sensor-only unaffected.
3. Unit tests + (off-hardware) protocol behavior; update `anemos-*.spec.md` + `aiolos-protocol`
   notes; update the curve "floor/fail-safe" wording.

## Execution Log
### 2026-05-30
- Created. Captured the requirement + the current behavior (reload keeps-old but is silent; startup
  does not fail). Edge-case decisions listed for the user. No code.

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
