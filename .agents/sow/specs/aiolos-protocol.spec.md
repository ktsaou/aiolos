# Spec: aiolos ↔ anemos wire protocol

Status: implemented. This is the authoritative wire contract; `DESIGN.md` holds rationale.

## Transport & framing
- Each anemos is launched as a process. aiolos writes **requests to stdin** and reads **responses from stdout**.
- **One line = one complete JSON object.** Newline (`\n`) is the only delimiter. No pretty/multi-line JSON.
- **Strict half-duplex:** aiolos sends one request, then reads exactly one response before the next request.
- A module never writes to stdout except as a response, with one optional exception: a single startup `hello` line.
- **stdout is protocol-only.** Logs/diagnostics/warnings go to **stderr**. A stray stdout byte is a protocol bug.

## Modes (argv)
- `<module> detect` — persistent process that answers `detect` requests.
- `<module> run <ID>` — persistent process bound to one detected ID; answers `apply`/`shutdown`.
- `<module> info [ID]` / `<module> collect [ID]` — one-shot read-only live snapshot. It opens
  devices in observe mode, calls the SDK `collect` path, emits a `detect`-shaped response with live
  signal values, and exits. It MUST NOT claim, set, release, or restore hardware.
- `<module> schema` — one-shot schema-only detect response for humans/tools.
- `<module> restore` — one-shot: restore every device this module manages to firmware/auto-safe, then exit. Idempotent.

## hello (optional, module → orchestrator, once at startup)
```json
{"hello":{"proto":2,"name":"nvidia","modes":["detect","run"]}}
```
`proto` is the protocol version (`2`). aiolos skips a leading `hello` on detect/run streams.

## Status model
Every `detect`, `info`/`collect`, and `apply` response carries `status` ∈ {`ok`,`error`,`fatal`}.
- `ok` — the module did its job; `units`/`components`/`signals` are authoritative (empty is real).
  An optional `error` is a non-fatal warning, not a failed report.
- `error` — transient: it could not do the job this time. Not "no devices". aiolos keeps existing instances and retries.
- `fatal` — cannot work for this ID/host now (wrong hardware, missing capability, invalid startup curve). aiolos surfaces it and retries on long backoff.

Faults MUST be reported explicitly with `status:error`/`fatal` + `error`. Exiting, returning empty, or silence to indicate a fault is non-conformant.

## Unit / component / signal report schema
`detect` schema surfaces, live `info`/`collect` snapshots, and live `apply` reports use the same
flat model. Detect omits live `value`; live reports include it when known.

```json
{
  "status":"ok",
  "units":[
    {"id":"gpu:<stable-id>","labels":{"type":"gpu","name":"gpu0","description":"GPU"}}
  ],
  "components":[
    {"id":"gpu:<stable-id>:thermal","unit":"gpu:<stable-id>",
     "labels":{"type":"temperature","name":"thermal"}},
    {"id":"gpu:<stable-id>:fan0","unit":"gpu:<stable-id>",
     "labels":{"type":"fan","name":"fan0"}}
  ],
  "signals":[
    {"id":"gpu:<stable-id>:thermal:temp","component":"gpu:<stable-id>:thermal",
     "role":"producer","value":63,"uom":"C",
     "labels":{"type":"temperature","name":"temperature"}},
    {"id":"gpu:<stable-id>:fan0:duty","component":"gpu:<stable-id>:fan0",
     "role":"sink","value":55,"uom":"%","range":[0,100],
     "labels":{"type":"fan-duty","name":"duty"},
     "control":{"needs_claim":true,"state":"claimed","safe":"auto",
       "direction":"up=more-cooling",
       "driven_by":[{"name":"gpu0","value":63,"uom":"C",
                     "signal":"gpu:<stable-id>:thermal:temp"}],
       "driving":{"type":"temperature","raw":63,"input":63,"uom":"C",
                  "output":55,"how":"self→curve"}}}
  ]
}
```

Rules:
- `Unit.id`, `Component.id`, and `Signal.id` are stable, system-derived IDs. Components reference
  their parent through `unit`; signals reference theirs through `component`.
- Every entity has an open `labels` bag. Reserved cross-project labels are `type`, `name`, and
  `description`; `type` is an open semantic tag.
- `Signal.role` is `producer|sink`. Producers are read-only values. Sink-only control semantics live
  in typed `control`, not labels.
- `uom` is unit of measure and is distinct from the hardware `Unit` entity.
- `state` is `released|claimed|diverged|unknown`.
- Every claimed sink must include `control.driving.input` and `.output`; `driven_by` identifies the
  producer signals which contributed to the decision.
- Consumers MUST NOT re-publish foreign devices. Consumed values are represented as sink
  `control.driven_by` metadata.

## detect
```json
→ {"cmd":"detect"}
← {"status":"ok","units":[{"id":"<stable-id>","labels":{"type":"gpu","name":"gpu0"}}],
   "components":[...],"signals":[...]}
← {"status":"error","error":"NVML init failed: …"}
← {"status":"fatal","error":"no /dev/ipmi0 on this host"}
```
- On `ok`, `units` is authoritative. Aiolos spawns one `run <ID>` per `units[].id`; empty means
  genuinely no devices.
- Unit IDs are stable across re-detect and device drop/return (GPU UUID, NVMe serial, `board`, etc.;
  never an unstable index).

## info / collect (read-only one-shot)
```json
$ <module> info [ID]
← {"status":"ok","units":[...],"components":[...],"signals":[...]}
```
- `info` and `collect` are aliases in the Rust SDK. They are companion/diagnostic CLI surfaces, not
  orchestrator heartbeat messages.
- They use `Anemos::open(id, OpenMode::Observe)` and `Device::collect`, never `Device::apply`.
- Observe mode MUST NOT claim/set/release hardware, and MUST NOT arm restore-on-drop side effects.
- With `[ID]`, only that detected ID is reported; an unknown ID returns `fatal`.
- Non-fatal per-device collect failures may be aggregated in the top-level `error` field while any
  successfully collected devices still include their entities.

## apply
```json
→ {"cmd":"apply","inputs":{"nvidia:GPU-...":[
     {"id":"gpu:<stable-id>:thermal:temp","component":"gpu:<stable-id>:thermal",
      "role":"producer","value":63,"uom":"C","labels":{"type":"temperature"}}
   ]}}
← {"status":"ok","units":[...],"components":[...],"signals":[...]}
← {"status":"error","error":"device read failed"}
← {"status":"fatal","error":"GPU unsupported"}
```
- `inputs` is present only when registry wires `input=<module>` (repeat `input=` or comma-list for multiple sources).
- `inputs` maps each source instance's **`module:id`** key to that instance's fresh **signal list**
  from its most recent successful, non-empty `apply`.
- Aiolos relays signals verbatim and uninterpreted. Consumers select the producer/sink signals they
  understand, optionally filtering by source key prefix or exact signal fields/labels.
- A source `error`/`fatal` or authoritative `ok` with an empty signal list immediately removes that
  source from the routing blackboard. The next consumer dispatch therefore sees telemetry loss
  rather than a stale last-good value.
- On `ok`, `units`/`components`/`signals` are authoritative for this instance's current live report.
- The run process knows its own ID from argv; `apply` does not repeat it.
- Invalid startup curve for a control module: the module must not regulate, must answer first `apply` with `fatal` explaining the curve problem, then exit non-zero so aiolos retries on long backoff. Sensor-only modules are exempt.

## shutdown
```json
→ {"cmd":"shutdown"}
← {"status":"ok"}
```
On `shutdown` — identically on stdin EOF and SIGTERM/SIGINT — a `run` process MUST restore its device to firmware/auto-safe, then exit.

## Signal self-restore
A `run` instance MUST catch SIGTERM/SIGINT and restore itself. The signal handler sets a flag only; restore runs in normal code. Shipped modules use `anemos::StdinReader` + `install_shutdown_handlers`.

## Timing, failure, fail-safe
- aiolos waits at most the module's per-module `timeout` for one response. Timeout/partial-line/flood → kill + respawn; this is a backstop only.
- Slow-but-answering applies are delayed, never killed for being slower than `every`.
- A controlled state must be safer/more aggressive than firmware defaults, so module death trends toward safe cooling or a restorable default.

## Conformance checklist
1. stdout emits only valid one-line JSON; logs only on stderr.
2. `detect` returns stable unit IDs and flat unit/component/signal schema.
3. `info`/`collect` returns live signal values through read-only `collect`, without side effects.
4. `apply` returns live `units[]`/`components[]`/`signals[]`; faults are explicit `error`/`fatal`.
5. `apply.inputs` consumes fresh signal lists keyed by `module:id` when wired.
6. No module re-publishes foreign devices; use `sink.control.driven_by` for consumed inputs.
7. `apply` returns within `timeout` or aiolos can kill it without harming siblings.
8. `shutdown`, stdin EOF, and SIGTERM/SIGINT restore safe/auto state.
9. `<module> restore` is idempotent and restores every managed device.
