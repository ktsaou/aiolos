---
name: project-create-anemos
description: "Mandatory guide when creating a new aiolos module (anemos) for any device or signal — in any language. How to implement detect/collect/apply/shutdown over the one-line-JSON stdio protocol, component publishers/sinks, fail-safe restore, registry wiring (input=), curve config, and tests. Use whenever asked to add a module, plugin, sensor reactor, or fan/temperature controller to aiolos."
---
# Create a new anemos (aiolos module)

## Purpose
Add a new module without touching the orchestrator. An anemos is a standalone binary that owns all
hardware/signal knowledge and speaks the protocol; aiolos stays agnostic. Read
`project-anemos-protocol` first.

## Scope
Use when adding/scaffolding a module for a new device or signal (GPU brand, NIC, NVMe, UPS/power
reactor, board fans, sensors, alerting reactor, …). Do not use for orchestrator-core or protocol
changes.

## Reuse the SDK — do not re-implement boilerplate
1. **Level-1 tech crates** (`tech/ipmi`, `tech/nvml`, `tech/hwmon`, …): device/API access.
2. **Level-2 `anemos` SDK**: lifecycle (`detect`/`info`/`collect`/`run`/`restore` argv), one-line
   JSON stdio loops, signal-aware stdin, logging, `Controller` (curve + EMA + deadband), and
   restore-on-shutdown/EOF/SIGTERM wiring.
3. **Level-3 module**: implement `Anemos` + `Device`, plus a thin `main()` calling `anemos::run` or
   `run_with`.

Rust modules MUST use the SDK. Non-Rust modules may speak the raw protocol, but must implement the
same fail-safe rules.

## Mandatory contract
- stdout = protocol-only, one JSON object per line; logs to stderr.
- `detect` → `Detected::ok(vec![FoundEntry { id, kind, name, components, … }])`.
- `info`/`collect` → read-only live values via `Anemos::open(id, OpenMode::Observe)` and
  `Device::collect`; never claim, set, release, or arm restore-on-drop side effects.
- `apply` → `Applied::ok(vec![Component { publishers, sinks, … }])`, or explicit `error`/`fatal`.
- Reports use `components[]`: component `class` for grouping; scalar `publishers[]`; controllable
  `sinks[]` with `safe`, `state`, and `driven_by` when consumed inputs drive the output.
- `input=<peer>` routes the peer's prior completed component list in `apply.inputs`, keyed by
  `module:id`. Consumers select needed publishers (usually `kind:"temperature"` or power-state
  kinds). Do not re-publish foreign devices; use `sink.driven_by`.
- `shutdown`, stdin EOF, and SIGTERM/SIGINT restore safe/firmware/auto state and exit. Also implement
  `<name> restore` as an idempotent one-shot.

## Workflow checklist
1. Name the module for what it does (`nvidia`, `rome2d-fans`, `nvme`, `nvidia-powercap`, …).
2. Open/update a SOW for non-trivial work.
3. Write `.agents/sow/specs/anemos-<name>.spec.md`: purpose, stable IDs, component schema, inputs,
   hardware/API access, fail-safe, config/curves, acceptance criteria.
4. Implement device logic only: stable detect; read-only collect; bounded apply; explicit errors;
   safe restore.
5. Add config under `/opt/aiolos/etc/<name>.*` templates as needed. No secrets/IPs in committed
   defaults.
6. Register in `aiolos.conf`; add `input=<source>` for consumers.
7. Validate with protocol smoke tests, unit tests, and orchestrator integration.

## Minimal Rust skeleton
```rust
use anemos::{
    Anemos, Applied, Component, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo,
    OpenMode, Publisher, Sink, SinkState,
};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "demo",
            curve_default_path: Some("/opt/aiolos/etc/demo.curve.json"),
            curve_env_filename: Some("demo.curve.json"), // None/None = sensor-only
        },
        Demo,
    )
}

struct Demo;
impl Anemos for Demo {
    fn detect(&mut self) -> Detected {
        Detected::ok(vec![FoundEntry {
            id: "thing0".into(),
            kind: "DEMO".into(),
            name: "demo".into(),
            components: vec![Component::new("device", "demo", "board")
                .with_publishers(vec![Publisher::new("temp", "Temperature", "temperature").unit("C")])
                .with_sinks(vec![Sink::new("fan", "Fan", "fan-duty")
                    .unit("%")
                    .range(0.0, 100.0)
                    .safe(json!("auto"))
                    .needs_claim(true)])],
            extra: Default::default(),
        }])
    }
    fn open(&mut self, id: &str, mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        Ok(Box::new(Dev::open(id, mode == OpenMode::Control)?))
    }
    fn restore_all(&mut self) { /* restore every managed device */ }
}

impl Device for Dev {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Applied {
        let temp = self.read_temp();
        Applied::ok(vec![Component::new("device", "demo", "board").with_publishers(vec![
            Publisher::new("temp", "Temperature", "temperature").value(json!(temp)).unit("C"),
        ])])
    }

    fn apply(&mut self, _inputs: Option<&Inputs>, ctrl: &mut Controller) -> Applied {
        let temp = self.read_temp();
        let duty = match ctrl.duty(temp).pct {
            Some(p) => p,
            None => { self.restore_dev(); return Applied::error("no usable curve"); }
        };
        if let Err(e) = self.set(duty) {
            self.restore_dev();
            return Applied::error(e.to_string());
        }
        Applied::ok(vec![Component::new("device", "demo", "board")
            .with_publishers(vec![
                Publisher::new("temp", "Temperature", "temperature").value(json!(temp)).unit("C"),
                Publisher::new("fan.duty", "Fan duty", "fan-duty").value(json!(duty)).unit("%"),
            ])
            .with_sinks(vec![Sink::new("fan", "Fan", "fan-duty")
                .value(json!(duty)).unit("%").safe(json!("auto"))
                .needs_claim(true).state(SinkState::Claimed)
                .readback("fan.duty")])])
    }
    fn restore(&mut self) { self.restore_dev(); }
}
```

## Sensor-only modules
- Set `curve_default_path: None, curve_env_filename: None`.
- Implement `collect`; the default `Device::apply` calls `collect`, so sensor-only modules usually
  do not need a custom `apply`.
- `restore`/`restore_all` are no-ops; still implement the uniform `restore` mode.
- No curve file is shipped. Wire into consumers with `input=<name>`.

## Curve loading
For control modules with a curve path, the SDK handles startup/runtime curve errors:
- invalid startup curve → device is never opened; first `apply` returns `fatal`; process exits
  non-zero; aiolos retries on `max_backoff`.
- runtime curve break → last-good curve remains active and a warning is logged.
Sensor-only modules are exempt.

## Bad practices
- Any stdout debug/log line.
- Unstable IDs (indices/sensor numbers when UUID/serial/bus-id exists).
- Re-publishing a routed peer's device instead of using `driven_by`.
- Manual/override device state without a guaranteed restore.
- Unbounded apply work.

## Validation checklist
- `printf '{"cmd":"detect"}\n' | <name> detect` → one valid `found` line with component schema.
- `<name> info [id]` → one valid `found` line with live values and no hardware side effects.
- `printf '{"cmd":"apply"}\n' | <name> run <id>` → one valid `components` line within timeout.
- EOF, SIGTERM (stdin held open), and `shutdown` each restore device state.
- `<name> restore` returns safe/auto and is idempotent.
- SIGKILL mid-run leaves the device safe where hardware allows, or `aiolos restore` recovers it.
- Run under the orchestrator; confirm it does not stall siblings.
- Specs/registry/docs updated; no secrets in committed config.

## Evidence
- `project-anemos-protocol` and `.agents/sow/specs/aiolos-protocol.spec.md`.
- `anemos-nvidia.spec.md`, `anemos-rome2d-fans.spec.md`, and existing modules.
- `DESIGN.md` for isolation and routing rationale.

## Update rules
Update when module conventions, component kinds, config layout, fail-safe patterns, or recommended
language bindings change.
