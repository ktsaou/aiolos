# Spec: aiolos orchestrator

Status: design. Authoritative behavior of the `aiolos` daemon. See also
`aiolos-protocol.spec.md` and `DESIGN.md`.

## Responsibilities (and non-responsibilities)
The orchestrator is **domain-agnostic**. It does: process lifecycle, the wire protocol, data
routing between modules, central state, and a status web page. It does **not** know about fans,
GPUs, temperatures, IPMI, or curves — all device knowledge lives in anemoi.

## Registry
`/opt/aiolos/etc/aiolos.conf`, one anemos per line with optional `key=value` directives:
```
nvidia
asrock16-2t  input=nvidia
```
- `input=<module>`: relay that module's instances' last `readings` into this module's `apply`
  `inputs`. Extensible directives (future): `args=`, `every=`, `timeout=`.
- Module binaries: `/opt/aiolos/bin/<name>`. Per-module config: `/opt/aiolos/etc/<name>.*`.

## Lifecycle
1. **Start:** read registry → spawn one `detect` process per module.
2. **Detect/reconcile** (every `detect_every`, default 10 s): send `detect` to each detect
   process → diff returned `id`s against running `run` instances → spawn new `run <id>`, kill
   vanished ones. Handles devices appearing/dropping (e.g. a GPU falling off the bus and
   returning).
3. **Heartbeat** (every `tick`, default 3 s): for every `run` instance, write `apply` (with any
   routed `inputs`), then collect one response within `timeout` (default 2 s). **Fan-out then
   collect** — write to all, then poll all reply fds under one deadline; no instance waits on
   another.
4. **Timeout/exit:** missed deadline or process exit → `SIGKILL` if needed, respawn next cycle
   with crash-loop backoff. The module's own EOF path performs the device-safe restore.
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
| `timeout` | 2 s (must be < tick) |
| `detect_every` | 10 s |
| status bind | 127.0.0.1:<port> |

## Acceptance criteria
- Spawns/reconciles modules from the registry; routes `input=` data correctly.
- A module that hangs is killed within ~`timeout` and respawned; siblings keep ticking on time.
- SIGTERM cleanly shuts all modules down (each restores its device).
- Status page reflects live readings, per-instance health, and recent errors.
- Idle RSS is single-digit MB; binary is low-MB.
