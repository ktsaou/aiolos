# SOW-0025 - Non-panicking stderr helper for anemoi restore/failure paths

## Status

Status: completed

Sub-state: shipped; follow-up to SOW-0024.

## Requirements

### Purpose

Restore and failure messages in the anemoi must never abort a device restore because stderr is broken (journald stream gone at shutdown).

### User Request

"Do it" - the long-term-best option for decision 3 of SOW-0024: one shared non-panicking helper in the SDK, applied to all restore/failure `eprintln!` sites.

### Assistant Understanding

Facts:

- `eprintln!` panics on a broken stderr (reproduced in SOW-0024). 10 restore/failure `eprintln!` sites in `nvidia`, `nvidia-powercap`, `it87`, `rome2d-fans` could panic mid-restore.
- 3 remaining `eprintln!` in anemoi are startup config messages (`hwmon-temps`, `nut`, `it87` config load).

Inferences:

- A panic there unwinds and may cut a restore short; Drop-based restore is the backstop.

Unknowns:

- None.

### Acceptance Criteria

- `anemos::elog!` exists; a test proves it does not panic on an unwritable stderr, and fails if `eprintln!` is used instead.
- All 10 sites use it; build, clippy and tests clean.

## Analysis

Sources checked:

- All `eprintln!` sites in anemos/anemoi/tech/aiolos.

Current state:

- See Facts.

Risks:

- Low: a failed diagnostic write is silently dropped (by design).

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- Std `eprintln!` panics on write error; restore paths run exactly when stderr is most likely broken.

Evidence reviewed:

- SOW-0024 reproduction; grep of every `eprintln!`.

Affected contracts and surfaces:

- None (diagnostic output only; protocol stdout untouched).

Existing patterns to reuse:

- SDK macro/export style in `anemos/src/lib.rs`.

Risk and blast radius:

- Low; message text unchanged.

Sensitive data handling plan:

- None involved.

Implementation plan:

1. `anemos/src/elog.rs` macro + test; export.
2. Replace the 10 sites.
3. Gate, deploy, observe.

Validation plan:

- Test passes with the macro and fails with `eprintln!`; fmt/clippy/full tests; live check.

Artifact impact plan:

- AGENTS.md, skills, specs, docs: unaffected. SOW lifecycle: completed here.

Open-source reference evidence:

- Not relevant.

Open decisions:

- None.

## Implications And Decisions

Decision 3 of SOW-0024: shared SDK helper applied to all 10 restore/failure sites (user: "do it").

## Plan

As above.

## Execution Log

### 2026-11-11

- Added `anemos/src/elog.rs`; replaced the 10 sites in nvidia, nvidia-powercap, it87, rome2d-fans.
- Verified the test fails with `eprintln!` (child exit 101) and passes with `elog!`; macro restored.

## Validation

Acceptance criteria evidence:

- As above.

Tests or equivalent validation:

- `cargo fmt --check`, clippy (only the two pre-existing function-cast warnings), full `cargo test` green.

Real-use evidence:

- Rolled out; service active, journal clean.

Reviewer findings:

- No external review requested or run.

Same-failure scan:

- Remaining `eprintln!` are the 3 startup config messages plus `aiolos/src/bin/mock.rs` (test helper); none run on shutdown/restore.

Sensitive data gate:

- Clean.

Artifact maintenance gate:

- AGENTS.md / skills / specs / docs: no update needed. SOW lifecycle: completed in `done/`.

Specs update:

- None needed.

Project skills update:

- None needed. (Possible future note in `project-create-anemos`: use `anemos::elog!` on restore paths; not required.)

End-user/operator docs update:

- None affected.

End-user/operator skills update:

- None affected.

Lessons:

- Restore and shutdown paths must not use panicking std print macros.

Follow-up mapping:

- None.

## Outcome

Restore/failure messages can no longer panic a module.

## Lessons Extracted

See Lessons.

## Followup

None yet.

## Regression Log

None yet.
