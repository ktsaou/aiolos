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
2. **Detect/reconcile** (every `detect_every`, default 10 s): send `detect` to each detect
   process → diff returned `id`s against running `run` instances → spawn new `run <id>`, kill
   vanished ones. Handles devices appearing/dropping (e.g. a GPU falling off the bus and
   returning).
3. **Heartbeat** (every `tick`, default 3 s): for every `run` instance, write `apply` (with any
   routed `inputs`), then collect one response within `timeout` (default 2 s). **Fan-out then
   collect** — write to all, then collect all replies under one shared deadline; no instance waits
   on another. Both the stdin write and the stdout read are **non-blocking and deadline-bounded**:
   a module that stops reading its stdin, writes a partial line, or floods stdout without a newline
   is `SIGKILL`ed at the deadline — it can never wedge the instance thread (the isolation
   guarantee). A response line larger than 256 KiB is treated as a protocol violation and killed.
   A leading optional `hello` line is consumed/skipped.
4. **Timeout/exit:** missed deadline or process exit → `SIGKILL` if needed, respawn within a
   detect/reconcile step (sub-`detect_every`) with per-id crash-loop backoff. The module's own
   shutdown/EOF path performs the device-safe restore. When an instance is removed (vanished or
   dead) its blackboard entry is pruned, so stale readings are never relayed as `inputs`.
5. **Shutdown (SIGTERM):** close every instance's stdin → modules restore + exit → reap → exit.

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
- SIGTERM cleanly shuts all modules down (each restores its device).
- Status page reflects live readings, per-instance health, and recent errors.
- Idle RSS is single-digit MB; binary is low-MB.
