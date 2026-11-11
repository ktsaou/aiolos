# SOW-0024 - aiolos exits 101 when stderr becomes unwritable

## Status

Status: completed

Sub-state: fixed, regression-tested, rolled out; anemoi `eprintln!` hardening awaiting a user decision (Followup).

## Requirements

### Purpose

The orchestrator and modules must survive a broken log stream (journald namespace gone at system shutdown, full disk) instead of panicking.

### User Request

Diagnose the unit's recurring `status=101` exits; then decisions 1b (fix both the orchestrator and SDK subscribers), 2a (add an integration regression test), 3 (asked what is long-term best for the anemoi `eprintln!` sites).

### Assistant Understanding

Facts:

- All 12 historical `status=101` exits happened at system shutdown (each followed by a new-boot marker); 6 other shutdowns exited 0; manual stops exit 0.
- Reproduced with the real binary and mock modules: stderr write failing (EPIPE or ENOSPC) gives exit 101 in 12/12 runs; closed, `/dev/null` and drained stderr give 0.
- `strace`: the first failed log write is followed by tracing-subscriber's own `eprintln!` fallback ("Unable to write an event…"), which fails and panics the main thread (exit 101). `log_internal_errors` defaults to true on the fmt subscriber builder.

Inferences:

- In production the trigger is the journald namespace stream closing before aiolos finishes shutting down. A mid-run journald restart or a full journal disk would likely crash the controller too (not tested).

Unknowns:

- Original panic text was lost (namespace journal retained only from 2026-11-05).

### Acceptance Criteria

- With stderr on `/dev/full`, aiolos keeps running and exits 0 on SIGTERM (integration test; stress run).

## Analysis

Sources checked:

- Unit history, namespace journal, `aiolos/src/main.rs`, `anemos/src/run.rs`, tracing-subscriber 0.3.23 source, strace of a reproduction.

Current state:

- See Facts.

Risks:

- Low: only the subscriber's internal-error reporting is disabled; log content unchanged.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- tracing's fmt layer reports write errors via `eprintln!`; `eprintln!` panics when stderr is broken; the panic unwinds the main thread and exits 101.

Evidence reviewed:

- Reproduction matrix and strace above; the fix was first validated in a scratch copy (all stress cells exit 0).

Affected contracts and surfaces:

- None (logging internals).

Existing patterns to reuse:

- Existing `Harness` in `aiolos/tests/orchestrator.rs` (extended with a stderr parameter).

Risk and blast radius:

- Low. Modules still self-restore on SIGTERM/EOF; this removes an early, noisy aiolos exit.

Sensitive data handling plan:

- None involved.

Implementation plan:

1. `.log_internal_errors(false)` on the aiolos and anemos subscribers.
2. Regression test on `/dev/full`.
3. Install and observe.

Validation plan:

- Test fails without the fix and passes with it; full suite; live check.

Artifact impact plan:

- AGENTS.md, skills, specs, docs: unaffected (no contract change). SOW lifecycle: completed here.

Open-source reference evidence:

- tracing-subscriber 0.3.23 `src/fmt/fmt_layer.rs` and `src/fmt/mod.rs` (`log_internal_errors`); no external clone consulted.

Open decisions:

- Decision 3 (anemoi `eprintln!` sites) is out of this SOW's scope; see Followup.

## Implications And Decisions

1. 1b: fix both subscribers. 2. 2a: add the integration test. 3. Pending user choice.

## Plan

As above.

## Execution Log

### 2026-11-11

- Edited `aiolos/src/main.rs`, `anemos/src/run.rs`, `aiolos/tests/orchestrator.rs`.
- Verified the new test fails when the aiolos line is removed ("aiolos died when stderr became unwritable") and passes with it. Restored the line afterwards.

## Validation

Acceptance criteria evidence:

- Test above; scratch-copy stress: 10 of 10 cells exit 0 (previously 2 of 10 cells exited 101 on every run).

Tests or equivalent validation:

- `cargo fmt --check`, `cargo clippy --all-targets` (only the two pre-existing function-cast warnings), `cargo test`: 217 passed, 0 failed.

Real-use evidence:

- See rollout note in the final report/journal check.

Reviewer findings:

- No external review requested or run.

Same-failure scan:

- 23 `eprintln!` sites remain in anemoi/SDK/tech; 10 are restore/failure paths that can panic on a broken stderr pipe. Tracked in Followup.

Sensitive data gate:

- Clean.

Artifact maintenance gate:

- AGENTS.md / skills / specs / docs: no update needed (no contract change). SOW lifecycle: completed in `done/`.

Specs update:

- None needed.

Project skills update:

- None needed.

End-user/operator docs update:

- None affected.

End-user/operator skills update:

- None affected.

Lessons:

- Rust `eprintln!` panics on a broken stderr; code that must run during shutdown or restore should never use it. tracing-subscriber's internal-error fallback uses it by default.

Follow-up mapping:

- Tracked below.

## Outcome

Orchestrator and SDK subscribers no longer panic on an unwritable stderr.

## Lessons Extracted

See Lessons.

## Followup

- Anemoi restore/failure paths still use panicking `eprintln!` (nvidia, nvidia-powercap, it87, rome2d-fans). Awaiting user decision on scope (shared non-panicking SDK helper vs per-site).

## Regression Log

None yet.
