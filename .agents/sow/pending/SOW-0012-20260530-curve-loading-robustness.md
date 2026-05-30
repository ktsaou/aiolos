# SOW-0012 - Curve-loading robustness (keep last-good + complain on reload; fail-to-start on invalid)

## Status

Status: open

Sub-state: requested 2026-05-30; edge-case decisions RESOLVED (user, 2026-05-30) — gate ready. Not
started (queued). Note: pulls in a general orchestrator change — exponential respawn backoff with a
config-capped max (default 300 s).

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
Status: ready (decisions resolved by user 2026-05-30)

Resolved decisions (user, 2026-05-30):
1. **"Invalid at startup" → FAIL on ALL three** (unparseable JSON, missing file, empty/no-usable
   points). A control module will NOT start regulating without a valid curve. aiolos **never gives
   up** — it retries forever (with backoff, #3) — and **the device is left under firmware control**
   the whole time (safe). (Supersedes the SOW-0001 "startup empty → firmware fallback" semantics;
   the *runtime* keep-last-good fail-safe is retained.)
2. **Reload warning: log EVERY tick** while the curve file is broken/unparseable (keep last-good,
   complain loudly each tick — not just on transition).
3. **Failure mechanism:** the anemos returns a **structured error to aiolos** (a `fatal`
   status+reason over the protocol) **AND exits non-zero**. aiolos respawns it with **exponential
   backoff, capped at a config max — default 5 min (300 s)**. → introduces a new orchestrator backoff
   policy + config global (e.g. `max_backoff=300`), applied generally to fatal/crash respawns (not
   just curves). NB: per-anemos curve failure must still surface its reason on the status page.

Implementation notes:
- A control module with a bad startup curve should emit `{"status":"fatal","error":"curve …"}` on
  its first `apply` (structured reason for the status page) then exit non-zero — never silently die.
- Sensor-only modules (`curve_* = None`) remain exempt (no curve expected).
- The exponential-backoff-with-config-cap is a general supervision change shared with SOW-0012's
  startup-fail path and any other declared-fatal/crash respawn.

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

### 2026-05-30 — implementation
Implemented all three decisions. Files touched (within the agreed domain):

**Live reload — broken-vs-unchanged + warn every tick (decision 1, 2)**
- `anemos/src/curve.rs`:
  - `CurveCache::reload()` now returns a `ReloadOutcome` enum (`Unchanged` / `Changed` /
    `Broken{reason}`) instead of a bare `bool`. This is the mechanism that distinguishes "valid &
    unchanged" (silent) from "present-but-broken" (warn). `Broken` carries a `BrokenReason`
    (`Unreadable` / `InvalidJson` / `NoUsablePoints`) — the three startup-invalid cases plus the
    read error. Last-good curve/α are untouched on `Broken` (runtime fail-safe preserved).
  - `CurveCache::new()` captures the FIRST load's outcome into `initial: Option<BrokenReason>`,
    exposed via `initial_error()`. This is how a control module learns its startup curve was
    unusable without changing `Controller::new`'s signature.
- `anemos/src/controller.rs`:
  - `duty()` matches the new outcome: `Changed` → info (as before); `Broken` → `warn!` **every
    tick** (decision 2 — complain loudly while broken); `Unchanged` → silent.
  - Added `initial_curve_error()` (additive — `Controller::new(String)`/`duty()`/`reset()`/`path()`/
    `curve_is_empty()` public signatures unchanged).
- `anemos/src/lib.rs`: re-export `BrokenReason`, `ReloadOutcome`.

**Startup fail for a control module (decision 1, 3)**
- `anemos/src/run.rs`:
  - `run_loop` now returns `i32` (exit code). For a control module (`curve_default_path = Some`)
    whose `ctrl.initial_curve_error()` is `Some`, it **never opens the device** and enters
    `startup_curve_fatal_loop`: it answers the first `apply` with
    `{"status":"fatal","error":"startup: curve invalid: <reason>"}` (so the reason shows on the
    status page) then exits non-zero. `shutdown` → ok + exit 0; EOF/signal → exit non-zero (never
    regulated, nothing to restore — firmware/auto keeps the device cooled).
  - Sensor-only modules (`curve = None`) skip the check entirely (`curve_configured` is false).
  - The pre-existing `init_logging().with_ansi(false)` line was NOT touched.

**Orchestrator: capped exponential respawn backoff (decision 3)**
- `aiolos/src/config.rs`: new global `max_backoff` (default 300 s; `parse_secs` validates; clamped
  up to ≥ 1 s with a warning). Added to `Config`.
- `aiolos/src/module.rs`: threaded `max_backoff` into `Supervisor`. The per-id backoff and the
  declared-fatal jump now use it via a pure `backoff_delay_secs(count, max_secs) =
  2^count.min(max_secs.max(1))`. `record_fatal` sets `count = BACKOFF_SATURATE_SHIFT (32)` so
  `2^count` saturates to exactly the cap for ANY `max_backoff` (the old code hardcoded `9`/`300`).
  The detect-process fatal path returns `self.max_backoff` (was the hardcoded `FATAL_BACKOFF`
  const, now removed). This is general (crash + declared-fatal + curve-startup-fatal all share it).
- `aiolos/src/main.rs`: pass `cfg.max_backoff` to both `run_module` call sites (initial spawn +
  watchdog respawn); log `max_backoff_s` at startup. (The supervisor-THREAD panic-respawn keeps its
  own fixed 5 s floor — a separate, rarer recovery axis, intentionally not changed; the per-instance
  backoff is the path that respawns a curve-fatal anemos.)

**Artifacts**
- `packaging/aiolos.conf`: documented the `max_backoff=300` default in the globals comment block.
- Specs: `aiolos-orchestrator.spec.md` (globals + config table + lifecycle apply/detect fatal
  wording + the control-module-startup-fatal note), `aiolos-protocol.spec.md` (generalized the
  fatal long-backoff to `max_backoff`; added the invalid-curve-at-startup apply rule),
  `anemos-nvidia.spec.md` + `anemos-asrock16-2t.spec.md` (split curve fail-safe into
  startup-fatal vs runtime-keep-last-good; asrock no longer lists "curve missing/empty" as a
  release-to-auto trigger — runtime breakage keeps last-good).
- Skill `project-create-anemos` (both `.agents` canonical + the untracked `.claude` mirror kept in
  sync): added a "Curve loading — the SDK handles it for you" section.

**Tests added (compile-only; never executed here — production system, user runs them):**
- `curve.rs`: `reload_distinguishes_changed_unchanged_and_broken` (full edit→break→recover cycle,
  keeps last-good, reports Broken every read); `initial_error_classifies_each_invalid_case` (the 3
  startup-invalid cases + a valid one); updated `cache_reload_keeps_last_good_on_bad_read`.
- `controller.rs`: `duty_keeps_last_good_curve_when_file_breaks_at_runtime` (regulates on last-good
  after a runtime break); `initial_curve_error_is_set_for_an_invalid_startup_curve`.
- `run.rs`: `a_control_module_with_an_invalid_curve_detects_a_startup_error`;
  `a_sensor_only_module_is_exempt_from_the_startup_curve_check`;
  `startup_fatal_applied_line_is_well_formed_protocol`.
- `config.rs`: `max_backoff` in `parses_globals_and_modules` + `defaults_when_empty`;
  `max_backoff_clamped_up_and_parsed`; `max_backoff=abc` error case.
- `module.rs`: `backoff_is_exponential_and_capped_at_max` (2,4,8,… → cap; saturate-jump caps for
  any max); `backoff_cap_never_busy_loops_on_a_zero_cap`.

## Validation
- `cargo build --release` — clean.
- `cargo clippy --all-targets` — clean (no warnings).
- `cargo fmt --all -- --check` — clean (ran `cargo fmt --all` once to fix one wrapped line).
- `cargo test --workspace --no-run` — all test binaries compile (tests NOT run; the live aiolos is
  cooling production — the user runs the suite).
- Acceptance-criteria mapping:
  - Reload of broken/partial/empty curve keeps the active curve + warns every tick →
    `curve.rs::reload_distinguishes_…` + `controller.rs::duty_keeps_last_good_…` (the warn is in
    `duty()`'s `Broken` arm).
  - Startup of a control module with an invalid curve (all 3 cases) → fatal + non-zero exit, device
    on firmware → `run_loop`'s gate + `startup_curve_fatal_loop`; classification covered by
    `curve.rs::initial_error_classifies_each_invalid_case` + `run.rs` tests.
  - Sensor-only modules unaffected → `run.rs::a_sensor_only_module_is_exempt_…`.
  - Exponential backoff capping → `module.rs::backoff_is_exponential_and_capped_at_max`.
- Same-failure search: grepped for other `2u64.saturating_pow`/hardcoded `300` backoff sites — the
  only ones were `backoff_expired`, `record_fatal`, and the detect `FATAL_BACKOFF`, all now routed
  through `max_backoff`. No other `reload()`-as-bool callers exist (only `controller.rs`).
- Sensitive-data gate: no secrets/IPs/credentials added to any artifact.
- Reviewer findings: external reviewers NOT run (autonomous worktree per instructions); the user
  runs review + the test suite on integration.

## Outcome
Implemented and verified to build/clippy/fmt/compile clean. Left in `pending/` (status `open`) per
the worktree instructions — the user validates on real hardware and closes/moves the SOW.

## Lessons Extracted
- Returning a 3-state `ReloadOutcome` (not a bool) is what makes "warn only when broken, only while
  it stays broken" expressible without a separate transition flag — the caller decides log level
  per tick from the outcome.
- Capping the exponential backoff via `count.min(cap)` plus a "saturate" sentinel count for the
  declared-fatal jump keeps ONE formula for both the crash-loop ramp and the fatal long-backoff, and
  stays correct for any configured `max_backoff` (the old hardcoded `9`/`300` only worked for a
  300 s cap).

## Followup
None yet.

## Regression Log
None yet.
