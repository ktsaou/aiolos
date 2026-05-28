# SOW-0002 - Test & validate aiolos

## Status

Status: open
Sub-state: pending; depends on SOW-0001 implementation chunks landing.

## Requirements

### Purpose
Define and execute the test/validation strategy for `aiolos` and its anemoi: protocol
conformance, orchestrator supervision/isolation, per-module behavior, and on-hardware safety
(especially fan fail-safes) before cutover from the C `nvfd`.

### User Request
"Add SOWs for implementation and testing."

### Assistant Understanding
Facts:
- The protocol is line-delimited JSON with strict request/response; modules are separate
  processes; fail-safe = restore device to firmware/BMC auto on shutdown/EOF.
- Production GPUs/CPUs depend on cooling; the C `nvfd` keeps running until aiolos is validated.
Inferences:
- Most logic is unit/integration-testable without hardware via a **mock anemos** that speaks the
  protocol (deterministic detect/apply, injectable hangs/errors).
Unknowns: none blocking.

### Acceptance Criteria
- **Protocol conformance suite**: golden request→response cases; rejects malformed lines;
  enforces stdout-only-protocol (a module writing junk to stdout is flagged).
- **Orchestrator tests** (with a mock anemos): reconcile on detect changes (add/remove id);
  fan-out timing; a hung module is killed within ~timeout and respawned while a sibling keeps
  ticking; `input=` routing delivers prior-tick readings; SIGTERM shuts all down.
- **Isolation test**: mock module that sleeps past timeout / exits / floods stderr does not
  stall the orchestrator or siblings.
- **nvidia**: on hardware, fan % tracks curve; per-GPU processes independent; shutdown/EOF
  restores firmware auto (verified by reading fan policy after exit).
- **asrock16-2t**: on hardware, all-manual+non-zero `0xd6` sets fans; `0xda` readback matches;
  shutdown/EOF releases BMC auto (verified); never leaves the claimed-but-undutied minimum trap;
  a single monitored test never drops CPU cooling unsafely.
- **Footprint**: idle RSS single-digit MB measured.
- CI-runnable subset (no hardware) is green; hardware subset documented as a manual runbook.

## Analysis
Sources checked: the four specs; `/opt/nvfd/TODO.md` (verified IPMI behavior incl the 0xcc trap).
Current state: greenfield. Risk: hardware tests touch production cooling — gate behind user
approval, monitor temps, guaranteed restore.

## Pre-Implementation Gate
Status: blocked (fill before execution)
- Problem/root-cause model: N/A (new test suite).
- Evidence reviewed: specs, IPMI findings.
- Affected contracts: protocol conformance is the key contract under test.
- Patterns to reuse: a reusable **mock anemos** binary for orchestrator tests.
- Risk/blast-radius: hardware fan tests = whole-system cooling; monitored + guaranteed restore.
- Sensitive-data plan: no secrets in test fixtures/logs.
- Implementation plan: see Plan.
- Validation plan: the suite IS the validation; on-hardware runbook for the device parts.
- Artifact-impact plan: specs' acceptance criteria are the source of truth; update if behavior shifts.
- Open decisions: which hardware tests are safe to automate vs manual-only (lean manual for
  anything that claims fans).

## Plan
1. Build a **mock anemos** (Rust) honoring the protocol with injectable behaviors (hang, error,
   detect-set changes, stderr noise).
2. Protocol conformance unit tests (serde round-trip + golden lines + malformed-input rejection).
3. Orchestrator integration tests against the mock (reconcile, timeout-kill+respawn, isolation,
   `input=` routing, SIGTERM).
4. Footprint check (idle RSS).
5. On-hardware manual runbook: nvidia restore-on-exit; asrock claim/duty/release + the 0xcc-trap
   guard + monitored CPU-fan test; documented step-by-step with restore commands.

## Execution Log
(none yet)

## Validation
- CI subset green; isolation test proves a hung module cannot stall siblings.
- Hardware runbook executed with temps monitored and fans confirmed returning to auto.
- Same-failure scan: re-check the `0xd6` all-manual/non-zero rule still holds on the running BMC.

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
- Wire the CI subset into the repo's CI when hosting is decided.

## Regression Log
None yet.
