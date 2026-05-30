# Spec: aiolos orchestrator

Status: design. Authoritative behavior of the `aiolos` daemon. See also
`aiolos-protocol.spec.md` and `DESIGN.md`.

## Responsibilities (and non-responsibilities)
The orchestrator is **domain-agnostic**. It does: process lifecycle, the wire protocol, data
routing between modules, central state, and a status web page. It does **not** know about fans,
GPUs, temperatures, IPMI, or curves — all device knowledge lives in anemoi.

## Registry & globals
`/opt/aiolos/etc/aiolos.conf`. Module lines: one anemos per line with optional `key=value`
directives. Global lines: a bare `key=value` whose first token contains `=`.
```
# globals (defaults): base_tick=100 (ms), detect_every=10, max_backoff=300, status_bind=0.0.0.0:9876
nvidia                       every=500ms timeout=3
nvme
asrock16-2t  input=nvidia input=nvme
```
- `input=<module>`: relay that module's instances' last `readings` arrays into this module's
  `apply.inputs` (keyed by `module:id`). **Multiple sources** are allowed — repeat `input=` and/or
  use a comma list (`input=nvidia input=nvme` ≡ `input=nvidia,nvme`); order preserved, duplicates
  dropped. Unknown module directives are preserved but ignored (forward-compat). Unknown globals are
  warned + ignored.
- `every=<dur>` / `timeout=<dur>` (per-module, SOW-0013): this anemos's own cadence and its
  per-`apply` timeout. A **bare number is seconds** (`every=1` ≡ `every=1s`); a `ms`/`s` suffix is
  honoured (`every=500ms`). Defaults: `every=1s`, `timeout=5s`. `every` is floored to `base_tick`
  (it can never be finer than a scheduler wake); `timeout` MAY exceed `every`/`base_tick`. A
  malformed directive is warned + falls back to the default (never refuses to boot).
- A **module name MUST NOT contain `:`** (the blackboard/routing key is `module:id`, so a `:` in the
  name would make source prefix-matching ambiguous). A module line with a `:` in its name is
  rejected at parse time with a warning and does not run.
- Globals: `base_tick` (the scheduler wake period — **bare number is milliseconds**, default 100;
  `100ms`/`1s` accepted; floored to ≥ 10 ms), `detect_every`, `max_backoff` (integer seconds),
  `status_bind` (`host:port`). The pre-SOW-0013 lockstep globals `tick`/`timeout` are **obsolete** —
  if present they are warned + ignored (no `0 < timeout < tick` clamp exists any more). `max_backoff`
  caps the exponential respawn backoff (default 300 s; clamped up to ≥ 1 s).
- Module binaries: `<bin_dir>/<name>` (default `/opt/aiolos/bin`). Per-module config:
  `/opt/aiolos/etc/<name>.*`. Paths overridable via env (`AIOLOS_CONF`, `AIOLOS_BIN_DIR`) for
  testing/packaging.

## Lifecycle
1. **Start:** read registry → spawn one `detect` process per module.
2. **Detect/reconcile** (every `detect_every`, default 10 s): send `detect`; react to the module's
   declared `status` (it is never inferred from empty/exit/silence):
   - `ok` → `found` is authoritative; diff against running instances → spawn new `run <id>`, shut
     down vanished ones (empty `found` legitimately tears all down).
   - `error` → keep the current instances (a transient fault is NOT "no devices"), surface the
     reason, recycle the detect process, retry next cycle.
   - `fatal` → keep instances, surface loudly, retry only on a long backoff (the `max_backoff` cap).
   - **unresponsive/crashed** (no reply / dead detect process) — backstop only: recycle it, keep
     instances (last good `found` is retained so reconcile never tears down on a detect outage).
   A `detect` process that exits is respawned. NVML/IPMI handles are initialised once per process
   (re-initialising per cycle leaks fds → EMFILE).
3. **Scheduler** (SOW-0013 — replaces the lockstep heartbeat). The main loop wakes every `base_tick`
   (default 100 ms) and does only **non-blocking** work:
   - **Dispatch:** for every `run` instance, if it is **due** (`now - last_dispatch >= its every`)
     AND its worker is **idle** (no apply in flight), build its `inputs` from the **current**
     blackboard and send one `apply` (with `inputs`), marking it busy. **At most one apply is in
     flight per instance** — a busy instance is **delayed** (re-dispatched at a later wake when it
     frees), never queued and never replayed (run-latest-when-free). Its effective period is
     ≈ `max(every, apply_duration)` quantised to `base_tick`. A due-but-busy instance bumps a
     per-instance `skipped_busy` counter.
   - **Reap:** drain every result a worker posted asynchronously since the last wake and fold it into
     state (status/readings/blackboard + per-instance latency); a posting worker is idle again.
   Each instance worker runs its `apply` on **its own thread/process under its own `timeout`** — not
   gated by the base tick. Both the stdin write and stdout read are **non-blocking and
   deadline-bounded**: a module that stops reading its stdin, writes a partial line, or floods stdout
   without a newline is `SIGKILL`ed at *its* `timeout` — it can never wedge its worker or stall a
   sibling (the isolation guarantee). A response line larger than 256 KiB is a protocol violation and
   killed. A leading optional `hello` line is consumed/skipped. No instance ever blocks the scheduler
   or another instance: the loop never waits on a reply.
4. **apply result handling:** `ok` → store readings (+ blackboard), record latency, mark idle.
   `error` → keep the instance, surface the reason, retry next time it is due. `fatal`
   (module-declared) → respawn on a **long backoff** (jumps straight to the `max_backoff` cap).
   Missed `timeout` / process exit / protocol violation → `SIGKILL` if needed and respawn with per-id
   **exponential** crash-loop backoff (2,4,8,… seconds, capped at `max_backoff`; backstop). aiolos
   **never gives up** — it retries forever, only ever slowing to the cap. The module's own
   shutdown/EOF path performs the device-safe restore. When an instance is removed (vanished or dead)
   its blackboard entry **and its scheduler slot** are pruned, so stale readings are never relayed as
   `inputs`.
   - A **control module that cannot load a usable curve at startup** declares this `fatal` on its
     first `apply` (so the reason reaches the status page) and exits non-zero, leaving its device
     under firmware/auto; the supervisor then respawns it on the `max_backoff` cap (see the
     anemos specs' fail-safe sections). A *runtime* curve breakage is NOT fatal — the module keeps
     its last-good curve and warns.
5. **Shutdown (SIGTERM):** close every instance's stdin → modules restore + exit → reap → exit.
   Modules ALSO self-restore on the SIGTERM they receive directly (see below), so shutdown is
   robust even without the orchestrator's orchestration.
6. **Supervisor watchdog:** each module's supervisor runs in its own thread; if one dies (panics),
   the main loop respawns it (backoff-bounded, never gives up). A dying supervisor's `Drop` shuts
   down and deregisters its own instances first (when not in a global shutdown), so the replacement
   re-detects from a clean slate — no orphaned or duplicated instances on a device.

### Process kill discipline (devices must never be stranded in manual)
- **KillMode is the systemd default (`control-group`).** On stop, SIGTERM reaches the orchestrator
  AND every module at once; each module catches it and self-restores (protocol spec: signal
  self-restore). The unit names no modules and assumes nothing about the configured set.
- **Instance teardown escalates, never SIGKILL-first:** close stdin (EOF → module restores) →
  grace → `SIGTERM` (module's handler restores) → grace → `SIGKILL` only as an absolute last resort
  for a wedged child.
- **`aiolos restore` (one-shot):** reads the registry and runs every configured module's uniform
  `restore` one-shot (concurrent, per-module time-bounded). Wired to systemd `ExecStopPost` as the
  belt-and-suspenders for a hard kill (SIGKILL/crash/OOM) where modules could not self-restore.
  Keeps the unit config-agnostic (no module names in the unit file).

## Data routing (blackboard)
The orchestrator keeps each instance's last `readings`. For a module configured `input=X [Y …]`, it
includes every named source's instances' readings (keyed by `module:id`, so the consumer can
attribute each reading to its source module and keys never collide across sources) as `inputs` in
this module's next `apply`. Values are relayed verbatim and uninterpreted (orchestrator stays
agnostic). Under the SOW-0013 scheduler each consumer receives the producer's **most recent
completed** readings as of the consumer's own dispatch (built from the live blackboard at dispatch
time) — producers and consumers run on independent cadences, so there is no within-tick ordering
dependency.

## State & status web page
Holds: registry, per-module detect results, per-instance last readings + status + last error +
restart count + last-seen, captured stderr tail, and per-instance **scheduler state** (SOW-0013:
busy/idle, last dispatch, last `apply` latency, and the delay-not-skip `skipped_busy` counter, kept
in `AppState.sched` for the status page / metrics to surface). Serves a **read-only** HTTP status
page (bind `127.0.0.1:<port>` by default) rendering all of it. Dependency-light (hand-rolled or
`tiny_http`); no async runtime required.

## Isolation guarantee (the core requirement)
Each `run` instance is a separate OS process with its own device handles, driven by its own worker
thread on its own cadence. A wedged syscall in one instance cannot block the orchestrator or
siblings; the scheduler never waits on a reply. Worst case is that instance running late (a slow
apply just delays *its own* next dispatch) or being killed at *its* `timeout` and restarted — a
healthy sibling keeps firing at its `every` regardless. (A true uninterruptible kernel hang is
unkillable by anyone but stays harmless to others — orphaned, siblings keep ticking.)

## Configuration defaults
| Key | Default |
|---|---|
| `base_tick` | 100 ms (scheduler wake period; bare number = ms; floored to ≥ 10 ms) |
| per-module `every` | 1 s (the anemos's own cadence; bare number = s; floored to `base_tick`) |
| per-module `timeout` | 5 s (per-`apply` timeout; bare number = s; may exceed `every`/`base_tick`) |
| `detect_every` | 10 s |
| `max_backoff` | 300 s (cap on the exponential respawn backoff; clamped up to ≥ 1 s) |
| `status_bind` | `0.0.0.0:9876` (user decision SOW-0001 #7; set `127.0.0.1:9876` to restrict) |

(The pre-SOW-0013 lockstep globals `tick`/`timeout` are obsolete — warned + ignored if present.)

## Acceptance criteria
- Spawns/reconciles modules from the registry; routes `input=` data correctly (most-recent completed
  readings at the consumer's dispatch).
- Each anemos runs on its own cadence; a slow anemos runs at ≈ `max(every, apply_duration)` and is
  **never killed** for being slow (only a `timeout` breach kills+restores). A hung/partial anemos is
  killed at *its* `timeout` and respawned; siblings keep firing at their `every` throughout.
- `timeout` may exceed `every`/`base_tick` (the old `timeout < tick` clamp is gone).
- SIGTERM cleanly shuts all modules down (each restores its device — via both the orchestrator's
  stdin-close and the module's own signal handler).
- A supervisor thread that panics is respawned (backoff-bounded) with no orphaned/duplicate
  instances; the device keeps being regulated or falls back to firmware in the interim.
- `aiolos restore` returns every configured module's device to firmware/BMC auto (ExecStopPost net).
- Status page reflects live readings, per-instance health, and recent errors.
- Idle RSS is single-digit MB; binary is low-MB.
