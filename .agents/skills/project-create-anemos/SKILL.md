---
name: project-create-anemos
description: "Mandatory guide when creating a new aiolos module (anemos) for any device or signal — in any language. How to implement detect/apply/shutdown over the one-line-JSON stdio protocol, the device fail-safe, registry wiring (input=), curve config, and the test checklist. Use whenever asked to add a module, plugin, sensor reactor, or fan/temperature controller to aiolos."
---
# Create a new anemos (aiolos module)

## Purpose
Let any assistant (or contributor) add a new module to aiolos correctly and safely, in any
language, without touching the orchestrator. An anemos is a standalone binary that speaks the
protocol; aiolos stays agnostic. Read `project-anemos-protocol` first — this skill builds on it.

## Scope
Use when: adding/scaffolding a module for a new device or signal (a GPU brand, a NIC, NVMe,
power capping, an alerting reactor, a different board's fans, …).
Do not use for: changing the orchestrator core, or the protocol itself (that's a spec change).

## What a module IS
- A single executable installed at `/opt/aiolos/bin/<name>`, launched by aiolos in three modes:
  - `<name> detect` — long-running; answers `{"cmd":"detect"}` with the IDs it manages.
  - `<name> run <ID>` — long-running, bound to one ID; answers `apply`/`shutdown`.
  - `<name> restore` — one-shot; restore ALL devices it manages to safe/auto, then exit
    (idempotent). Verb is uniform across anemoi so `aiolos restore` can call it agnostically.
- It owns ALL device knowledge. aiolos only spawns it, ticks it, routes data, and renders status.

## Mandatory Knowledge (the contract — see project-anemos-protocol)
- One line in (request), one line out (response). JSON only on stdout; logs to stderr.
- `detect` → `{"found":[{"id":"<stable>","type":"…","name":"…"}]}` (ids stable across re-detect).
- `apply` (maybe with `inputs` if wired via `input=`) → `{"status":"ok","readings":[{type,label,…}]}`
  or `{"status":"error","error":"…"}`, within `timeout`.
- `shutdown` OR stdin EOF OR **SIGTERM/SIGINT** → **restore the device to its safe/firmware/auto
  state, then exit.** The module is self-sufficient: it catches the signal itself (async-signal-safe
  flag → restore in normal code), never relying on the parent to kill it. In Rust, use
  `protocol::StdinReader` + `protocol::install_shutdown_handlers` (non-blocking stdin + poll that
  wakes on the signal). Also implement the `restore` one-shot.
- The module's controlled state must be more aggressive/safe than the device default.

## Workflow Checklist
1. **Name it** for the thing it controls (a "wind"): `nvidia`, `asrock16-2t`, `nvme`, `powercap`…
2. **Write a spec** at `.agents/sow/specs/anemos-<name>.spec.md` (purpose, detect ids, apply
   readings, IPMI/API/sysfs access, **fail-safe**, config/curve, acceptance criteria). Model it on
   `anemos-nvidia.spec.md` / `anemos-asrock16-2t.spec.md`.
3. **Open a SOW** from `.agents/sow/SOW.template.md` for the work (it's non-trivial).
4. **Implement the three modes** over the protocol. Put device-restore in ONE function called from
   the shutdown handler, the EOF path, the SIGTERM/SIGINT path, and the `restore` one-shot. The
   signal handler only sets a flag; the restore runs in normal code (device libs aren't
   async-signal-safe). In Rust, `protocol::StdinReader::next_event` returns `Line`/`Shutdown`/`Eof`.
5. **Config**: device IDs stable; curves/params in `/opt/aiolos/etc/<name>.*` (e.g. a JSON
   temp→duty curve). No secrets/IPs in committed defaults — operator config or `*.local.md`.
6. **Register** it in `/opt/aiolos/etc/aiolos.conf` (one line; add `input=<other>` if it consumes
   another module's readings — aiolos relays the prior tick's readings into `apply.inputs`).
7. **Test** (see below) before claiming done.

## Minimal skeleton (language-agnostic — shell shown for clarity)
```sh
#!/bin/sh
# logs MUST go to stderr; stdout is protocol-only
mode="$1"; id="$2"
emit() { printf '%s\n' "$1"; }          # one JSON line to stdout
case "$mode" in
  detect)
    while IFS= read -r line; do
      case "$line" in
        *'"detect"'*) emit '{"found":[{"id":"thing0","type":"DEMO","name":"demo"}]}';;
        *'"shutdown"'*) emit '{"status":"ok"}'; exit 0;;
      esac
    done; exit 0 ;;                       # stdin EOF -> nothing to restore in detect
  run)
    trap 'restore; exit 0' TERM INT       # fatal-signal restore where possible
    while IFS= read -r line; do
      case "$line" in
        *'"apply"'*) act_on "$id";        # read device, apply curve, set output
                     emit '{"status":"ok","readings":[{"type":"temp","label":"'"$id"'","temp":42,"pwm":50}]}';;
        *'"shutdown"'*) restore; emit '{"status":"ok"}'; exit 0;;
      esac
    done
    restore; exit 0 ;;                     # stdin EOF (aiolos died) -> RESTORE then exit
  restore) restore; exit 0 ;;              # one-shot fail-safe (called by `aiolos restore`)
esac
```
(Rust modules: parse with serde_json into typed structs; same control flow. NVML →
`nvml-wrapper`; IPMI → raw `/dev/ipmi0` ioctl or libfreeipmi FFI.)

## Bad Practices
- Writing logs/debug to stdout (corrupts the protocol).
- Unstable ids (renumbering index/sensor number) — use UUID/serial/bus-id.
- No restore on EOF — if aiolos dies, the device is stranded in the module's last state.
- Setting a device "manual/override" without a guaranteed path back to firmware/auto.
- Unbounded work in `apply` (causes timeout-kill and flapping).

## Validation Checklist
- `printf '{"cmd":"detect"}\n' | <name> detect` → one valid `found` line.
- `printf '{"cmd":"apply"}\n' | <name> run <id>` → one valid `readings` line within timeout.
- Closing stdin (EOF), sending SIGTERM (with stdin held open), and `shutdown` each restore the
  device (verify by reading device state after exit).
- `<name> restore` returns the device to safe/auto and is idempotent.
- `SIGKILL` mid-run leaves the device safe (firmware reclaims where hardware allows).
- Run under the orchestrator with the mock-timeout test: confirm it doesn't stall siblings.
- Spec + registry updated; no secrets in committed config.

## Evidence
- `project-anemos-protocol` skill + `.agents/sow/specs/aiolos-protocol.spec.md`: the contract.
- `anemos-nvidia.spec.md`, `anemos-asrock16-2t.spec.md`: worked examples.
- `DESIGN.md`: why modules are isolated processes and how `input=` routing works.

## Update Rules
Update when the module conventions change (new config layout, new readings types, a new
fail-safe pattern, or a new language binding becomes the recommended one).
