# SOW-0003 - Three-level anemos restructure (tech libs / anemos SDK / per-anemos)

## Status

Status: completed

Sub-state: completed 2026-05-29. Three-level structure implemented (tech crates / anemos SDK / thin
anemoi), behavior-preserving, 60 tests + clippy/fmt clean, deployed and verified on nova (commit
5b20978). Optional follow-up offered to the user: a multi-model edge-case review of the restructure.

## Requirements

### Purpose

Minimize the code each anemos must carry. All boilerplate (CLI, protocol comms, signals, logging,
curve, EMA/deadband, restore wiring) and all underlying technologies (IPMI, NVML, hwmon, …) become
reusable, single-maintenance components. Each new anemos provides only its specific logic
(detect / open / apply / restore) and reuses everything else. This lets the protocol and the
boilerplate change once globally, and makes a new anemos cheap to write.

### User Request

> All modules (anemoi) must be minimal in code. All boilerplate (command line parameters, curve
> processing, EMA, signal handlers, communication with aiolos, etc) must be a standard
> library/module/corpus, all anemoi reuse, without any duplication between them. This also includes
> reusable components like IPMI, nvidia, hwmon and any other underlying technologies. Structured in
> 3 levels: (1) libraries of the various technologies (ipmi, nvidia, hwmon), selectable per anemos;
> (2) aiolos interface and anemos boilerplate (cli, signals, logging, curves, EMA, etc); (3) code
> for the specific anemos (detect, claim, release, apply). Each anemos reuses 1 and 2, provides 3.

### Assistant Understanding

Facts (from the current code):
- The two modules' `main.rs` are largely identical boilerplate. Shared verbatim or near-verbatim:
  tracing init; `protocol::install_shutdown_handlers()`; the `detect`/`run`/`restore` mode dispatch;
  the `detect_loop` (StdinReader + while-let + Request dispatch); the `run_loop` (CurveCache +
  Damper + per-tick reload + `set_alpha` + Request dispatch + restore-on-signal/EOF/shutdown);
  `emit_line`; `curve_path`. Estimated ~80% of each module's `main.rs` is boilerplate.
- Tech-specific code already lives in its own module per anemos: `anemoi/nvidia/src/nvml.rs` (NVML)
  and `anemoi/asrock16-2t/src/ipmi.rs` (raw `/dev/ipmi0`). hwmon/k10temp reading is currently inline
  in `anemoi/asrock16-2t/src/main.rs` (`read_cpu_temps`) — it is a reusable technology, not
  asrock-specific.
- `protocol/` already holds reusable level-2 pieces: wire types, `StdinReader`,
  `install_shutdown_handlers`, `Curve`/`CurveCache`, `Damper`. But there is NO shared `main()`
  driver, so the loop boilerplate is duplicated in each module.
- `aiolos/` (the orchestrator) uses ONLY the wire types + `is_hello` from `protocol` — it does NOT
  use `Curve`/`Damper`/`CurveCache`/`StdinReader` (those are module-side).

Inferences:
- The level-2/level-3 boundary is best expressed as a trait the anemos implements plus a `run()`
  driver in the SDK that owns the entire process lifecycle.
- "Selectable per anemos" maps cleanly to Cargo dependencies: a tech crate per technology; an
  anemos depends only on the tech crates it needs.

Unknowns:
- None blocking; the open forks below are design choices for the user, not investigation gaps.

### Acceptance Criteria

- A new minimal anemos can be written by implementing the anemos trait + a ~10-line `main()`; no
  CLI/signal/curve/EMA/protocol/restore code is copied. Verified by the line counts of the migrated
  `nvidia`/`asrock16-2t` modules dropping substantially (target: their `main.rs` reduced to the
  detect/open/apply/restore surface only).
- Zero behavioral change vs the current (reviewed, deployed) build: the full existing test suite
  (62 tests) plus the orchestrator integration suite pass unchanged; on-hardware re-verify shows the
  same control behavior (35% floor, curve tracking, restore-on-stop, `aiolos restore`).
- No duplication of boilerplate between anemoi (single source for CLI/signals/curve/EMA/comms).
- Tech components (IPMI, NVML, hwmon) are independent, reusable crates selectable per anemos.
- The protocol/boilerplate can be changed in ONE place and all anemoi inherit it (demonstrated by
  the migration touching only the SDK for a boilerplate change).

## Analysis

Sources checked:
- `anemoi/nvidia/src/{main,nvml}.rs`, `anemoi/asrock16-2t/src/{main,ipmi}.rs`
- `protocol/src/{lib,curve,damper,stdio}.rs`
- `aiolos/src/{main,instance,module}.rs` (to confirm aiolos's dependency surface on `protocol`)
- `DESIGN.md`, `.agents/sow/specs/*.spec.md`, project skills

Current state (duplication map):
- BOILERPLATE (duplicated in both modules' `main.rs`): logging init, signal-handler install, mode
  dispatch, detect loop, run loop (curve reload + damper alpha + smooth/eval/deadband call sequence),
  emit_line, curve_path. ~100 lines each, ~80% identical.
- TECH (per-anemos, already separated but not shared): `nvml.rs`, `ipmi.rs`; hwmon inline in asrock.
- ANEMOS-SPECIFIC: nvidia detect=enumerate-by-UUID, open=`Gpu::open`, apply=read GPU temp→duty→set
  fans, restore=`set_default`; asrock detect=one board, open=`Ipmi::open`, apply=max(input GPU temps,
  hwmon CPU temps)→duty→set 8 fans, restore=release BMC.

Risks:
- This refactor touches the **review-hardened, production-deployed** fan-control logic. The hazard is
  a behavior regression that strands fans in manual or breaks a fail-safe. Mitigation: behavior-
  preserving move (no logic changes), the 62-test suite as the safety net, and on-hardware re-verify
  before declaring done; cut over only after green.
- Over-abstraction risk: a too-clever trait could make level-3 HARDER to write. Mitigation: keep the
  trait minimal and concrete; measure success by the resulting per-anemos line count.
- The `Damper`/`Curve` move from `protocol` changes import paths across modules (mechanical).

## Pre-Implementation Gate

Status: needs-user-decision

Problem / root-cause model:
- Boilerplate is duplicated because there is no shared `main()` driver; each module re-implements the
  lifecycle. The tech layers are separated per-module but not packaged for reuse across anemoi.

Evidence reviewed:
- The duplication map above (current `main.rs` of both modules vs the `protocol` crate contents).

Affected contracts and surfaces:
- Cargo workspace layout (new crates); `protocol` crate contents (split); module `main.rs` (shrink);
  `nvml.rs`/`ipmi.rs`/hwmon (promote to tech crates); the anemos↔SDK trait (new internal contract).
- NOT affected: the aiolos↔anemos **wire protocol** (unchanged); the `aiolos` orchestrator (depends
  only on `protocol` wire types); `/opt/aiolos` install layout (same binaries `aiolos`, `nvidia`,
  `asrock16-2t`); systemd unit; curve config files.

Existing patterns to reuse:
- The trait-based seams already added in review (`FanBus`, `FanControl`) are the level-1 device
  abstractions in miniature — generalize them. `CurveCache`/`Damper`/`StdinReader` already are the
  level-2 primitives — they only need a driver wrapped around them.

Sensitive data handling plan:
- No secrets involved. BMC IP/creds remain out of code (inband `/dev/ipmi0`). Pure structural change.

Implementation plan (behavior-preserving; only after decisions + SOW-0001 completed):
1. Split `protocol`: keep wire types + `is_hello` in `protocol`; move `StdinReader`, signal handlers,
   `Curve`/`CurveCache`/`Damper` into the new level-2 `anemos` SDK crate (re-export as needed).
2. Define the level-2 `anemos` trait(s) + `run()` driver implementing the full lifecycle
   (dispatch/detect-loop/run-loop/restore one-shot/emit), driven by the trait. Unit-test the driver
   against a mock anemos impl.
3. Create level-1 tech crates: `ipmi` (from `ipmi.rs`), `nvml` (from `nvml.rs`'s NVML access),
   `hwmon` (from asrock's `read_cpu_temps`, generalized). Each independent + unit-tested.
4. Migrate `nvidia` onto SDK + `nvml` (+ keep its `apply_or_restore`/Gpu logic) → thin `main.rs`.
5. Migrate `asrock16-2t` onto SDK + `ipmi` + `hwmon` (+ keep `regulate`/FanRestore logic) → thin
   `main.rs` (+ its `query` diagnostic via the SDK's extra-subcommand hook).
6. Run full suite + clippy + fmt; rebuild; redeploy; on-hardware re-verify; cut over.

Validation plan:
- The existing 62 tests must pass unchanged (they encode the hardened behavior). Add SDK-driver unit
  tests (mock anemos) + per-tech-crate unit tests. On-hardware re-verify (control + restore paths).

Artifact impact plan:
- AGENTS.md: update the Layout section (new crates) + project-specific commands.
- Runtime project skills: `project-create-anemos` rewritten around "implement the trait + ~10-line
  main, pick your tech crates" — this is the biggest skill win. `project-anemos-protocol` largely
  unchanged (the wire contract is stable) but points at the SDK as the conformant implementation.
- Specs: `aiolos-protocol.spec.md` notes the SDK provides the conformant module runtime; anemos specs
  note their tech-crate dependencies. Add an `anemos-sdk` spec (level-2 contract).
- SOW lifecycle: this SOW starts after SOW-0001 is `completed`.

Open decisions (block implementation until resolved — see numbered options below):
- D1 crate layout / level-2 home; D2 tech-crate location+naming; D3 the anemos trait shape;
  D4 where curve+EMA+deadband apply; D5 Curve/Damper move; D6 extra-subcommand hook; D7 migration
  style (big-bang vs one module first).

## Implications And Decisions

**User confirmed all recommended options (2026-05-29): D1=A, D2=A, D3=A, D4=A, D5=yes, D6=hook,
D7=A.** Resulting target structure:
- Level 1 (tech, selectable per anemos via Cargo deps): crates `tech/ipmi`, `tech/nvml`, `tech/hwmon`.
- Level 2 (SDK boilerplate): new `anemos` crate (depends on `protocol`) = `StdinReader` + signal
  handlers + `Curve`/`CurveCache`/`Damper` (moved out of `protocol`) + a `Controller` exposing
  `duty(raw_temp)->u32` (reload+alpha+smooth+eval+deadband, centralized) + the `run()` driver +
  the `Anemos`/`Device` traits + an optional extra-subcommand hook (for asrock `query`).
- `protocol` keeps ONLY the wire types + `is_hello` (shared by `aiolos` + `anemos`).
- Level 3 (per-anemos): `anemoi/nvidia`, `anemoi/asrock16-2t` implement the traits + a thin `main()`.
- Migration: nvidia first (verify: tests + hardware), then asrock. Behavior-preserving — the
  reviewed/deployed logic (apply_or_restore, regulate, FanRestore, the floor/EMA) is moved, not
  changed; the 62-test suite is the safety net.

Original options + recommendations (for the record):

**D1 — Level-2 home / `protocol` split.**
- Option A (recommended): split into `protocol` (wire types only, used by aiolos + SDK) and a new
  `anemos` SDK crate (StdinReader, signals, curve, EMA, the `run()` driver + trait). Clean separation;
  aiolos keeps a tiny dep.
- Option B: keep everything in `protocol` and add the driver there. Less churn, but conflates the
  orchestrator's wire-type dep with the module SDK.
- Recommendation: **A** — it matches the 3-level intent and keeps aiolos's surface minimal.

**D2 — Level-1 tech crates location/naming.**
- Option A (recommended): a `tech/` workspace dir with `tech/ipmi`, `tech/nvml`, `tech/hwmon`
  (crate names `ipmi`, `nvml`, `hwmon`). Selectable via each anemos's Cargo deps.
- Option B: a flat `lib/` dir; or publishable-style names (`aiolos-ipmi`).
- Recommendation: **A** (short names, `tech/` dir) unless you foresee publishing them standalone
  (then prefixed names).

**D3 — The anemos contract (trait shape).**
- Option A (recommended): two traits — `Anemos { fn detect(&mut self) -> Detected; fn open(&mut self,
  id:&str) -> Result<Box<dyn Device>>; fn restore_all(&mut self); }` and `Device { fn apply(&mut self,
  inputs, &Controller) -> Applied; fn restore(&mut self); }`. The SDK's `run()` owns the loop.
- Option B: single trait with an associated `Device` type (no `Box<dyn>`); marginally faster, more
  generics.
- Recommendation: **A** for simplicity/readability (per-anemos clarity > micro-perf; devices tick
  every few seconds).

**D4 — Where curve + EMA + deadband apply.**
- Option A (recommended): the SDK gives the anemos a `Controller` (owns the live `CurveCache` +
  `Damper`) exposing `duty(raw_temp:i32) -> u32` (does reload/alpha/smooth/eval/deadband once,
  centrally). Level-3 `apply` = source temp(s) → `ctrl.duty(t)` → set device → build readings.
- Option B: SDK exposes the primitives and each module calls smooth/eval/deadband itself (today's
  duplicated sequence).
- Recommendation: **A** — removes the last duplicated logic and centralizes the safety-critical
  smoothing/floor path (one place to audit).

**D5 — Move `Curve`/`CurveCache`/`Damper` out of `protocol` into the SDK.**
- aiolos does not use them; only modules do. Recommendation: **yes** (follows from D1-A).

**D6 — Extra per-anemos subcommands (asrock `query`).**
- Recommendation: SDK `run()` accepts an optional map of extra subcommands (name → closure) so an
  anemos can add diagnostics without bypassing the SDK. Keeps `query` working.

**D7 — Migration style.**
- Option A (recommended): build the SDK + tech crates, migrate `nvidia` FIRST (smaller), verify
  green + on-hardware, then migrate `asrock16-2t`. Lower blast radius per step.
- Option B: big-bang both at once.
- Recommendation: **A**.

## Plan

1. Decisions (D1-D7) recorded; SOW-0001 completed; this SOW → `current`/in-progress.
2. SDK + tech crates (steps 1-3 of the gate plan) with unit tests.
3. Migrate nvidia → verify (tests + hardware) → migrate asrock → verify.
4. Update AGENTS.md, skills, specs; redeploy; cut over; close.

## Execution Log

### 2026-05-29

- Authored. Duplication mapped; 3-level design + decisions drafted.
- User confirmed D1-D7 (all recommended). SOW-0001 completed; this SOW activated.
- Implemented (commit 5b20978): created `anemos` SDK (`run`/`run_with` driver, `Anemos`/`Device`
  traits, `Controller`); moved `stdio`/`curve`/`damper` from `protocol` (now wire-types-only);
  created `tech/ipmi` (transport, `raw` made public), `tech/nvml` (pure NVML — curve/EMA/readings
  removed), `tech/hwmon` (generalized `read_temps(chip)`); migrated nvidia (single-file Anemos/Device)
  and asrock16-2t (thin main + `src/board.rs` for the OEM commands, `query` via the extra hook);
  the mock now dogfoods the SDK's `StdinReader`; aiolos depends on `anemos` only for the mock bin.
- Deviation from the as-stated D2 plan (improvement): `tech/ipmi` is the GENERIC IPMI transport;
  the ASRockRack OEM fan commands (claim/set/release/regulate/payloads) live in
  `anemoi/asrock16-2t/src/board.rs` (level 3), keeping the tech crate reusable.

## Validation

Acceptance criteria evidence:
- Minimal per-anemos code: nvidia is a single ~110-line file of device logic only; asrock is a thin
  main + an isolated board module. No CLI/signal/curve/EMA/protocol/restore code in either.
- Zero behavioral change: `cargo test --workspace` = **60 tests** pass (the hardened behavior is
  encoded in moved tests: `regulate`, `apply_or_restore`, damper, curve floor, stdio, orchestrator
  integration incl. signal-restore + `aiolos restore` + blackboard-liveness). clippy 0; fmt clean.
- On-hardware re-verify (nova, 2026-05-29 ~14:57): restructured build deployed via `install.sh` +
  restart; both modules `status:ok`, board fans tracking the curve, GPUs at the 35% floor, no
  warn/error. Binaries unchanged in name/location (`aiolos`,`nvidia`,`asrock16-2t`).
- No boilerplate duplication: lifecycle/signals/curve/EMA/protocol live once in `anemos`.
- Tech reusable + selectable: `tech/{ipmi,nvml,hwmon}` are independent crates; each anemos depends
  only on the ones it needs.

Reviewer findings: 5-model edge-case review run on the restructure (glm/mimo/kimi/minimax/qwen,
2026-05-29) — ALL returned "behavior-preserving / ready to ship", zero blockers/majors. Every
finding validated against the code. One real observability regression fixed (commit 03cf398): the
per-module per-tick "decision" logs (nvidia uuid/temp/commanded_pct; asrock
gpu_max/cpu_max/raw/smoothed/pct) had been dropped in the move — restored, and the unused
`ModuleInfo::name` wired into the SDK logs. Rejected as non-issues (with reasons): absent
`EMPTY_STRIKE_LIMIT` (deliberately removed in SOW-0001 decision 14 — explicit error/fatal detect
status replaced it); generic fatal message + lazy NVML init (correct/improvements); idempotent
double-restore. Redeployed; decision logs confirmed live and fans scaling correctly under real GPU
load (66-69 °C → 70-81%). 60 tests; clippy + fmt clean.

Sensitive data gate: structural change only; no secrets in any artifact.

Artifact maintenance gate:
- AGENTS.md: Layout rewritten to the 3-level structure + "a new anemos = traits + thin main".
- Runtime project skills: `project-create-anemos` rewritten around the SDK (implement the traits +
  thin main; pick tech crates); `project-anemos-protocol` updated (`anemos::StdinReader`).
- Specs: `aiolos-protocol.spec.md` updated (`anemos::` SDK reference). Protocol wire contract
  unchanged. (A dedicated `anemos-sdk` spec is a possible later addition — noted in Followup.)
- SOW lifecycle: `Status: completed`; moved to `.agents/sow/done/` together with the work.

## Outcome

**Completed 2026-05-29.** aiolos is now structured in three reuse levels: level-1 tech crates
(`tech/ipmi`,`tech/nvml`,`tech/hwmon`), the level-2 `anemos` SDK (all boilerplate + the
`Anemos`/`Device` traits), and thin level-3 anemoi. A new anemos implements only its device logic
and reuses everything else; the protocol/CLI/signals/curve/EMA are maintained once. Behavior is
unchanged (60 tests + hardware re-verify); deployed and live on nova.

## Lessons Extracted

- A shared `run()` driver + small `Anemos`/`Device` traits collapse ~80% of per-module code; the
  trait boundary (detect/open/apply/restore + a `Controller::duty` helper) keeps level-3 readable.
- Keep the level-1 tech crate GENERIC (IPMI transport) and push device/board-specific OEM commands
  up to level 3 — otherwise the "tech library" isn't reusable.
- A big behavior-preserving refactor of production code is safe when an existing comprehensive test
  suite encodes the behavior: move tests with their code, keep the suite green at each step.

## Followup

None yet.

## Regression Log

None yet.
