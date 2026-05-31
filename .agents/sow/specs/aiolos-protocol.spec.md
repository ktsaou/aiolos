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
  component values, and exits. It MUST NOT claim, set, release, or restore hardware.
- `<module> schema` — one-shot schema-only detect response for humans/tools.
- `<module> restore` — one-shot: restore every device this module manages to firmware/auto-safe, then exit. Idempotent.

## hello (optional, module → orchestrator, once at startup)
```json
{"hello":{"proto":1,"name":"nvidia","modes":["detect","run"]}}
```
`proto` is the protocol version (`1`). aiolos skips a leading `hello` on detect/run streams.

## Status model
Every `detect`, `info`/`collect`, and `apply` response carries `status` ∈ {`ok`,`error`,`fatal`}.
- `ok` — the module did its job; `found`/`components` are authoritative (empty is real). Optional `error` is a non-fatal warning.
- `error` — transient: it could not do the job this time. Not "no devices". aiolos keeps existing instances and retries.
- `fatal` — cannot work for this ID/host now (wrong hardware, missing capability, invalid startup curve). aiolos surfaces it and retries on long backoff.

Faults MUST be reported explicitly with `status:error`/`fatal` + `error`. Exiting, returning empty, or silence to indicate a fault is non-conformant.

## Component report schema
`detect` schema surfaces, live `info`/`collect` snapshots, and live `apply` reports use the same
stable component model:

```json
{
  "id":"gpu",
  "label":"GPU-...",
  "class":"gpu",
  "publishers":[{"id":"temp","label":"Temperature","kind":"temperature","value":63,"unit":"C"}],
  "sinks":[{"id":"fans","label":"GPU fans","kind":"fan-duty","value":55,"unit":"%","range":[0,100],"safe":"auto","needs_claim":true,"state":"claimed","driven_by":[{"from":"self","publisher":"temp","value":63,"unit":"C"}]}]
}
```

Rules:
- `Component.id`, `Publisher.id`, and `Sink.id` are stable local IDs. They are local to their parent unless documented otherwise.
- `class` is an open device kind used for UI grouping/icons (`gpu`, `cpu`, `ssd`, `board`, `power`, `nic`, …).
- Publishers are normalized scalar streams: `{id,label,kind,value?,unit?,range?}`. `value` is absent on schema-only detect surfaces and present on live reports when known.
- Sinks are controllable outputs: `{id,label,kind,range?,unit?,value?,readback?,safe?,needs_claim,state,direction?,driven_by?}`.
- `state` is `released|claimed|diverged|unknown`.
- Consumers MUST NOT re-publish foreign devices. Consumed values are represented as sink `driven_by` metadata.
- Extra keys are allowed on components/publishers/sinks for forward compatibility; aiolos relays them verbatim.

## detect
```json
→ {"cmd":"detect"}
← {"status":"ok","found":[{"id":"<stable-id>","type":"GPU","name":"…","components":[{"id":"gpu","label":"GPU","class":"gpu","publishers":[{"id":"temp","label":"Temperature","kind":"temperature","unit":"C"}]}]}]}
← {"status":"error","error":"NVML init failed: …"}
← {"status":"fatal","error":"no /dev/ipmi0 on this host"}
```
- On `ok`, `found` is authoritative. Empty means genuinely no devices.
- `id` is stable across re-detect and device drop/return (GPU UUID, NVMe serial, `board`, etc.; never an unstable index).
- A bare legacy `{"found":[...]}` with no `status` is accepted as `ok`; new modules must include `status`.

## info / collect (read-only one-shot)
```json
$ <module> info [ID]
← {"status":"ok","found":[{"id":"<stable-id>","type":"GPU","name":"…","components":[{"id":"gpu","label":"GPU","class":"gpu","publishers":[{"id":"temp","label":"Temperature","kind":"temperature","value":63,"unit":"C"}]}]}]}
```
- `info` and `collect` are aliases in the Rust SDK. They are companion/diagnostic CLI surfaces, not
  orchestrator heartbeat messages.
- They use `Anemos::open(id, OpenMode::Observe)` and `Device::collect`, never `Device::apply`.
- Observe mode MUST NOT claim/set/release hardware, and MUST NOT arm restore-on-drop side effects.
- With `[ID]`, only that detected ID is reported; an unknown ID returns `fatal`.
- Non-fatal per-device collect failures may be aggregated in the top-level `error` field while any
  successfully collected devices still include their components.

## apply
```json
→ {"cmd":"apply","inputs":{"nvidia:GPU-...": [{"id":"gpu","label":"GPU","class":"gpu","publishers":[{"id":"temp","label":"Temperature","kind":"temperature","value":63,"unit":"C"}]}]}}
← {"status":"ok","components":[{"id":"board","label":"ROME2D16-2T","class":"board","publishers":[...],"sinks":[...]}]}
← {"status":"error","error":"device read failed"}
← {"status":"fatal","error":"GPU unsupported"}
```
- `inputs` is present only when registry wires `input=<module>` (repeat `input=` or comma-list for multiple sources).
- `inputs` maps each source instance's **`module:id`** key to that instance's full **component list** from the most recent completed `apply`.
- aiolos relays inputs verbatim and uninterpreted. Consumers select the publishers/sinks they understand, optionally filtering by source key prefix.
- On `ok`, `components` is authoritative for this instance's current live report. It replaces the old flat `readings[]` model.
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
2. `detect` returns stable IDs and component schema.
3. `info`/`collect` returns live component values through read-only `collect`, without side effects.
4. `apply` returns live `components[]`; faults are explicit `error`/`fatal`.
5. `apply.inputs` consumes component lists keyed by `module:id` when wired.
6. No module re-publishes foreign devices; use `sink.driven_by` for consumed inputs.
7. `apply` returns within `timeout` or aiolos can kill it without harming siblings.
8. `shutdown`, stdin EOF, and SIGTERM/SIGINT restore safe/auto state.
9. `<module> restore` is idempotent and restores every managed device.
