# SOW-0022 - Source-matched case-fan curves

## Status

Status: completed

Repository implementation and the approved target-host rollout are complete. The workstation was
not changed.

## Requirements

### Purpose

Keep every existing fan-control decision intact while adding one independent NVMe safety demand for
case fans. The hottest NVMe must raise all case fans gradually and reach 100% at 70 C.

### User Request

- Existing configuration must behave exactly as before.
- Match all NVMe temperature sensors, including all four drives on the target host.
- Reduce the matched NVMe temperatures by maximum.
- Apply a gradual `50 C -> 30%, 70 C -> 100%` NVMe curve with immediate response.
- Apply the NVMe result to every case fan, not the CPU fans.
- Final case-fan duty is `max(existing duty, NVMe duty)`.
- A sensor may affect the existing curve and the NVMe curve; rules must not use first-match
  assignment.
- On aiolos exit, modules release hardware to BIOS/BMC/firmware automatic control exactly as today.
- The second supported host must retain its existing behavior unless its NVMe overlay is explicitly
  enabled.

### Acceptance Criteria

- The normal existing fan calculation always runs and remains the baseline.
- An optional shared signal-to-curve overlay engine is used by `rome2d-fans` and `it87`.
- Every rule independently sees every matching numeric producer signal; there is no first-match
  consumption.
- Each rule takes the hottest matching sensor and applies its own curve/controller.
- Rule outputs combine by maximum; the combined overlay combines with the existing case duty by
  maximum.
- On `rome2d-fans`, the overlay affects FAN3-FAN8; FAN1/FAN2 retain their existing baseline duty.
- On `it87`, the overlay affects all configured case channels; configured CPU channels retain their
  existing baseline duty.
- The packaged NVMe selector matches every routed `nvme` temperature signal and therefore all four
  target-host drives without embedding serials in committed configuration.
- The NVMe curve is linear between 50 C / 30% and 70 C / 100%, uses `sensitivity:1.0`, and commands
  100% at 70 C on the same apply tick.
- No policy file, or an explicitly disabled policy, is a zero-effect overlay; existing configuration
  remains valid and no separate legacy algorithm is introduced.
- Once enabled, a missing required NVMe signal or invalid policy/curve fails only the case-fan
  overlay high to 100%; the existing baseline still runs for CPU-fan decisions.
- Failed/empty source reports stop routing stale telemetry; recovery restores routing.
- Shutdown, EOF, signal termination, and restore continue to release managed hardware to automatic
  firmware control.
- Repository tests demonstrate old-vs-new baseline equivalence, overlapping matches, hottest-of-four
  NVMe reduction, gradual interpolation, max composition, case-only targeting, stale-source pruning,
  and protocol sink reporting.
- No production file, service, process, or hardware state is changed without a separate approved
  rollout plan.

## Analysis

### Facts and Evidence

- `anemoi/rome2d-fans/src/main.rs` currently computes the established uniform or CPU/case-zone
  baseline before writing FAN1-FAN8.
- `anemoi/it87/src/main.rs` currently computes the established uniform or configured zone baseline
  before writing its managed PWM channels.
- Routed input keys retain the source as `module:instance`, and signals retain role, value, UOM, and
  labels (`protocol/src/lib.rs`; `aiolos/src/main.rs::build_inputs`). This is sufficient to select all
  NVMe temperatures without drive serials.
- The running target uses one baseline ROME2D curve for all eight fans. FAN1/FAN2 are CPU fans and
  FAN3-FAN8 are case fans.
- The existing target baseline is `50 C -> 30%, 75 C -> 70%, 90 C -> 100%` with sensitivity 0.5; it
  does not reach 100% at an NVMe temperature of 70 C.
- The existing shutdown lifecycle calls module restore, and fan modules release to BMC/firmware
  automatic mode (`anemos/src/run.rs`, `anemoi/rome2d-fans/src/main.rs`,
  `anemoi/it87/src/main.rs`). This behavior must not become policy-specific.
- The uncommitted first implementation assigned each signal to its first rule and replaced the
  baseline case result. Both contradict the clarified requirement and must be corrected before the
  work is review-clean.

### Root Cause

The current modules collapse unlike temperature sources into one maximum before one curve. An NVMe
therefore inherits a curve designed for general system temperature. The missing operation is an
independent NVMe curve whose result is overlaid with, rather than substituted for, the existing
case-fan result.

### Risks

- `rome2d-fans` must use its per-fan command so FAN3-FAN8 can be raised without changing FAN1/FAN2.
  The board requires an atomic eight-fan payload with the second half mirroring the first.
- A policy overlay must not lower an existing duty; max composition is a safety invariant.
- A transient missing NVMe source must not leave a stale last-good temperature routed indefinitely.
- A broken enabled safety curve should not silently disappear. It fails the affected case outputs
  high and reports a warning.
- Live hardware proof cannot be obtained safely during repository implementation. Deployment and
  service restart require separate approval.

## Pre-Implementation Gate

Status: passed on 2026-07-26 after the user replaced the previous design discussion with the exact
narrow behavior recorded above and instructed implementation to proceed.

### Affected Contracts

- Shared `anemos` signal-to-curve overlay semantics.
- `rome2d-fans` and `it87` case-fan decision composition and provenance.
- Aiolos routed-signal freshness semantics.
- Packaged optional policy and NVMe curve files.
- Orchestrator, protocol, and module specs plus operator design documentation.

### Patterns To Reuse

- Existing `Controller`, `Curve`, `Damper`, and curve live-reload conventions.
- Existing ROME2D uniform/zone calculation and fault compensation.
- Existing it87 uniform/zone calculation and channel mapping.
- Existing flat signal selectors and `module:instance` routed source identity.
- Existing `Report::ok_warn`, `Provenance`, and `Driving` reporting.
- Existing module-wide restore paths; no per-policy restore mechanism.

### Sensitive-Data Plan

Committed selectors match the generic `nvme` module and temperature label. No drive serial, host
identifier, BMC address/credential, endpoint, or personal/customer data is written to artifacts.

### Ordered Implementation Plan

1. Correct the shared policy engine so rules overlap and independently reduce matching inputs.
2. Preserve the existing baseline calculation in both fan modules and max-compose only case outputs
   with the policy overlay.
3. Make policy-disabled/absent state a zero-effect overlay and reset inactive overlay damping.
4. Keep required-source/configuration failure as a 100% case overlay without bypassing the baseline
   calculation.
5. Simplify packaged examples to one NVMe overlay rule and validate hottest-of-four plus interpolation.
6. Retain the routed-source freshness fix and its recovery tests.
7. Update specs, mandatory protocol skill, DESIGN, and packaging comments to the final semantics.
8. Run focused tests, workspace tests, clippy, formatting, diff checks, same-failure searches, and a
   final read-only review.
9. Stop before production and present a separate host-specific rollout plan.

### Validation Plan

- `cargo fmt --all --check`
- `cargo test -p anemos`
- `cargo test -p aiolos`
- `cargo test -p anemoi-rome2d-fans`
- `cargo test -p anemoi-it87`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --workspace --all-targets --no-fail-fast`
- `git diff --check`
- Same-failure searches for first-match assignment, policy substitution, stale routing, and any path
  that changes CPU fans because of an NVMe overlay.
- Production hardware verification only under a later approved rollout.

### Artifact-Impact Plan

- Update the active SOW, affected specs, mandatory protocol skill, DESIGN, and packaging comments.
- No AGENTS.md or new project skill is required.
- SOW remains current until repository validation/review and separately approved production evidence
  are complete.

### Open Decisions

None for repository implementation. Production rollout remains an action approval, not an unresolved
design question.

## Decisions

1. Existing fan calculation is always the baseline.
2. The NVMe curve is an independent overlay.
3. Each rule evaluates every matching sensor; rules may overlap.
4. Hottest sensor wins within a rule; maximum demand wins across rules and against the baseline.
5. The NVMe overlay targets all case fans and no CPU fans.
6. The packaged NVMe curve is `50:30, 70:100, sensitivity:1.0`.
7. All NVMe instances match generically; committed configuration contains no serials.
8. Missing/disabled overlay configuration has zero effect on existing behavior.
9. Enabled policy failure or required telemetry loss commands the case overlay to 100%.
10. Hardware ownership is released system-wide through the existing module restore lifecycle.
11. Repository implementation is approved; production changes are not.

Recommendation classification: **surgical**. This preserves the established control algorithms and
adds only a max-composed case-fan safety overlay.

## Execution Log

### 2026-07-26

- Completed read-only code, contract, installed-configuration, and live-status investigation.
- A first uncommitted implementation passed automated tests but used first-match/substitution
  semantics; review stopped it before deployment.
- User clarified the required narrow behavior and rejected further architecture questionnaires.
- Replaced the implementation gate and acceptance criteria with baseline-plus-overlay semantics.
- Corrected the shared engine so every matching rule receives the same signal independently; rule
  maxima and outputs combine by maximum.
- Refactored both fan modules so the established uniform/zone calculation always runs and only case
  outputs are raised by overlay demand.
- Added the disabled generic NVMe policies plus `50 C -> 30%, 70 C -> 100%,
  sensitivity:1.0` curves. Existing configuration remains valid without migration.
- Retained and tested removal of failed/empty source reports from the routing blackboard.
- Hardened strict overlay-curve loading against arithmetic overflow and symlink escape.
- Updated the relevant specs, protocol skill, DESIGN, and non-clobbering packaging.
- Built the complete release workspace. No production files, services, processes, or hardware state
  were changed.
- Received explicit approval for the target-host rollout.
- Captured rollback binaries and configuration under
  `~/tmp/aiolos-nvme-rollout-20260726T171847Z/`.
- Installed the verified release `aiolos` and `rome2d-fans` binaries plus the enabled NVMe overlay
  and its curve. The existing registry and baseline curve were not changed.
- Restarted `aiolos` once. The old fan module released control to BMC automatic mode during shutdown,
  and the new service became active with zero restarts.
- Verified four healthy NVMe instances and twelve live temperature signals.
- Observed the real overlay raising FAN3-FAN8 to 44% while FAN1/FAN2 remained at the 36% baseline;
  all eight tachometers reported non-zero RPM. This is consistent with a hottest NVMe reading near
  54 C on the configured linear curve.
- The required-source rule intentionally failed high for the first startup apply before routed NVMe
  telemetry arrived, then recovered on the following apply. No subsequent fail-high, `0xcc`,
  error/fatal status, panic, fan fault, or service restart occurred.

## Validation

### Acceptance Evidence

- `anemos/src/signal_curve_policy.rs`
  - independently matches every rule;
  - reduces each rule to its hottest signal;
  - max-combines rule outputs;
  - uses independent damping;
  - fails enabled invalid/required overlays high;
  - rejects unsafe curve paths and invalid curves.
- `anemoi/rome2d-fans/src/main.rs`
  - computes the established baseline first;
  - applies `max(baseline, overlay)` only to FAN3-FAN8;
  - leaves FAN1/FAN2 on their baseline;
  - preserves fan-fault compensation and release-to-BMC lifecycle.
- `anemoi/it87/src/main.rs`
  - computes the established baseline first;
  - applies `max(baseline, overlay)` to every configured case channel;
  - leaves configured CPU channels on their baseline;
  - preserves release-to-firmware lifecycle.
- Packaged policies match `module=["nvme"]` plus the temperature label, so every NVMe instance is
  included without serial-specific configuration.
- Shared tests use four NVMe instances, verify hottest-drive reduction, 60 C -> 65%, and 70 C ->
  100% on the same apply.
- Module tests verify overlays cannot lower baseline duty, touch no CPU fans, and raise all case
  fans/channels.

### Automated Validation

- `cargo fmt --all --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --workspace --all-targets --no-fail-fast` — passed, 212 tests.
- `cargo build --release` — passed.
- `git diff --check` — passed.

### Review Findings And Handling

- First-match assignment consumed an NVMe signal before broader rules could see it — replaced with
  independent per-rule matching and covered by an overlap regression test.
- The first implementation substituted a policy result for the existing case result — replaced with
  max composition after the normal baseline and covered by both module tests.
- Live policy disable could expose inactive baseline damping state — eliminated because baseline
  controllers now run every tick.
- A policy curve symlink could escape its configuration directory — canonical containment check and
  regression test added.
- Full-range accepted curve temperatures could overflow interpolation subtraction — arithmetic now
  converts to floating point before subtraction and has an extreme-point test.
- The packaged it87 fallback curve could lower the existing uniform demand — fallback rule removed;
  the policy now contains only the NVMe overlay.

### Same-Failure Search

- No first-match rule search remains in `anemos` or either fan module.
- Every policy output application is through `max`; no overlay assignment can lower baseline duty.
- ROME2D overlay indexing is confined to FAN3-FAN8; it87 uses only configured case channels.
- Hardware restore remains module-wide and independent of policy state.
- Failed/empty route pruning and successful recovery are covered in `aiolos` tests.

### Sensitive-Data And Artifact Gates

- No drive serials, host identifiers, credentials, private endpoints, or personal/customer data were
  added.
- Updated affected specs, the mandatory protocol skill, DESIGN, packaging comments, and active SOW.
  No AGENTS.md or new project skill was needed.

### Real-Use Evidence

- Target host:
  - service active with zero restarts after the single approved restart;
  - all five module detectors healthy;
  - all nine instances healthy, including four NVMe and two GPU instances;
  - four NVMe instances published twelve current temperature signals;
  - FAN1/FAN2 stayed on the existing baseline while FAN3-FAN8 responded to a higher live NVMe
    overlay;
  - all eight fan RPM values were present and non-zero;
  - no IPMI `0xcc` or post-telemetry warning/error appeared.
- Workstation:
  - no binary, configuration, service, or hardware change was made;
  - packaged overlay remains disabled by default, preserving existing behavior.

## Outcome

Completed. Existing configuration remains the baseline; all routed NVMe sensors contribute an
independent gradual demand to every case fan; maximum demand wins; 70 C maps to 100%; and module exit
still returns hardware control to BMC/firmware automatic mode.
