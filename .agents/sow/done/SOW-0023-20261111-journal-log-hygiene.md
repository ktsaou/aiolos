# SOW-0023 - Journal log hygiene (ANSI escapes + per-tick volume)

## Status

Status: completed

Sub-state: shipped and verified on the production host; one residual noise source tracked as follow-up.

## Requirements

### Purpose

Keep the `aiolos` journal namespace readable and bounded: no control bytes, and no per-second
steady-state spam.

### User Request

"aiolos on nova pollutes the journals with invalid characters and also logs way too much." Decisions
given: 1b (strip control characters from forwarded module stderr), 2b (log on change + heartbeat),
3 ok (build/install). Vacuuming old journals was not approved and was not done.

### Assistant Understanding

Facts:

- Every orchestrator log line carried raw ANSI colour escapes (subscriber in `aiolos/src/main.rs` had no `with_ansi(false)`; the SDK one already did).
- Baseline ~11 lines/s (~3 MB/10 min); the namespace held 3.9 GB. Split: `apply result` 63%, `routing inputs` 18%, anemoi `decision:` lines 18%.

Inferences:

- journald stores messages containing control bytes as binary blobs (the "invalid characters" symptom).

Unknowns:

- None blocking.

### Acceptance Criteria

- No ESC/control bytes in journal lines written by the new binaries (checked via journal grep).
- Steady-state per-tick lines are logged only on change plus a 5-minute heartbeat (unit tests + live line counts).
- Errors/status flips still logged immediately.

## Analysis

Sources checked:

- `aiolos/src/main.rs`, `aiolos/src/module.rs`, `aiolos/src/status_page.rs`, `anemos/src/run.rs`, anemoi `it87`/`rome2d-fans`/`nvidia`/`nvidia-powercap`, specs (no spec defines log content), live journal namespace.

Current state:

- See Facts above.

Risks:

- Less per-tick history in the journal (live values remain on the status page).
- Fan restart on rollout (devices restored to firmware during the swap).

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- ANSI: missing `.with_ansi(false)` in orchestrator subscriber. Volume: unconditional `info!` per instance per wake in `reap_results`/`dispatch_due` and per tick in anemoi `apply`.

Evidence reviewed:

- Live journal breakdown (10 min sample), code sites listed above.

Affected contracts and surfaces:

- Journal content only. Wire protocol, status page, curves, registry: unchanged.

Existing patterns to reuse:

- `status_page::strip_ansi` (made `pub(crate)`) for forwarded stderr; SDK `Device`/`Controller` structure for the gate field.

Risk and blast radius:

- Low: logging-only. `aiolos` intentionally keeps a tiny duplicate of the gate (it depends only on `protocol`, not the SDK).

Sensitive data handling plan:

- No secrets involved; no GPU UUIDs, serials or hostnames recorded here.

Implementation plan:

1. `anemos::LogGate` (change + heartbeat) with unit tests; export from SDK.
2. Gate anemoi decision logs keyed on the commanded outcome.
3. Orchestrator: `with_ansi(false)`, sanitize forwarded stderr, gate `apply result` (key: status+error) and `routing inputs` (key: source set).
4. fmt/clippy/test, install, measure live.

Validation plan:

- cargo fmt/clippy/test; live journal ESC count and line rate after rollout.

Artifact impact plan:

- AGENTS.md: unaffected. Runtime skills: unaffected (stderr=logs rule unchanged). Specs: unaffected (none define log content). Docs: none. SOW lifecycle: this SOW, completed in the same commit.

Open-source reference evidence:

- Not relevant (internal logging change).

Open decisions:

- None. Heartbeat fixed at 300 s (constant); not made configurable.

## Implications And Decisions

1. 1b: strip control characters from forwarded module stderr (plus orchestrator `with_ansi(false)`, required to fix the actual symptom).
2. 2b: log on change + heartbeat.
3. Build and install approved; journal vacuum not approved.

## Plan

As in the Implementation plan.

## Execution Log

### 2026-11-11

- Added `anemos/src/log_gate.rs` and `aiolos/src/log_gate.rs`; edited orchestrator, three anemoi, SDK exports.
- Installed via `packaging/update.sh`; service active.

## Validation

Acceptance criteria evidence:

- Live, 43 s after restart: 64 lines (~1.5/s vs ~11/s), 0 lines with ESC bytes. `apply result` 7 and `routing inputs` 2 (startup/changes) vs thousands before.

Tests or equivalent validation:

- `cargo fmt --all`, `cargo clippy --all-targets` (only the two pre-existing function-cast warnings), `cargo test`: all suites pass, including new `LogGate` tests.

Real-use evidence:

- Production host restarted onto the new binaries; journal inspected as above.

Reviewer findings:

- No external review requested or run.

Same-failure scan:

- Remaining per-tick `info!` sites reviewed: nvidia-powercap logs only on state changes (already change-based); nvml/restore lines are one-shot.

Sensitive data gate:

- Clean.

Artifact maintenance gate:

- AGENTS.md / skills / specs / docs: no update needed (see Artifact impact plan).
- SOW lifecycle: completed, in `done/`, committed with the work.

Specs update:

- None: no spec defines journal content.

Project skills update:

- None needed.

End-user/operator docs update:

- None affected.

End-user/operator skills update:

- None affected.

Lessons:

- tracing-subscriber emits ANSI regardless of TTY; every subscriber that writes to journald must set `with_ansi(false)`.
- Gating on commanded duty is still noisy when the duty itself flaps tick to tick (see Followup).

Follow-up mapping:

- Tracked below.

## Outcome

ANSI escapes eliminated; steady-state volume down ~85% on the live host.

## Lessons Extracted

See Validation > Lessons.

## Followup

- it87 `decision:` lines still log ~0.75/s under CPU load because the commanded duty changes nearly every tick. Options: log only when duty moves by >= N points, or demote to debug. Needs a design decision.
- Optional: journald cap for the namespace (`journald@aiolos.conf`); vacuum of the existing 3.9 GB (irreversible, not approved).
- Unit exits with status 101 (panic) on several past stops/restarts; uninvestigated.
- Optional shared tiny crate to avoid the duplicated `LogGate`.

## Regression Log

None yet.
