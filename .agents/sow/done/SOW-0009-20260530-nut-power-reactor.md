# SOW-0009 - NUT / power-event reactor (+ GPU power-cap action)

## Status

Status: completed

Sub-state: completed 2026-05-30. `nut` power-state sensor + `nvidia-powercap` reactor implemented
(commit a7640e5; renamed gpu-powercap→nvidia-powercap in d2fc89b) and merged via Wave 2a integration
(6-round external review). `nut` is live, reporting the `pr3000-nova` UPS. Caveat: the GPU power-cap
action is **operator-disabled** (the host runs a deliberate 400 W cap; nvidia-powercap would restore
the 600 W firmware default on AC), so the cap/restore was NOT exercised on hardware. SOW-0013 shipped;
SOW-0014 (typed inputs) remains open and is not required for this SOW's correctness.

## Requirements

### Purpose
Make aiolos power-aware: read UPS / AC state, surface it, and on utility-power loss take a declared
action — e.g. cap GPU power via NVML to extend battery runtime. Showcases aiolos's domain-agnostic
"any device/signal" model (DESIGN §15 names a power-cap / alert anemos as the example).

### User Request
> Idea #4: the box has a 3 kVA UPS; aiolos is explicitly domain-agnostic — a power reactor is the
> showcase non-fan module.

### Assistant Understanding
Facts (inspection 2026-05-30):
- UPS via NUT (`upsc pr3000-nova`): `ups.status=OL`, `battery.charge=100`, `battery.runtime=5241 s`,
  `ups.load=9 %`, `input.voltage=224 V`. AC loss → `ups.status` changes (e.g. `OB`); the BMC also has
  an `STS_PSU1_AC_LOST` sensor.
- GPUs: `power.draw` 17–20 W idle, limit 400 W, **max 600 W**, throttle-reason bits — all
  readable/settable via NVML. Capping power under battery meaningfully extends runtime.

### Acceptance Criteria
- A module reads UPS state (and/or BMC AC-loss) and surfaces it (status page / readings).
- On a declared trigger (on-battery, or runtime < threshold) it performs the declared action
  (e.g. NVML power cap), and reverts on AC restore — with a fail-safe (action failure never harms).
- Verified against a real or simulated AC-loss event.

## Analysis
Sources: NUT (`upsc`/`upsd` socket), BMC SDR (`STS_PSU1_AC_LOST`), NVML (power limit get/set),
`~/CLAUDE.md` (UPS model/creds live in operator config, NOT artifacts).

This needs both a new **signal source** (NUT) and a new **controlled capability** (NVML power cap) —
the first aiolos module that controls something other than fans.

## Pre-Implementation Gate
Status: implemented against current master (SOW left in pending/ pending on-hardware validation +
the SOW-0013/0014 dependencies; see Execution Log). Decisions below were taken during this build.

Decisions (recorded 2026-05-30):
1. **Shape — split** (chosen): a `nut` *sensor* anemos (reports UPS state) + a `gpu-powercap`
   *control* anemos that reacts via routing (`input=nut`). Fits the domain-agnostic model; the
   sensor is reusable by any future reactor (alerting, etc.). Rejected: one combined reactor (couples
   the signal source to one action).
2. **Signal source — NUT via `upsc`** (chosen): shell out to the system `upsc` client (NUT's
   canonical read-only tool), parse its `key: value` stdout. Rejected: re-implementing the upsd TCP
   protocol (risk, no benefit for a read-only sensor) and the BMC `STS_PSU1_AC_LOST` sensor (the UPS
   gives richer state — charge/runtime/load — and `upsc` needs no creds). `upsc` notices go to
   stderr, so stdout parsing is clean. Binary overridable via `$AIOLOS_UPSC_BIN` for testing.
3. **Action policy — conservative, monitor-by-default** (chosen): only ever caps while ON BATTERY;
   on AC it always lifts. Default = monitor + log on a healthy battery; cap to `cap_pct`% (default
   70%) of the firmware default only when the battery is genuinely draining (`runtime ≤
   runtime_floor_s`, default 300 s) or the UPS raises `LB`. `cap_on_battery=true` opts into capping
   on any on-battery state. All thresholds in `gpu-powercap.conf`, reloaded live. This never throttles
   a running job on a brief outage, but buys runtime when it matters.
4. **Fail-safe — restore the recorded firmware default** (chosen): the original default limit is
   recorded at `open`; restored on shutdown/EOF/SIGTERM, the `restore` one-shot, on a failed control
   tick, and on `Drop` (panic backstop). Mirrors `nvidia`'s fan restore. NVML power limits persist
   after exit, so restore is mandatory; capped < default means "module dies → firmware reclaims" is
   the safe direction (more power, never less). The `nvml` tech crate gained
   power-limit get/set/restore (mW) + a `without_fan_restore_on_drop()` opt-out so the power module
   never issues a fan command.
5. **Sensitive data — operator config only** (chosen): the UPS host/id lives in `nut.conf`
   (`name` or `name@host[:port]`), never committed; `upsc` reads public variables (no login).
   `gpu-powercap.conf` holds thresholds only. Both shipped as fully-commented templates (installing
   them changes nothing; auto-discovery / built-in defaults apply until edited).

### Affected contracts
- Protocol: NO change. Two new reading **types** (`power-state`, `powercap`) ride the existing
  open `type` enumeration; aiolos relays them verbatim (documented in `aiolos-protocol.spec.md`).
- New specs: `anemos-nut.spec.md`, `anemos-gpu-powercap.spec.md`.

### Patterns reused
- `anemos` SDK lifecycle (`run`/`Anemos`/`Device`), sensor-only pattern (`nvme`/`ipmi-temps`),
  control + restore-on-exit (`nvidia`), input-routing extraction (`asrock16-2t`),
  config-next-to-curve env convention (`$AIOLOS_ETC_DIR`), the level-1-tech / level-3-module split.

### Risk / blast radius
- `tech/nvml`: added power methods + a `restore_fans_on_drop` opt-out field (default true) — the
  `nvidia` module is unchanged (default preserved). No GPU touched at build time; on-hardware power
  set/restore is operator-validated (production GPUs).
- `nvfd` still owns GPU fans in production; this module only touches the power LIMIT, never fans, and
  is inert until wired + cut over.

## Plan
1. `nut` sensor anemos (UPS state) and/or BMC AC-loss reader.
2. `gpu-powercap` control anemos (NVML power limit set/restore) reacting to routed power state.
3. Surface power state; validate on a simulated AC-loss.

## Execution Log
### 2026-05-30
- Created (open) from the server-inspection idea list. No code.
- Implemented both modules against current master (one-tick-stale routing; no typed-input
  declarations — that is SOW-0014's job, applied later). Files:
  - `tech/nut/` (new level-1): `upsc -l`/`upsc <id>` client; pure parse of `key: value` →
    typed `UpsState` (status flags, charge, runtime, load, voltage, model + raw vars). 5 unit tests.
  - `anemoi/nut/` (new level-3, sensor-only, curve=None): `power-state` reading per UPS;
    `config.rs` loads `nut.conf` (else `upsc -l` discovery). main + config unit tests.
  - `tech/nvml/`: added `PowerLimits`, `power_limits()`, `set_power_limit()` (clamps to device
    `[min,max]`), `restore_power()`, `power_usage()`, `restore_all_power()`, `clamp_power_limit()`
    (pure, tested), and `without_fan_restore_on_drop()` opt-out (default behaviour unchanged). 2 new
    tests; `nvidia` untouched.
  - `anemoi/gpu-powercap/` (new level-3, curve-less CONTROL, curve=None): `policy.rs` (config +
    pure `decide()`, 11 tests), `inputs.rs` (fold routed `power-state` → worst-case signal, 5 tests),
    `main.rs` (open records default; apply caps/lifts; restore + Drop fail-safe).
  - Workspace + packaging: added `tech/nut`, `anemoi/nut`, `anemoi/gpu-powercap` to `Cargo.toml`;
    `nut`+`gpu-powercap` to `install.sh`/`update.sh` binary lists; module lines in `aiolos.conf`
    (`nut`, `gpu-powercap input=nut`); commented `nut.conf` + `gpu-powercap.conf` templates installed
    if-absent.
  - Specs: `anemos-nut.spec.md`, `anemos-gpu-powercap.spec.md`; protocol spec's open `type` list
    annotated with the two new reading types (no contract change).
- Build/lint clean: `cargo build --release`, `cargo clippy --all-targets`, `cargo fmt --all --check`,
  `cargo test --workspace --no-run` all pass. Binaries (per safety rules) NOT executed; on-hardware
  power set/restore + a simulated AC-loss event remain operator-validated.

### 2026-05-30 — renamed at wave2a integration
- The control module was renamed **`gpu-powercap` → `nvidia-powercap`** (binary, crate, dir
  `anemoi/nvidia-powercap/`, config `nvidia-powercap.conf`, registry line, install/update lists, spec
  `anemos-nvidia-powercap.spec.md`) during the Wave 2a integration merge, per the user. This SOW's
  narrative above keeps the original as-built name for historical fidelity; all live artifacts and the
  spec now use `nvidia-powercap`. The forthcoming temperature trigger is tracked in SOW-0015.

## Validation
Off-hardware (this build):
- Acceptance (partial): the pure decision/parse/fold logic is unit-tested — UPS status parsing &
  flag booleans (incl. `OB` over `OL`), `power-state` reading shape, the cap/lift trigger (AC always
  lifts; low-runtime/LB cap; `cap_on_battery` opt-in; runtime-floor-0 disables), the cross-UPS
  worst-case fold, and the device-min power clamp.
- Tests/equivalent: 23 new unit tests across the four new/changed crates; `--no-run` confirms the
  whole workspace (incl. existing suites) still compiles.
- Reviewer findings: none run yet (no external reviewers per this task's instructions).
- Same-failure search: reused the established sensor-only and control+restore patterns verbatim, so
  no new failure class introduced; the `nvml` change is additive (default preserved for `nvidia`).
- Sensitive-data gate: PASS — no UPS host/creds/IPs in any committed artifact; host lives only in
  operator `nut.conf` (shipped commented).
- Artifact gate: specs added (2) + protocol annotation; packaging + workspace updated.

On-hardware (2026-05-30 cutover): `nut` reads the live `pr3000-nova` UPS — `status=ok`, the
`power-state` reading shows `online`, `on_battery:false`, `charge:100`, `runtime_s≈5241`,
`input_voltage≈229 V`, surfaced as the `aiolos_power_*` metrics. The `nvidia-powercap` **cap action
was not exercised**: the operator keeps the GPUs at a 400 W cap and nvidia-powercap is commented out
in the registry (it would restore 600 W on AC) — see the `aiolos.conf` note. The cap/lift decision
logic is covered by 23 unit tests; a real on-battery cap event remains an optional operator test if
the module is ever enabled with a configurable baseline (cross-refs SOW-0015).

## Outcome
**Completed 2026-05-30.** The `nut` power-state sensor ships and runs live (UPS state + `aiolos_power_*`
metrics). The `nvidia-powercap` reactor (NVML cap/restore, every-exit-path + `Drop` fail-safe, UPS
on-battery trigger, 23 unit tests) ships and is review-converged, but is **operator-disabled** by the
400 W-cap decision, so its cap action has not fired on hardware. Re-enabling it safely needs a
configurable baseline (so it can cap *below* 400 W during a UPS event without restoring 600 W) — that,
plus the thermal trigger, is SOW-0015. Moved to `done/`.

## Lessons Extracted
- A power-cap reactor must adopt a pre-existing operator cap, not assume the firmware default is the
  baseline: on this host the GPUs run a deliberate 400 W cap, so "restore to default (600 W)" is the
  wrong fail-safe — the reason the module ships disabled until it gains a configurable baseline.
- Restore-on-every-exit-path (and `Drop`) via direct panic-safe `stderr` writes is the load-bearing
  guarantee for an actuator that changes a hardware power limit.

## Followup
None yet.

## Regression Log
None yet.
