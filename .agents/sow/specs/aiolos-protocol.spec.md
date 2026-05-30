# Spec: aiolos ↔ anemos wire protocol

Status: design (no implementation yet). This is the authoritative contract; `DESIGN.md` holds rationale.

## Transport & framing
- Each anemos is launched as a process. The orchestrator writes **requests to the module's
  stdin** and reads **responses from its stdout**.
- **One line = one complete JSON object.** A request is exactly one line; a response is exactly
  one line. Newline (`\n`) is the only delimiter. No multi-line messages.
- **Strict half-duplex:** the orchestrator sends one request, then reads exactly one response
  line before sending the next. A module never writes to stdout except as a response (the one
  exception: an optional `hello` line at startup).
- **stdout is protocol-only.** All logs, diagnostics, warnings → **stderr**. A single stray byte
  on stdout corrupts the stream and is a conformance bug. The orchestrator captures each module's
  stderr (tail) for the status page.

## Modes (argv)
- `<module> detect` — a persistent process that answers `detect` requests.
- `<module> run <ID>` — a persistent process bound to one detected ID; answers `apply`/`shutdown`.
- `<module> restore` — a **one-shot**: restore EVERY device this module manages to its
  firmware/auto-safe state, then exit. Idempotent (safe when already auto). The verb is **uniform
  across all anemoi** so the orchestrator can invoke it agnostically — `aiolos restore` reads the
  registry and runs `<module> restore` for each configured module (wired to systemd `ExecStopPost`
  as a belt-and-suspenders for a hard kill where modules could not self-restore).

## Messages (each exactly one line)

### hello (optional, module → orchestrator, once at startup)
```json
{"hello":{"proto":1,"name":"nvidia","modes":["detect","run"]}}
```
`proto` is the protocol version (this spec = `1`). A module MAY emit one `hello` line before its
first response; the orchestrator consumes and skips any leading `hello` on both the detect and
run streams, so emitting it is optional and never desyncs the stream. (The shipped `nvidia` and
`asrock16-2t` modules do not emit `hello`.)

### Status model (both detect and apply)
Every `detect`/`apply` response carries `status` ∈ {`ok`,`error`,`fatal`}. Modules MUST report
faults **explicitly** via this status — never by exiting, returning empty, or going silent (the
supervisor must not have to infer faults from absence of data). Crash/timeout detection is a
last-resort backstop only.
- `ok` — the module did its job; `found`/`readings` are authoritative (empty is a real result). An
  accompanying `error` field is a **non-fatal warning** ("done, with errors").
- `error` — transient: it could NOT do its job this time. NOT "no devices". The supervisor keeps
  existing instances, surfaces the reason, and retries with backoff.
- `fatal` — it cannot work on this host (wrong hw, missing capability, or — for a control module —
  an invalid curve at startup). The supervisor surfaces it and retries only on a **long backoff**
  (jumps to the `max_backoff` cap; never permanently abandons — the condition may clear).
`error`/`fatal` responses include `error:"<reason>"`.

### detect (orchestrator → detect process; re-sent each detect cycle)
```json
→ {"cmd":"detect"}
← {"status":"ok","found":[{"id":"<opaque-stable-id>","type":"GPU","name":"…","...":"..."}]}
← {"status":"error","error":"NVML init failed: …"}      // cannot detect right now (keep instances)
← {"status":"fatal","error":"no /dev/ipmi0 on this host"} // cannot work here (long backoff)
```
- On `ok`, `found` is authoritative (possibly empty = genuinely no devices). Each entry MUST have
  `id` (opaque, stable across re-detect and across device drop/return) and SHOULD have `type` and a
  human `name`. Extra keys are allowed and surfaced on the status page.
- A bare `{"found":[...]}` with no `status` is accepted as `ok` (lenient/back-compat).

### apply (orchestrator → run process; on the module's own cadence)
```json
→ {"cmd":"apply","inputs":{"nvidia:<gpu-uuid>":[{"type":"temp","label":"GPU","temp":63}],
                           "nvme:<serial>":[{"type":"temp","label":"Composite","temp":43}]}}
← {"status":"ok","readings":[{"type":"temp","label":"CPU1","temp":37,"pwm":50,"rpm":900}]}
← {"status":"error","error":"device read failed"}        // transient; instance kept, retried
← {"status":"fatal","error":"GPU unsupported"}           // long-backoff respawn
```
- `inputs` is present only when the registry wires `input=<module>` (one or more sources — repeat
  `input=` or use a comma list). It maps each source instance's **`module:id`** key to **that
  instance's full `readings` array** (the source's **most recent completed** readings as of this
  apply's dispatch — producers/consumers run on independent cadences, SOW-0013).
  Keying by `module:id` (not the bare id) lets the consumer attribute each reading to its **source
  module** (e.g. tell `nvidia:*` GPU temps from `nvme:*` disk temps) and guarantees keys never
  collide across sources. The orchestrator relays the readings **verbatim and uninterpreted** — it
  does not pick "the temperature"; the consumer selects what it needs (e.g. records with
  `"type":"temp"`, optionally filtered by the `module:` key prefix). Absent when no `input=` is
  wired; the `inputs` key is omitted entirely (never serialized as `null`).
- On `ok`, `readings` is an array of records; each has a `type` (`temp`,`fan`,…) and `label`, plus
  arbitrary numeric/string fields (`temp`,`pwm`,`rpm`,…).
- The run process knows its own `id` from argv; `apply` does not repeat it.
- **Invalid curve at startup (control modules):** a module that controls a device (it has a curve)
  and cannot load a usable curve when it starts (missing file, invalid JSON, or no usable points)
  MUST NOT regulate. It never opens the device (firmware/auto keeps cooling), answers its first
  `apply` with `{"status":"fatal","error":"startup: curve …"}` so the reason reaches the status
  page, then **exits non-zero**. The supervisor respawns it on the `max_backoff` cap. Sensor-only
  modules (no curve) are exempt. (A curve that breaks *while running* is NOT fatal: the module keeps
  its last-good curve and warns every tick — see the anemos fail-safe sections.)

### shutdown (orchestrator → run/detect process; graceful stop)
```json
→ {"cmd":"shutdown"}
← {"status":"ok"}
```
On `shutdown` — **identically on stdin EOF** (covers the orchestrator dying) **and identically on
SIGTERM/SIGINT** — a `run` process MUST restore its device to firmware/auto-safe state, then exit.
This is the fail-safe, and all three triggers run the same restore.

### Signal self-restore (run mode)
A `run` instance MUST catch `SIGTERM`/`SIGINT` and restore its device itself — it must never depend
on the parent killing it. The handler must be async-signal-safe (set a flag only); the restore runs
in normal code (device libraries like NVML/IPMI allocate and take locks, so restoring inside a
signal handler can deadlock). Because std's blocking line reads swallow `EINTR`, modules read stdin
non-blocking and `poll(2)` in short steps, checking the flag between polls. The shipped modules use
`anemos::StdinReader` + `anemos::install_shutdown_handlers`, which implement exactly this.

## Timing, failure, fail-safe
- The orchestrator waits at most the module's own `timeout` for a response (per-module since
  SOW-0013; it may exceed the anemos's `every` and the scheduler `base_tick`). No response in time →
  the instance is `SIGKILL`ed and respawned (a **backstop** for a module too broken to report). Each
  module runs on its own worker thread/process on its own cadence, so a slow/hung one never blocks
  the orchestrator or a sibling; isolation is by separate processes. A slow-but-answering apply is
  merely delayed (it runs at ≈ `max(every, apply_duration)`), never killed.
- **Report errors, don't infer them.** A module that hits a fault MUST send `status:error`/`fatal`
  with a reason. Exiting, returning empty, or going silent to signal a fault is a conformance bug:
  it forces the supervisor to make critical decisions blind. The supervisor reacts to declared
  status and surfaces the reason; crash/timeout is only the last resort.
- A module's controlled state must be *more aggressive/safe* than the device default, so
  "module dies → firmware/BMC reclaims control" is always the safe direction.

## Conformance checklist (a module is conformant iff)
1. Emits only valid one-line JSON on stdout; everything else on stderr.
2. `detect` returns stable `id`s; re-detect of an unchanged device returns the same `id`.
3. Every `detect`/`apply` reply carries `status` ∈ {`ok`,`error`,`fatal`}; faults are reported
   EXPLICITLY (with a reason), never by exiting/empty/silence. `ok` data is authoritative.
4. `apply` returns within `timeout`.
5. On `shutdown` OR stdin EOF OR `SIGTERM`/`SIGINT`, restores the device to safe/auto and exits
   (signal handler is async-signal-safe; the restore runs in normal code).
6. Implements `<module> restore` as an idempotent one-shot that restores every device it manages.
7. Tolerates being `SIGKILL`ed at any time without leaving the device in an unsafe state
   (i.e. the safe state is the device's own firmware default, reached automatically on process
   death where the hardware allows; where it does not, `aiolos restore` via ExecStopPost is the net).
