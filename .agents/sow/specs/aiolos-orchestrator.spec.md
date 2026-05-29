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
# globals (defaults): tick=3, timeout=2, detect_every=10, status_bind=0.0.0.0:9876
nvidia
asrock16-2t  input=nvidia
```
- `input=<module>`: relay that module's instances' last `readings` arrays into this module's
  `apply.inputs` (keyed by peer id). Unknown module directives are preserved but ignored
  (forward-compat). Unknown globals are warned + ignored.
- Globals: `tick`, `timeout`, `detect_every` (integer seconds), `status_bind` (`host:port`).
  Invariant `0 < timeout < tick` is enforced by clamping (and `tick` is floored to 2s so an
  integer `timeout` can be smaller).
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
   - `fatal` → keep instances, surface loudly, retry only on a long backoff (~300 s).
   - **unresponsive/crashed** (no reply / dead detect process) — backstop only: recycle it, keep
     instances (last good `found` is retained so reconcile never tears down on a detect outage).
   A `detect` process that exits is respawned. NVML/IPMI handles are initialised once per process
   (re-initialising per cycle leaks fds → EMFILE).
3. **Heartbeat** (every `tick`, default 3 s): for every `run` instance, write `apply` (with any
   routed `inputs`), then collect one response within `timeout` (default 2 s). **Fan-out then
   collect** — write to all, then collect all replies under one shared deadline; no instance waits
   on another. Both the stdin write and the stdout read are **non-blocking and deadline-bounded**:
   a module that stops reading its stdin, writes a partial line, or floods stdout without a newline
   is `SIGKILL`ed at the deadline — it can never wedge the instance thread (the isolation
   guarantee). A response line larger than 256 KiB is treated as a protocol violation and killed.
   A leading optional `hello` line is consumed/skipped.
4. **apply result handling:** `ok` → store readings (+ blackboard). `error` → keep the instance,
   surface the reason, retry next tick. `fatal` (module-declared) → respawn on a **long backoff**.
   Missed deadline / process exit / protocol violation → `SIGKILL` if needed and respawn with per-id
   crash-loop backoff (backstop). The module's own shutdown/EOF path performs the device-safe
   restore. When an instance is removed (vanished or dead) its blackboard entry is pruned, so stale
   readings are never relayed as `inputs`.
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
The orchestrator keeps each instance's last `readings`. For a module configured `input=X`, it
includes X's instances' readings (keyed by id) as `inputs` in this module's next `apply`. Values
are relayed verbatim and uninterpreted (orchestrator stays agnostic). Routing uses the previous
tick's values (one heartbeat stale) to keep instances order-independent within a tick.

## State & status web page
Holds: registry, per-module detect results, per-instance last readings + status + last error +
restart count + last-seen, captured stderr tail. Serves a **read-only** HTTP status page (bind
`127.0.0.1:<port>` by default) rendering all of it. Dependency-light (hand-rolled or `tiny_http`);
no async runtime required.

## Isolation guarantee (the core requirement)
Each `run` instance is a separate OS process with its own device handles. A wedged syscall in
one instance cannot block the orchestrator or siblings; worst case is that instance missing a
tick and being restarted. (A true uninterruptible kernel hang is unkillable by anyone but stays
harmless to others — orphaned, siblings keep ticking.)

## Configuration defaults
| Key | Default |
|---|---|
| `tick` | 3 s |
| `timeout` | 2 s (must be < tick; clamped if not) |
| `detect_every` | 10 s |
| `status_bind` | `0.0.0.0:9876` (user decision SOW-0001 #7; set `127.0.0.1:9876` to restrict) |

## Acceptance criteria
- Spawns/reconciles modules from the registry; routes `input=` data correctly.
- A module that hangs is killed within ~`timeout` and respawned; siblings keep ticking on time.
- SIGTERM cleanly shuts all modules down (each restores its device — via both the orchestrator's
  stdin-close and the module's own signal handler).
- A supervisor thread that panics is respawned (backoff-bounded) with no orphaned/duplicate
  instances; the device keeps being regulated or falls back to firmware in the interim.
- `aiolos restore` returns every configured module's device to firmware/BMC auto (ExecStopPost net).
- Status page reflects live readings, per-instance health, and recent errors.
- Idle RSS is single-digit MB; binary is low-MB.
