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
  `<name> detect`, `<name> run <ID>`, `<name> restore` (one-shot; uniform verb so `aiolos restore`
  calls it agnostically). It owns ALL device knowledge; aiolos stays agnostic.

## Reuse the SDK — do NOT re-implement boilerplate (SOW-0003)
Three reuse levels exist so a module carries only its device logic:
1. **Level-1 tech crates** (`tech/ipmi`, `tech/nvml`, `tech/hwmon`, …): the underlying technologies.
   Depend on the ones you need; add a new one for a new technology.
2. **Level-2 `anemos` SDK**: owns the lifecycle (`anemos::run` — CLI dispatch, signals, logging,
   the protocol stdio loops, the restore-on-shutdown/EOF/signal wiring), the signal-aware
   `StdinReader`, the `Controller` (temp→duty: curve + EMA + deadband + 35% floor), and the
   `Anemos`/`Device` traits. **All of this is inherited — never copy it.**
3. **Level-3 (your module)**: implement `Anemos` (detect / open / restore_all) + `Device`
   (apply / restore), and a `main()` of `anemos::run(ModuleInfo { .. }, MyAnemos::new())`.
- A Rust anemos MUST use the SDK (zero boilerplate duplication). A non-Rust anemos speaks the raw
  protocol directly (see `project-anemos-protocol`) — but prefer Rust + the SDK.
- Model on `anemoi/nvidia` (single file) and `anemoi/asrock16-2t` (+ a `board.rs` for its IPMI OEM
  commands). The CLI/signals/curve/EMA/protocol/restore behaviour is changed once, in `anemos`.

## Mandatory Knowledge (the contract — see project-anemos-protocol)
- One line in (request), one line out (response). JSON only on stdout; logs to stderr.
- `detect` → `{"found":[{"id":"<stable>","type":"…","name":"…"}]}` (ids stable across re-detect).
- `apply` (maybe with `inputs` if wired via `input=`) → `{"status":"ok","readings":[{type,label,…}]}`
  or `{"status":"error","error":"…"}`, within `timeout`.
- `shutdown` OR stdin EOF OR **SIGTERM/SIGINT** → **restore the device to its safe/firmware/auto
  state, then exit.** The module is self-sufficient: it catches the signal itself (async-signal-safe
  flag → restore in normal code), never relying on the parent to kill it. In Rust, use
  `anemos::StdinReader` + `anemos::install_shutdown_handlers` (non-blocking stdin + poll that
  wakes on the signal). Also implement the `restore` one-shot.
- The module's controlled state must be more aggressive/safe than the device default.

## Workflow Checklist
1. **Name it** for the thing it controls (a "wind"): `nvidia`, `asrock16-2t`, `nvme`, `powercap`…
2. **Write a spec** at `.agents/sow/specs/anemos-<name>.spec.md` (purpose, detect ids, apply
   readings, IPMI/API/sysfs access, **fail-safe**, config/curve, acceptance criteria). Model it on
   `anemos-nvidia.spec.md` / `anemos-asrock16-2t.spec.md`.
3. **Open a SOW** from `.agents/sow/SOW.template.md` for the work (it's non-trivial).
4. **Implement the device logic only** (Rust): `impl Anemos` (`detect` → `Detected`; `open(id)` →
   `Box<dyn Device>`; `restore_all`) + `impl Device` (`apply(inputs, ctrl)` → `Applied` using
   `ctrl.duty(raw_temp)`; `restore`), and `fn main() -> ! { anemos::run(ModuleInfo { .. }, MyAnemos) }`
   (use `run_with` to add an extra subcommand like asrock's `query`). The SDK supplies the lifecycle,
   signals, logging, curve+EMA, and the restore wiring — you write NONE of that. Bring in the
   level-1 tech crates you need; add a new `tech/<name>` crate for a new technology. (Non-Rust
   module: speak the raw protocol per `project-anemos-protocol`, and restore on
   shutdown/EOF/SIGTERM + a `restore` one-shot yourself.)
5. **Config**: device IDs stable; curves/params in `/opt/aiolos/etc/<name>.*` (e.g. a JSON
   temp→duty curve). No secrets/IPs in committed defaults — operator config or `*.local.md`.
6. **Register** it in `/opt/aiolos/etc/aiolos.conf` (one line; add `input=<other>` if it consumes
   another module's readings — aiolos relays the prior tick's readings into `apply.inputs`).
7. **Test** (see below) before claiming done.

## Minimal skeleton (Rust + the SDK — this is the whole module)
```rust
use anemos::{Anemos, Applied, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo, Reading};

fn main() -> ! {
    anemos::run(
        ModuleInfo { name: "demo", curve_default_path: Some("/opt/aiolos/etc/demo.curve.json"),
                     curve_env_filename: Some("demo.curve.json") }, // None,None = sensor-only
        Demo,
    )
}

struct Demo;
impl Anemos for Demo {
    fn detect(&mut self) -> Detected {
        Detected::ok(vec![FoundEntry { id: "thing0".into(), kind: "DEMO".into(),
                                       name: "demo".into(), extra: Default::default() }])
    }
    fn open(&mut self, id: &str) -> anyhow::Result<Box<dyn Device>> { Ok(Box::new(Dev::open(id)?)) }
    fn restore_all(&mut self) { /* restore every device this module manages */ }
}
impl Device for Dev {
    fn apply(&mut self, _inputs: Option<&Inputs>, ctrl: &mut Controller) -> Applied {
        let temp = self.read_temp();                 // your tech crate
        match ctrl.duty(temp).pct {                  // SDK: curve + EMA + deadband + 35% floor
            Some(p) => { if let Err(e) = self.set(p) { self.restore_dev(); return Applied::error(e.to_string()); } }
            None    => self.set_default(),           // empty curve -> firmware/auto
        }
        Applied::ok(vec![Reading::new("temp", "demo", serde_json::json!({ "temp": temp }))])
    }
    fn restore(&mut self) { /* hand the device back to firmware/auto */ }
}
```
No CLI parsing, no signal handling, no stdin loop, no logging setup, no curve/EMA, no emit — the SDK
owns all of it. The level-1 tech (`read_temp`/`set`/`restore_dev`) lives in a `tech/<name>` crate.

## Sensor-only modules (report, control nothing)
A module that only *reports* a signal (e.g. `nvme` — NVMe temps for routing) and drives no device:
- Set **`curve_default_path: None, curve_env_filename: None`** in `ModuleInfo`. The SDK then skips
  the curve-empty warning, and `apply` ignores the `ctrl` argument (no curve to apply).
- `apply` just returns `Applied::ok(readings)`; `restore` and `restore_all` are **no-ops** (there is
  nothing to hand back to firmware), and the `restore` one-shot exits 0.
- No curve file is shipped/installed. Wire it into a consumer with `input=<name>` so its readings
  reach a fan controller. Isolation still matters: if its read can block (e.g. an NVMe admin
  command on a wedged drive), its own process being killed at the tick deadline protects siblings.

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
