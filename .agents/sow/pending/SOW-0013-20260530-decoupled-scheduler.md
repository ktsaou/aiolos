# SOW-0013 - Decoupled per-anemos scheduler (fast base tick, per-anemos cadence, slow-safe)

## Status

Status: open

Sub-state: designed with the user 2026-05-30 (semantics agreed). Not started. Foundational
orchestrator change; SOW-0009 (and faster cooling response generally) depend on it.

## Requirements

### Purpose
Replace the lockstep heartbeat (one global `tick`, every instance must answer within `timeout < tick`)
with a **fast non-blocking base scheduler** that runs each anemos on its **own cadence**, caches last
results, and lets a slow/hung anemos fall behind **without affecting the others** — cutting reaction
latency (≈1–2 s instead of 3–6 s) while keeping the isolation guarantee.

### User Request
> Discussion 2026-05-30: "tick every [100 ms], run anemoi based on their tick, caching last results
> and propagating values as required… slow anemoi will not skip a tick, they will be delayed and run
> at the next 100 ms period aiolos wakes up." + per-anemos `every`/`timeout`.

### Assistant Understanding
Facts (current code):
- `main.rs::heartbeat` fans out `apply` to all instances, then **synchronously collects** all replies
  under one shared deadline, every `tick`. `config.rs` enforces `0 < timeout < tick` and floors
  `tick` to 2 s. So it's lockstep; a per-anemos timeout cannot exceed the tick.
- `DESIGN.md §4` already anticipates `every=<sec>`/`timeout=<sec>` per-anemos directives.
- Each instance already has its own worker thread + OS process (isolation primitive in place).

Agreed semantics (the design to implement):
- aiolos wakes every **`base_tick`** (default **100 ms**, configurable) and does only **non-blocking**
  work: for each instance, if `now − last_dispatch ≥ every` **and** the worker is **idle** → dispatch
  a fresh `apply` (inputs built from the current blackboard); reap any results workers have posted
  asynchronously; nothing else.
- Each worker runs its `apply` under its own **`timeout`** in its own thread/process; on completion (or
  timeout → SIGKILL + device restore) it writes readings to the blackboard and goes idle.
- **At most ONE apply in flight per instance; no backlog/queue.** A busy anemos is **delayed**, not
  skipped-to-a-grid and not caught-up: when it frees, the next base wake re-dispatches a **fresh**
  apply (run-latest-when-free, never replay stale ticks).
- **Effective period ≈ `max(every, apply_duration)`** quantised to `base_tick`. Only a `timeout`
  breach causes a kill+restore; everything else is "runs a bit later."
- Per-instance **latency / last_run / skip(busy) counters** tracked + surfaced (status page / metrics).

### Acceptance Criteria
- A slow or hung anemos (mock `hang`/`partial`/slow apply) never delays a healthy sibling's cadence;
  the healthy one keeps firing at its `every`.
- A slow anemos runs at `max(every, apply_duration)`; a hung one is killed at its `timeout` + restored.
- `timeout` may exceed `every`/`base_tick`; the old `timeout < tick` clamp is removed.
- Per-anemos `every=`/`timeout=` registry directives honoured (+ sensible defaults); `base_tick`
  configurable global.
- Routing delivers each producer's **most recent completed** readings at the consumer's dispatch.
- Per-instance latency/last-seen surfaced. Existing isolation integration tests still pass (extended).

## Analysis
Sources: `aiolos/src/main.rs` (`heartbeat`, main loop, `build_inputs`, `apply_results`),
`aiolos/src/module.rs` (instance worker), `aiolos/src/instance.rs` (`tick`, kill/restore),
`aiolos/src/config.rs` (tick/timeout clamps), `aiolos/src/bin/mock.rs` (hang/partial behaviors for
tests), `DESIGN.md §4/§6/§7`.

This is a **central rewrite** (heartbeat → async scheduler) — highest blast radius of the backlog.
The existing per-instance worker threads + the `RwLock` blackboard are the right substrate; the
change is dispatch policy (timer + idle-gated) and **async result collection** (workers post results
when done, the loop never blocks on a reply).

## Pre-Implementation Gate
Status: resolved — implemented with the agreed defaults (2026-05-30).

Resolved (user, 2026-05-30): the semantics above (100 ms base, per-anemos `every`/`timeout`,
one-in-flight, run-latest-when-free, delay-not-skip, kill-only-on-timeout, async blackboard, latency
tracking).

Decisions applied at implementation (the agreed defaults; operator-overridable):
- `base_tick` default **100 ms** (bare number = ms; floored to ≥ 10 ms). Per-anemos default `every`
  **1 s**, `timeout` **5 s** (bare number on a module line = seconds; `ms`/`s` suffix accepted).
- `every` is **independent** of the detect cadence and is **floored to `base_tick`** (`every ≥
  base_tick`). `timeout` may exceed `every`/`base_tick` (old `timeout < tick` clamp removed).
- SOW-0012's **exponential respawn backoff** is on the supervisor's reap path (module.rs) and is
  preserved unchanged — the scheduler only changes *dispatch* (per-instance, idle-gated) and *result
  collection* (async via a shared channel). A `timeout`/crash still posts a fatal `TickReport`, the
  worker exits, and the supervisor applies the same per-id backoff (the `last_status=="fatal"` ↔
  declared-fatal path is unchanged). Latency/skip fields live in `AppState.sched`.
- Curve EMA/`sensitivity` retuning for sub-second cadences: doc-only follow-up (live-tunable).

Note: this subagent implemented the change in an isolated worktree and left the SOW in `pending/`
(not marked `completed`) for the user's review + operator-gated cutover (nvfd keeps cooling GPUs
until cutover).

Risk/blast radius: HIGH (core loop). Mitigation: keep the per-instance worker/process model; lean on
the mock's `hang`/`partial` + new "slow" behavior to prove isolation; stage behind thorough tests
before any cutover (operator-gated, like SOW-0004/0005).

## Plan (sketch)
1. Config: `base_tick` global; per-module `every=`/`timeout=` directives; drop `timeout < tick` clamp.
2. Scheduler: replace `heartbeat` with a 100 ms non-blocking dispatch/reap loop; per-instance
   idle/busy/last_dispatch/latency; async result → blackboard.
3. Worker/instance: post results asynchronously; kill+restore only on `timeout`.
4. Status/metrics: per-instance latency, last-seen age, skip counts.
5. Tests (mock slow/hang/partial isolation; cadence; timeout>every); specs/DESIGN updates.

## Execution Log
### 2026-05-30
- Created (open) from the 2026-05-30 design discussion. Semantics agreed. No code.
- Implemented the decoupled scheduler (isolated worktree). Files touched:
  - `aiolos/src/config.rs`: dropped the lockstep `tick`/`timeout` globals (now warned + ignored,
    no `timeout < tick` clamp). Added `base_tick` global (bare number = ms, default 100, `ms`/`s`
    accepted, floored to ≥ 10 ms). Added per-module `every=`/`timeout=` directives (bare number =
    seconds; defaults `every=1s`, `timeout=5s`), parsed from each entry's `unknown_directives`
    (registry.rs is out of domain — `every`/`timeout` still land there and are interpreted here).
    `every` floored to `base_tick`. New `ModuleSchedule` + `Config.schedules` + `schedule_for()`.
    `parse_dur_ms` + `Unit` helper. Tests rewritten for the new model.
  - `aiolos/src/instance.rs`: `InstanceCmd::Tick` no longer carries a per-call reply channel; added
    `TickReport { key, result, latency }` (async result envelope). Removed the unused dead method
    `TickStatus::is_declared_fatal` (was already unused at HEAD; surfaced a dead-code warning) to
    keep the build warning-free.
  - `aiolos/src/module.rs`: `run_module`/`Supervisor` take a shared `mpsc::Sender<TickReport>`;
    each worker is spawned with its instance key + a clone of that sender and posts its result
    asynchronously (with measured latency) instead of replying synchronously.
  - `aiolos/src/main.rs`: replaced `heartbeat` (fan-out-then-collect) with the non-blocking
    scheduler: the loop wakes every `base_tick` and runs `reap_results` (drain `try_iter`, fold to
    state, clear busy, record latency) then `dispatch_due` (per-instance due+idle gate via the pure
    `should_dispatch`, build inputs from the live blackboard, mark busy, send `Tick`; prune dead
    slots; count due-but-busy as `skipped_busy`). Added `AppState.sched: HashMap<String,
    InstanceSched>` (busy/last_dispatch/last_latency/skipped_busy) with a `Default` so the already
    merged `status_page.rs` compiles unchanged. Startup log now reports `base_tick_ms`.
  - `aiolos/src/bin/mock.rs`: added a `slow` behavior (sleeps `SLOW_MS`, default 800, then replies
    ok) to exercise delay-not-skip.
  - `aiolos/tests/orchestrator.rs`: harness conf now `base_tick=50`; hung/partial siblings given
    `timeout=1`; added `slow_sibling_is_delayed_not_skipped_and_never_killed` (asserts the slow one
    makes progress AND `starts == 1`, i.e. never killed) while a healthy sibling keeps ticking.
  - `packaging/aiolos.conf`: rewrote the GLOBALS comment section (base_tick + per-module
    every/timeout + scheduler model). Module lines at the bottom untouched (another agent's domain).
  - Specs: `aiolos-orchestrator.spec.md` (registry/globals, lifecycle scheduler, result handling,
    routing, state, defaults table, acceptance criteria); `aiolos-protocol.spec.md` (apply cadence,
    most-recent-completed inputs, per-module timeout); skill `project-anemos-protocol` timeout note.

## Validation
- Build: `cargo build --release` — clean, **zero warnings** (whole workspace).
- Lints: `cargo clippy --all-targets` — clean, no warnings.
- Format: `cargo fmt --all -- --check` — clean (no diff).
- Tests COMPILE only (per the production-safety constraint — the user runs them):
  `cargo test --workspace --no-run` — builds clean. NOT executed in this worktree.
- New/updated unit tests (compiled, not run here):
  - config: `base_tick` units + clamp; per-module every/timeout defaults+overrides; `every` floored
    to `base_tick`; `timeout` may exceed `every`/`base_tick`; bad directive falls back; obsolete
    `tick`/`timeout` ignored.
  - main: `should_dispatch` due+idle / skip-when-busy / every≥base_tick; `apply_results` clears busy
    + records latency; the existing blackboard-resurrection race guard (ported to `TickReport`).
  - integration: `slow_sibling_is_delayed_not_skipped_and_never_killed` plus the reworked
    hung/partial isolation tests (healthy sibling keeps firing; bad one killed at its `timeout`).
- Acceptance-criteria mapping (evidence is the compiled tests above; runtime verification is the
  user's, like SOW-0004/0005):
  - slow/hung never delays a healthy sibling → `slow_sibling_…`, `hung_sibling_…`,
    `partial_line_flood_…` (healthy `good` reaches ≥3 applies regardless).
  - slow runs at max(every, apply_duration), hung killed at timeout+restored → `slow_…` (starts==1,
    applies grow) vs `hung_…`/`partial_…` (starts≥2).
  - `timeout` may exceed `every`/`base_tick`; old clamp removed → `timeout_may_exceed_every_…`.
  - per-module every/timeout honoured + base_tick configurable → `module_schedule_…`,
    `base_tick_accepts_units_…`.
  - routing delivers most-recent completed readings → `input_routing_…`, `multi_input_routing_…`.
  - per-instance latency/last-seen surfaced → `AppState.sched` (busy/last_dispatch/last_latency/
    skipped_busy); status-page/metrics rendering of these fields is a follow-up (status_page.rs is
    another agent's domain in this wave — left compiling unchanged).
- Sensitive-data gate: none written (no BMC IP/creds/serials in code, tests, specs, or conf).
- Same-failure search: no other lockstep `tick`/`timeout` consumers remain (grepped); the only
  `< tick`/`< heartbeat` contract statements were in the two specs + the protocol skill, all fixed.
- Reviewer findings: none run by this subagent (no external reviewers per the task). The user's
  review/cutover gate remains (nvfd keeps cooling until aiolos is cut over).

### Follow-ups identified
- Surface `AppState.sched` (latency / skipped_busy / busy) on the status page + `/metrics`
  (status_page.rs owner) — data is already in `AppState`.
- Curve EMA/`sensitivity` retuning guidance for sub-second cadences (doc-only; live-tunable).

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
None yet.

## Regression Log
None yet.
