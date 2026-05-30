# SOW-0004 - NVMe disk temperatures drive the ASRockRack board fans

## Status

Status: in-progress

Sub-state: code-complete; 3-round external review converged (5/5 ready to ship); awaiting USER
runtime testing + cutover approval before completion. Not committed.

## Requirements

### Purpose

Make the ASRockRack ROME2D16-2T board fans respond to NVMe SSD temperature: when the NVMe
disks get hot, the board increases its fans. The orchestrator must stay domain-agnostic and the
process-isolation guarantee must be preserved (a hung/failing disk must not perturb board-fan
control).

### User Request

> "I want to also add nvme disks, passed to asrock, so that if nvme disks get hot, the
> motherboard will increase its fans."

### Assistant Understanding

Facts:

- Host has 2× Samsung 990 PRO NVMe drives (`nvme0`, `nvme1`). Both expose temperatures via Linux
  hwmon under chip `name=nvme`: `temp1=Composite`, `temp2=Sensor 1`, `temp3=Sensor 2` (milli-°C).
- Each controller dir `/sys/class/nvme/nvmeN/` contains `serial`, `model`, and a single `hwmonM/`
  subdir; `/sys/class/hwmon/hwmonM/device` resolves back to `nvmeN`. Serial is a stable ID;
  `nvmeN`/`hwmonM` numbering is probe-order-dependent (not stable).
- `asrock16-2t` today computes driving temp = `max(GPU temps from inputs, k10temp CPU temps)`
  (`anemoi/asrock16-2t/src/main.rs:68-90`) and is wired `asrock16-2t input=nvidia`
  (`packaging/aiolos.conf:15`).
- The orchestrator's registry supports only **one** `input=` per module: `RegistryEntry.input`
  is `Option<String>` (`aiolos/src/registry.rs:11`), and `build_inputs` resolves a single source
  (`aiolos/src/main.rs:343-371`).
- `DESIGN.md:298` and `aiolos/src/registry.rs:59` both name a future `nvme` anemos as the
  canonical extensibility example.
- NVMe hwmon temperature reads go through the NVMe driver (a SMART-log admin command); healthy
  reads measured ~0–10 ms, but they can block during a controller reset/timeout (admin timeout
  default ~60 s) — unlike `k10temp` register reads. This is why an isolated process is preferred.

Inferences:

- A sensor-only anemos (reports temps, controls no device) is a new but valid shape: `restore`
  is a no-op, no curve is needed. The SDK currently assumes a curve exists and logs an error when
  it is empty (`anemos/src/run.rs:130`); this needs a clean "sensor-only" path.

Unknowns:

- None blocking. Implementation-shape choices are listed under Open Decisions (4–8) for user
  confirmation before code.

### Acceptance Criteria

- A new `nvme` anemos: `nvme detect` lists one entry per drive (id = serial, name = model);
  `nvme run <serial>` reports that drive's temps within timeout; EOF/SIGTERM/`nvme restore` exit
  cleanly (no-op fail-safe). Verified by the protocol smoke test + unit tests.
- The orchestrator routes **multiple** `input=` sources into one consumer. Verified by a new
  integration test wiring two mock producers into one consumer and asserting the merged max.
- `asrock16-2t` driving temp includes NVMe: `max(GPU, CPU, NVMe)`. When an NVMe drive is hot, the
  board duty rises. Verified by a unit test over the driving-max function with NVMe inputs.
- Per-drive visibility (decision 3B): each NVMe drive appears as its own instance with its
  per-sensor readings; asrock reports an `NVMe` driving-contribution reading. Verified on the
  status page / readings summary.
- Isolation preserved: a hung `nvme` instance does not stall `asrock16-2t` (it keeps cooling on
  GPU+CPU; the stale NVMe input is dropped). Verified by the existing isolation tests + the design
  (separate process).
- No real serials/secrets in committed artifacts. Off-hardware build + tests green; on-hardware
  validation and `nvfd` cutover remain operator-gated (out of scope here).

## Analysis

Sources checked:

- `anemoi/asrock16-2t/src/main.rs`, `anemoi/nvidia/src/main.rs` (module patterns).
- `anemos/src/run.rs`, `anemos/src/controller.rs`, `anemos/src/lib.rs` (SDK lifecycle, curve).
- `tech/hwmon/src/lib.rs` (generic sysfs reader; "no device specifics" contract).
- `aiolos/src/registry.rs`, `aiolos/src/config.rs`, `aiolos/src/main.rs` (routing, single input).
- `aiolos/src/bin/mock.rs`, `aiolos/tests/orchestrator.rs` (test harness; existing
  `input_routing_delivers_peer_readings`).
- `packaging/install.sh`, `update.sh`, `aiolos.conf` (binary list, registry).
- Specs: `aiolos-protocol.spec.md`, `aiolos-orchestrator.spec.md`, `anemos-asrock16-2t.spec.md`,
  `anemos-nvidia.spec.md`; `DESIGN.md`.
- Live sysfs on this host (NVMe enumeration + hwmon linkage + read latency).

Current state:

- NVMe temps are not consumed anywhere. Routing is single-source. No `tech/nvme` crate, no `nvme`
  anemos.

Risks:

- Multi-input routing touches the orchestrator core + the protocol `inputs` contract — must stay
  backward-compatible with the single-`input=` form and must not break existing routing tests.
- Sensor-only SDK path must not regress the curve handling for `nvidia`/`asrock16-2t`.
- Peer-id collision across two routed sources would drop a reading (theoretical only: GPU UUID vs
  NVMe serial vs board name never collide). Must be documented and handled benignly.

## Pre-Implementation Gate

Status: ready (decisions 1–3 and 4–8 all resolved; user approved 2026-05-29)

Problem / root-cause model:

- The board cannot react to NVMe heat because NVMe temperature is never measured or routed. The
  consumer (`asrock16-2t`) only maxes GPU (routed) + CPU (local). Adding NVMe as a routed,
  isolated producer keeps the orchestrator agnostic and protects board-fan control from a wedging
  disk read (the one sensor here that can block). Chosen approach = Option B (user decision 1).

Evidence reviewed:

- See "Sources checked" above (files + line numbers) and the live-hardware investigation in this
  SOW's Facts.
- External OSS: none required. The Linux nvme-hwmon mechanism is referenced from general kernel
  behavior, not a mirrored repo; no `owner/repo @ commit` citation applies.

Affected contracts and surfaces:

- New crate `tech/nvme` (level-1) and new crate `anemoi/nvme` (level-3) → workspace members.
- `anemos` SDK: a sensor-only path (no curve) — `ModuleInfo` shape + `run_loop` (level-2; affects
  the contract every module uses; `nvidia`/`asrock16-2t` mains touched only at the `ModuleInfo`
  literal).
- Orchestrator: `registry.rs` (`input` → multiple), `config.rs` (field + tests), `main.rs`
  (`input_map` type + `build_inputs` merge).
- `asrock16-2t`: driving-max now includes NVMe inputs; one extra reading.
- Protocol/orchestrator specs: multi-input routing; new `anemos-nvme.spec.md`; updated
  `anemos-asrock16-2t.spec.md`; `DESIGN.md` registry example.
- Packaging: `install.sh`/`update.sh` binary list gains `nvme`; `aiolos.conf` gains `nvme` line and
  `asrock16-2t input=nvidia input=nvme`. (No nvme curve — sensor-only.)
- Project skills: `project-create-anemos` (note the sensor-only/no-curve pattern);
  `project-anemos-protocol` (note multi-input `inputs` merge). README mention of modules.

Existing patterns to reuse:

- The `nvidia` single-file module shape (detect/open/apply/restore + `anemos::run`).
- `hwmon` sysfs reading style (label discovery, milli/1000) — generalized for a specific dir.
- The `Reading`/`Inputs` types and asrock's `input_temps` consumer pattern.
- The integration harness + mock for the multi-input test (mirror
  `input_routing_delivers_peer_readings`).

Risk and blast radius:

- Medium. Orchestrator routing + the SDK `ModuleInfo` are shared surfaces. Mitigations:
  single-`input=` must keep working (tests pin it); the SDK change is additive (sensor-only is a
  new option; existing modules pass the curve as before); all changes are off-hardware and the
  `nvfd` cutover is untouched.

Sensitive data handling plan:

- NVMe **serials** are used as runtime IDs but MUST NOT appear in committed artifacts. Specs/SOW
  use `<serial>` placeholders. No other secrets involved. Live-evidence output in this SOW masks
  the serial.

Implementation plan (ordered):

1. `tech/nvme` crate: enumerate `/sys/class/nvme/nvme*` → `{id: serial, model, hwmon_dir}`; read a
   given drive's temps from its `hwmonM/temp*_input` (+ `_label`), returning `(label, °C)`. Sysfs
   root injectable (param/env) for unit tests against a fixture tree.
2. `anemos` SDK sensor-only path (decision 4): make the curve optional in `ModuleInfo`; when
   absent, skip the empty-curve error and treat the module as sensor-only (`Device::apply` ignores
   `ctrl`). Update `nvidia`/`asrock16-2t` `ModuleInfo` literals to the new shape. Unit-test that a
   curve-less module starts without the error and a curved module is unchanged.
3. Orchestrator multi-input (decision 5/6): `RegistryEntry.inputs: Vec<String>` parsing repeated
   `input=` and comma lists, back-compatible; `config.rs` field + tests; `main.rs` `input_map:
   HashMap<String, Vec<String>>` + `build_inputs` merging all sources (peer-id keyed; benign
   last-writer on the impossible collision, with a debug log). Unit + integration tests.
4. `anemoi/nvme` crate (level-3, sensor-only): detect → one entry per drive (id=serial);
   `run <serial>` → read that drive's temps, report per-sensor readings (decision 3B); `restore`
   = no-op. ~one file + `tech/nvme` dep.
5. `asrock16-2t`: add `nvme_max` from inputs into the driving max; report a `temp/NVMe` reading.
   Extend the existing driving-max unit test.
6. Packaging + registry: add `nvme` to `install.sh`/`update.sh`; `aiolos.conf` gains `nvme` and
   `asrock16-2t input=nvidia input=nvme`. Workspace `Cargo.toml` members.
7. Specs + skills + DESIGN + README updates.

Validation plan:

- `cargo build --release`, `cargo test --workspace`, `cargo clippy --all-targets`, `cargo fmt`.
- New unit tests: nvme tech enumeration/temp parse (fixture tree); registry multi-input parse;
  `build_inputs` two-source merge; asrock driving-max with NVMe; SDK sensor-only start.
- New integration test: two mock producers → one consumer → merged max input.
- Protocol smoke test for `nvme` (detect/apply/EOF/SIGTERM/restore one-liners).
- External reviewers (per project process) at the named-model effort, iterated to "ready to ship".
- On-hardware behavior + `nvfd` cutover: explicitly OUT OF SCOPE (operator-gated).

Artifact impact plan:

- AGENTS.md: layout/module list mentions a third anemos + multi-input — minor update likely.
- Runtime project skills: `project-create-anemos` (sensor-only pattern); `project-anemos-protocol`
  (multi-input `inputs`).
- Specs: new `anemos-nvme.spec.md`; update `anemos-asrock16-2t.spec.md`, `aiolos-protocol.spec.md`,
  `aiolos-orchestrator.spec.md`.
- End-user/operator docs: `DESIGN.md` (registry example/§15), `README.md` (module list),
  `packaging/aiolos.conf` comments.
- End-user/operator skills: none beyond the project skills above.
- SOW lifecycle: single SOW; complete + move to `done/` in one commit with the work.

Open-source reference evidence:

- None checked (no mirrored repos relevant). NVMe-hwmon behavior referenced generically.

Open decisions:

- ALL RESOLVED by the user (1–3 on first reply; 4–8 confirmed "ok, proceed" on 2026-05-29). See
  Implications And Decisions.

## Implications And Decisions

Resolved (user, 2026-05-29):

1. **Where NVMe is read → Option 1B:** a new isolated `nvme` anemos, routed into `asrock16-2t`
   via `input=`. Requires multi-input routing in the orchestrator. (Chosen over 1A in-process read
   for full process isolation, matching the project's core guarantee and `DESIGN.md` §15.)
2. **Which sensors → 2A:** driving temp uses the **max of all exposed NVMe sensors** (Composite +
   Sensor 1 + Sensor 2) across both drives.
3. **Reporting → 3B:** per-drive readings for richer status-page visibility (each drive is its own
   instance reporting its per-sensor temps).

Resolved (user, 2026-05-29 — "ok, proceed", all confirmed as recommended):

4. **SDK sensor-only shape.** Make `ModuleInfo`'s curve fields `Option` so a module can declare
   "no curve / sensor-only"; `run_loop` then skips the empty-curve error and the sensor's `apply`
   ignores the controller. *Alt:* ship a dummy `nvme.curve.json` and tolerate the existing error
   log. **Recommend the `Option` approach** (semantically correct, two trivial call-site edits in
   `nvidia`/`asrock16-2t`; no behavior change for curved modules).
5. **Multi-input syntax.** Accept repeated `input=` AND comma lists, e.g.
   `input=nvidia input=nvme` or `input=nvidia,nvme`; single `input=nvidia` stays valid.
   **Recommend supporting both** (most forgiving, fully back-compatible).
6. **Input keying across sources.** REFINED during implementation (recorded 2026-05-29; user
   directed "finish everything", flagged for veto at review): the routed `inputs` map is keyed by
   the full **`module:id`** (e.g. `nvidia:GPU-…`, `nvme:<serial>`), not the bare peer id. *Why the
   change:* the originally-recommended peer-id keying does NOT let the consumer attribute a routed
   reading to its source module — which `asrock16-2t` now requires to label GPU vs NVMe distinctly
   (decisions 2A/3B/8) and to keep its existing `temp/GPU` reading correct once NVMe also flows in.
   `module:id` keying is **non-breaking** (the only shipped consumer iterated values, not keys; the
   mock + integration test are key-agnostic) and **collision-proof** by construction. The protocol
   spec is updated accordingly. This supersedes the earlier "keep peer-id keying" recommendation.
7. **Tech layer.** New `tech/nvme` level-1 crate (enumerate by serial + read per-controller hwmon
   temps), keeping NVMe device knowledge out of the generic `tech/hwmon` ("no device specifics").
   *Alt:* add `hwmon::read_temps_at(dir)` and do enumeration in the anemos. **Recommend
   `tech/nvme`** (cohesive, reusable, respects the hwmon contract).
8. **asrock NVMe reading granularity.** asrock reports a single `temp/NVMe` = max (symmetric with
   the existing single `temp/GPU` max); per-drive detail is visible via the `nvme` instances
   themselves. *Alt:* asrock emits one reading per input drive. **Recommend the single max in
   asrock** (consistent with GPU handling; 3B per-drive visibility already provided by the nvme
   instances).

## Plan

1. `tech/nvme` crate + unit tests (fixture sysfs).                              [decision 7]
2. `anemos` sensor-only path + `ModuleInfo` Option + update two mains + tests.  [decision 4]
3. Orchestrator multi-input (registry/config/main) + unit + integration tests.  [decisions 5,6]
4. `anemoi/nvme` sensor-only module + protocol smoke test.                       [decisions 1,3]
5. `asrock16-2t` driving-max includes NVMe + reading + unit test.               [decisions 2,8]
6. Packaging/registry/workspace wiring.
7. Specs + skills + DESIGN + README; reviewers; validation gate; complete.

## Execution Log

### 2026-05-29

- SOW created. Investigation complete (code + live hardware). User decisions 1B/2A/3B recorded.
  Gate written; items 4–8 confirmed by user ("ok, proceed"). Gate → ready.
- Mid-work the user reported asrock not reporting RPM. Triaged (read-only): NOT a regression
  (asrock never reported RPM; spec deferred it). RPM is feasible via IPMI SDR. User chose to
  finish this SOW first → captured RPM work + decisions in **SOW-0005** (pending). Resuming NVMe.
- Implementation finding: decision 6 (peer-id keying) does not let asrock attribute NVMe vs GPU
  inputs, which decisions 2A/3B/8 require. Refined decision 6 → key routed `inputs` by `module:id`
  (non-breaking, collision-proof, self-describing source); recorded above; flagged for user veto at
  review.
- User constraints received: do NOT run/stop/install/test aiolos (production GPUs at risk).
  Finish all code, verify by compile/lint/format only, then external review (glm/mimo/kimi/qwen/
  minimax); user drives all runtime testing later.
- Implemented chunks 1–7 (all keying-dependent and -independent):
  - `tech/nvme` crate (enumerate by serial + per-drive hwmon temps; sysfs root injectable) + 3 unit
    tests.
  - `anemos` SDK sensor-only path: `ModuleInfo.curve_* : Option`; `run_loop` skips curve-empty
    warning + constructs an unused controller when `None`. Updated `nvidia`/`asrock16-2t` mains to
    `Some(...)`.
  - Orchestrator multi-input: `RegistryEntry.inputs: Vec<String>` (repeated/comma, dedup, ordered);
    `config.rs`/`main.rs` updated; `build_inputs` merges all sources keyed by `module:id` (the `:`
    guards short-name prefix matches); registry + build_inputs unit tests + a multi-input
    integration test (writes only — NOT run).
  - `anemoi/nvme` sensor-only module (detect by serial, per-tick re-resolve, report per-sensor
    temps; restore no-op).
  - `asrock16-2t`: driving max = `max(all routed temps, CPU)`; partitions by source for distinct
    `temp/GPU` + `temp/NVMe` readings; `input_temps_from` + `push_temps` helpers + 2 unit tests.
  - Packaging (`install.sh`/`update.sh` add `nvme`; `aiolos.conf` wires `input=nvidia input=nvme`);
    workspace members; specs (new `anemos-nvme.spec.md` + updated protocol/orchestrator/asrock);
    `DESIGN.md`, `README.md`, `AGENTS.md`, both project skills.
- Verified COMPILE-ONLY: `cargo fmt --check` clean, `cargo clippy --all-targets` clean,
  `cargo build --release` ok, `cargo test --workspace --no-run` compiles all tests. Nothing was
  executed. External reviews next.

## Validation

**Constraint:** user directed NO runtime testing (production GPUs at risk) — verification is
compile/lint/format + external review only; the user drives all runtime testing later.

Acceptance-criteria evidence (compile-time + review; runtime PENDING user):
- New `nvme` anemos (detect by serial / per-drive temps / no-op restore): implemented
  `anemoi/nvme/src/main.rs`; `tech/nvme` unit tests cover enumeration + per-drive temp parsing
  against a fixture sysfs tree.
- Multi-input routing: `build_inputs` merges sources keyed by `module:id`; unit tests
  (`build_inputs_merges_multiple_sources_keyed_by_module_id`, edge-case test) + integration test
  `multi_input_routing_merges_sources` (written; NOT run).
- asrock driving max includes NVMe + reports `temp/NVMe`: `input_temps`/`input_temps_from` +
  unit tests.
- Sensor-only SDK path: `ModuleInfo` curve `Option` + `curve_path` unit tests.
- Isolation/fail-safe unchanged: verified by review (no edits to instance kill/restore paths;
  nvme restore is a no-op).

Tests or equivalent validation (compile-only — NOT executed):
- `cargo fmt --all --check` clean; `cargo clippy --all-targets` clean; `cargo build --release` ok;
  `cargo test --workspace --no-run` compiles all unit + integration tests. Nothing was run.

Real-use evidence:
- PENDING — user-gated on-hardware run/cutover (the C `nvfd` keeps cooling GPUs meanwhile).

Reviewer findings (5 external models: GLM-5.1, MiMo-V2.5-Pro, Kimi-K2.6, Qwen3.6-Plus,
MiniMax-M2.7; full-scope, read-only, iterated):
- Round 1: design judged correct (isolation/fail-safe/protocol/non-breaking). Fixed validated
  findings: removed unused `tracing` dep; corrected stale `protocol` `Inputs` docstring; added
  `build_inputs`/`curve_path` edge tests; removed a redundant clone; softened an overstated comment.
  Discarded false positives (integer truncation matches the hwmon convention; per-tick re-enumerate
  is intentional for hotplug and reads only cached attrs).
- Round 2: all 5 → PRODUCTION QUALITY / READY TO SHIP. Last real item (Qwen/Kimi): a module name
  containing `:` would make `module:id` prefix-matching ambiguous → added registry rejection +
  warning + unit test + spec note. Verified GLM's "format! per blackboard entry" as a false
  positive (it is hoisted per source).
- Round 3 (confirming the `:` fix, full-scope): **all 5 → READY TO SHIP, no blocking defects.**
  GLM: "thermally safe — it can only increase cooling, never decrease it." MiniMax: "no defects
  remain." Kimi: "ship it." Only non-blocking, future-host robustness notes remained (below).

Same-failure scan:
- Searched for other `input`/peer-id key consumers and other `ModuleInfo`/curve call sites; the only
  consumers iterate `inputs.values()` (key-agnostic) or were updated; no other site assumed bare-id
  keys or non-optional curve fields.

Sensitive data gate:
- No secrets in artifacts. NVMe serials are runtime ids only; specs/SOW use `<serial>` placeholders;
  the live-evidence command output masked the serial.

Artifact maintenance gate:
- AGENTS.md: updated (layout + anemoi list). Specs: new `anemos-nvme.spec.md`; updated protocol /
  orchestrator / asrock specs. Docs: DESIGN.md, README.md, packaging/aiolos.conf. Skills:
  project-create-anemos (sensor-only) + project-anemos-protocol (multi-input/module:id). Follow-up:
  SOW-0005 (asrock RPM) queued.

Reviewer follow-up mapping (non-blocking, future-host robustness — Kimi/Qwen, deferred):
- "no-hwmon limp": a future host whose NVMe exposes no hwmon node yields `status:error` each tick
  (correct, but could be a clearer one-time log). Tracked, not blocking this host (both drives expose
  hwmon).
- "silent missing source": when a wired `input=` source produces no readings, the consumer cools on
  the remaining sources without a dedicated warning. Acceptable (fail-safe direction); a future
  observability nicety.
- Optional cosmetics declined: a build_inputs complexity comment; symmetric `:` validation of
  `input=` values (a `:`-bearing source name is already inert — it matches no module). Recorded.

## Outcome

**Code-complete and review-converged (3 rounds, 5 models, unanimous READY TO SHIP); NOT yet
runtime-validated.** Per user constraint (production GPUs at risk) nothing was run/installed/tested:
verification is compile + lint + format + external review only. The SOW stays **in-progress** until
the user runs the on-hardware validation (protocol smoke test + observed cooling) and approves
cutover; the C `nvfd` keeps cooling the GPUs until then. Not committed (awaiting user).

## Lessons Extracted

Pending.

## Followup

- **SOW-0005** (pending): `asrock16-2t` per-fan RPM via IPMI SDR + truthful pwm readback. Queued
  to start after this SOW completes (user decision 1a/2a/3, 2026-05-29).

## Regression Log

None yet.

Append regression entries here only after this SOW was completed or closed and later testing or
use found broken behavior. Use a dated `## Regression - YYYY-MM-DD` heading at the end of the file.
Never prepend regression content above the original SOW narrative.
