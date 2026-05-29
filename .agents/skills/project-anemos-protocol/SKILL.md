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
- **Messages:** `detect → {found:[{id,type,name,…}]}`; `apply{inputs?} → {status,readings:[…]}`
  or `{status:"error",error}`; `shutdown → {status:"ok"}`. `id`s from `detect` MUST be stable
  across re-detect and device drop/return (e.g. GPU UUID, never NVML index).
- **`inputs` shape:** when present, `apply.inputs` maps each peer instance `id` to **that peer's
  full readings array** (e.g. `{"GPU-…":[{"type":"temp","label":"GPU","temp":63}, …]}`). aiolos
  relays them verbatim and uninterpreted — the consumer selects what it needs (typically the
  `"type":"temp"` records). The `inputs` key is omitted entirely when no `input=` is wired (never
  `null`). `Request::Apply.inputs` is `Option`, serialized with `skip_serializing_if`.
- **`hello` is consumed by aiolos:** a module MAY emit one `hello` line at startup; the
  orchestrator skips a leading `hello` on both streams, so it never desyncs. Emitting it is
  optional (the shipped modules don't).
- **Fail-safe:** on `shutdown` OR **stdin EOF**, a `run` instance MUST restore its device to
  firmware/auto-safe and exit. EOF covers aiolos dying. The controlled state must be more
  aggressive/safe than the device default so "module dies → firmware reclaims" is the safe direction.
- **Timeout:** aiolos kills a module that doesn't answer within `timeout` (< heartbeat). Modules
  must never assume they won't be `SIGKILL`ed mid-operation.

## Best Practices
- Read stdin line-by-line with a generous buffer; parse with serde_json into typed structs.
- Flush stdout after every response line (line-buffered or explicit flush).
- Put the device-restore in a single function called from both the `shutdown` handler and the
  EOF path (and, where the OS allows, a fatal-signal handler).
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
3. Implement/verify detect (stable ids), apply (within timeout), shutdown + EOF (device restore).
4. Test with a one-line stdin → one-line stdout harness and with the orchestrator's mock/timeout.

## Validation Checklist
- Golden request→response lines round-trip; malformed input is rejected without crashing.
- A `SIGKILL` mid-apply leaves the device safe (or auto-reclaimed by firmware).
- shutdown and stdin-EOF both restore the device (verified by reading device state after exit).
- No non-JSON ever appears on stdout (grep the captured stream).

## Evidence
- `.agents/sow/specs/aiolos-protocol.spec.md`: the contract.
- `DESIGN.md`: rationale (isolation, blackboard, lifecycle).

## Update Rules
Update when the protocol version bumps, a new message/field is added, or a conformance bug
reveals an unstated rule.
