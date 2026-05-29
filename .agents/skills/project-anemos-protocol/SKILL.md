---
name: project-anemos-protocol
description: "Mandatory contract when editing the aiolos orchestrator's protocol handling, any anemos's stdin/stdout, or claiming protocol conformance. The wire protocol between aiolos and the anemoi: one-line JSON request/response, stdout=protocol/stderr=logs, detect/apply/shutdown/hello, half-duplex + timeout, fail-safe on EOF."
---
# aiolos ↔ anemos protocol contract

## Purpose
Prevent the two failure classes that break this system: (1) corrupting the stdio stream so
aiolos and a module desync, and (2) leaving a device in an unsafe state when a module stops.
Authoritative spec: `.agents/sow/specs/aiolos-protocol.spec.md`.

## Scope
Use this skill when:
- editing the orchestrator's child I/O, framing, timeout, or routing;
- writing or changing any anemos's stdin reading / stdout writing / shutdown path;
- claiming a module is protocol-conformant.
Do not use for: orchestrator-internal concerns unrelated to module I/O (web page rendering, etc.).

## Mandatory Knowledge
- **One line = one complete JSON object.** Request is one line; response is one line; `\n` is the
  only delimiter. No multi-line messages, no partial writes. Evidence: `aiolos-protocol.spec.md`.
- **stdout is protocol-only.** Every log/diagnostic/warning goes to **stderr**. A single stray
  byte on stdout desyncs the stream — this is the #1 conformance bug.
- **Strict half-duplex.** aiolos writes one request, reads exactly one response line before the
  next. A module never writes to stdout unsolicited (sole exception: one `hello` line at startup).
- **Messages:** `detect → {status,found:[{id,type,name,…}]}`; `apply{inputs?} →
  {status,readings:[…]}`; `shutdown → {status:"ok"}`. `id`s from `detect` MUST be stable across
  re-detect and device drop/return (e.g. GPU UUID, never NVML index).
- **Status taxonomy (every detect/apply reply):** `status` ∈ {`ok`,`error`,`fatal`}.
  `ok` = did the job (`found`/`readings` authoritative; empty is real; an `error` field = non-fatal
  warning). `error` = transient fault, could NOT do the job (NOT "no devices") → supervisor keeps
  instances + retries. `fatal` = cannot work on this host → supervisor surfaces + long-backoff retry.
- **Report faults EXPLICITLY, never by exiting/empty/silence.** A module that crashes, returns an
  empty `found`, or goes quiet to signal a problem forces the supervisor to decide blind — that is a
  bug. Say what's wrong with `status:error`/`fatal` + a reason. (Crash/timeout is only the
  orchestrator's last-resort backstop, surfaced as "unresponsive/crashed".)
- **Init device libraries ONCE per process** (NVML/IPMI open fds); re-initialising every detect
  cycle leaks fds → EMFILE → the module silently stops working.
- **`inputs` shape:** when present, `apply.inputs` maps each peer instance `id` to **that peer's
  full readings array** (e.g. `{"GPU-…":[{"type":"temp","label":"GPU","temp":63}, …]}`). aiolos
  relays them verbatim and uninterpreted — the consumer selects what it needs (typically the
  `"type":"temp"` records). The `inputs` key is omitted entirely when no `input=` is wired (never
  `null`). `Request::Apply.inputs` is `Option`, serialized with `skip_serializing_if`.
- **`hello` is consumed by aiolos:** a module MAY emit one `hello` line at startup; the
  orchestrator skips a leading `hello` on both streams, so it never desyncs. Emitting it is
  optional (the shipped modules don't).
- **Fail-safe (three equivalent triggers):** on `shutdown`, **stdin EOF**, OR **SIGTERM/SIGINT**, a
  `run` instance MUST restore its device to firmware/auto-safe and exit. EOF covers aiolos dying;
  the signal covers a direct kill / system stop. The controlled state must be more aggressive/safe
  than the device default so "module dies → firmware reclaims" is the safe direction.
- **Signal self-restore:** a module is self-sufficient — it catches SIGTERM/SIGINT and restores
  itself; it never relies on the parent killing it. The handler is async-signal-safe (sets a flag
  only); the restore runs in normal code (NVML/IPMI allocate + lock → unsafe in a handler). Read
  stdin non-blocking + `poll(2)` in short steps checking the flag (std blocking reads swallow
  `EINTR`). Use `protocol::StdinReader` + `protocol::install_shutdown_handlers` (they do exactly
  this) rather than hand-rolling.
- **`restore` one-shot:** every module implements `<module> restore` — restore ALL its devices and
  exit, idempotently. Verb is uniform across anemoi so `aiolos restore` (systemd ExecStopPost) can
  call it without naming modules.
- **Timeout:** aiolos kills a module that doesn't answer within `timeout` (< heartbeat). Modules
  must never assume they won't be `SIGKILL`ed mid-operation.

## Best Practices
- Read stdin line-by-line with a generous buffer; parse with serde_json into typed structs.
- Flush stdout after every response line (line-buffered or explicit flush).
- Put the device-restore in a single function called from ALL of: the `shutdown` handler, the EOF
  path, the SIGTERM/SIGINT path, and the `restore` one-shot. Never restore inside the signal handler
  itself (set a flag; restore in normal code).
- Keep `apply` work bounded so it always returns within `timeout`.

## Bad Practices
- `println!`-style debug to stdout (corrupts protocol) — use `eprintln!`/stderr.
- Emitting pretty-printed/multi-line JSON.
- Using a renumbering index (NVML index, sensor number) as the stable `id`.
- Doing slow/blocking work in `apply` without a bound.
- Relying on a graceful path only — assume `SIGKILL` can happen anytime; the safe state must be
  reachable without the module's cooperation wherever the hardware permits.

## Workflow Checklist
1. Re-read `aiolos-protocol.spec.md`.
2. Confirm stdout carries only JSON; route all logs to stderr.
3. Implement/verify detect (stable ids), apply (within timeout), shutdown + EOF + SIGTERM (device
   restore), and the `restore` one-shot. Use `protocol::StdinReader` + `install_shutdown_handlers`.
4. Test with a one-line stdin → one-line stdout harness and with the orchestrator's mock/timeout.

## Validation Checklist
- Golden request→response lines round-trip; malformed input is rejected without crashing.
- A `SIGKILL` mid-apply leaves the device safe (or auto-reclaimed by firmware).
- shutdown, stdin-EOF, AND SIGTERM each restore the device (verified by reading device state after
  exit; the integration suite holds stdin open and SIGTERMs to prove the signal path specifically).
- `<module> restore` returns the device to safe/auto and is idempotent.
- No non-JSON ever appears on stdout (grep the captured stream).

## Evidence
- `.agents/sow/specs/aiolos-protocol.spec.md`: the contract.
- `DESIGN.md`: rationale (isolation, blackboard, lifecycle).

## Update Rules
Update when the protocol version bumps, a new message/field is added, or a conformance bug
reveals an unstated rule.
