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

## Messages (each exactly one line)

### hello (optional, module → orchestrator, once at startup)
```json
{"hello":{"proto":1,"name":"nvidia","modes":["detect","run"]}}
```
`proto` is the protocol version (this spec = `1`). A module MAY emit one `hello` line before its
first response; the orchestrator consumes and skips any leading `hello` on both the detect and
run streams, so emitting it is optional and never desyncs the stream. (The shipped `nvidia` and
`asrock16-2t` modules do not emit `hello`.)

### detect (orchestrator → detect process; re-sent each detect cycle)
```json
→ {"cmd":"detect"}
← {"found":[{"id":"<opaque-stable-id>","type":"GPU","name":"…", "...":"..."}]}
```
- `found` is an array (possibly empty). Each entry MUST have `id` (opaque, stable across
  re-detect and across device drop/return) and SHOULD have `type` and a human `name`. Extra
  keys are allowed and surfaced on the status page.

### apply (orchestrator → run process; each heartbeat)
```json
→ {"cmd":"apply","inputs":{"<peer-id>":[{"type":"temp","label":"GPU","temp":63}], "...":[]}}
← {"status":"ok","readings":[{"type":"temp","label":"CPU1","temp":37,"pwm":50,"rpm":900}]}
```
- `inputs` is present only when the registry wires `input=<other-module>`. It maps each peer
  instance's `id` to **that instance's full `readings` array** (the same records the peer last
  reported), one heartbeat stale. The orchestrator relays the readings **verbatim and
  uninterpreted** — it does not pick "the temperature"; the consumer selects what it needs (e.g.
  records with `"type":"temp"`). Absent when no `input=` is wired; the `inputs` key is omitted
  entirely (never serialized as `null`).
- Response: `status` ∈ {`ok`,`error`}. On `ok`, `readings` is an array of records; each record
  has a `type` (`temp`,`fan`,…) and `label`, plus arbitrary numeric/string fields
  (`temp`,`pwm`,`rpm`,…). On `error`, include `error:"<reason>"`; `readings` optional.
- The run process knows its own `id` from argv; `apply` does not repeat it.

### shutdown (orchestrator → run/detect process; graceful stop)
```json
→ {"cmd":"shutdown"}
← {"status":"ok"}
```
On `shutdown` — and **identically on stdin EOF** (covers the orchestrator dying) — a `run`
process MUST restore its device to firmware/auto-safe state, then exit. This is the fail-safe.

## Timing, failure, fail-safe
- The orchestrator waits at most `timeout` (< heartbeat) for a response. No response in time →
  the instance is `SIGKILL`ed and respawned on a later cycle. Modules must never block the
  orchestrator; isolation is by separate processes.
- A module's controlled state must be *more aggressive/safe* than the device default, so
  "module dies → firmware/BMC reclaims control" is always the safe direction.

## Conformance checklist (a module is conformant iff)
1. Emits only valid one-line JSON on stdout; everything else on stderr.
2. `detect` returns stable `id`s; re-detect of an unchanged device returns the same `id`.
3. `apply` returns within `timeout`; reports `readings` or `error`.
4. On `shutdown` OR stdin EOF, restores the device to safe/auto and exits.
5. Tolerates being `SIGKILL`ed at any time without leaving the device in an unsafe state
   (i.e. the safe state is the device's own firmware default, reached automatically on process
   death where the hardware allows; where it does not, see the module spec's fail-safe note).
