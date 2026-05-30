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
Status: needs-user-decision on a couple of defaults → then ready

Resolved (user, 2026-05-30): the semantics above (100 ms base, per-anemos `every`/`timeout`,
one-in-flight, run-latest-when-free, delay-not-skip, kill-only-on-timeout, async blackboard, latency
tracking).

Open decisions (defaults — confirm at activation):
- `base_tick` default **100 ms**; per-anemos default `every` **1 s**, `timeout` **5 s**.
- Whether a per-anemos `every` may be < the *detect* cadence (independent — yes), and the minimum
  (`every ≥ base_tick`).
- Interaction with SOW-0012's **exponential respawn backoff** (shared supervision path) and the
  status-page/metrics fields for latency/last-seen.
- Curve EMA/`sensitivity` retuning guidance for sub-second cadences (live-tunable; doc only).

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
