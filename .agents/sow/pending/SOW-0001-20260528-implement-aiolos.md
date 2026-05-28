# SOW-0001 - Implement aiolos (orchestrator + nvidia & asrock16-2t anemoi)

## Status

Status: open
Sub-state: pending review of DESIGN.md + specs; pending the open parameter decisions below.

## Requirements

### Purpose
Build `aiolos`: a lean, domain-agnostic orchestrator (Rust) that supervises autonomous module
binaries ("anemoi") over the one-line-JSON stdio protocol, plus the first two anemoi — `nvidia`
(GPU onboard fans) and `asrock16-2t` (ROME2D16-2T board fans via IPMI). Replace the current C
`nvfd` once tested.

### User Request
"Build the agnostic orchestrator + the nvidia and asrock16-2t modules per DESIGN.md; Rust
orchestrator (lean, no GC); modules may be Rust/C; install to /opt/aiolos."

### Assistant Understanding
Facts:
- Design and contracts exist: `DESIGN.md`, `.agents/sow/specs/aiolos-protocol.spec.md`,
  `aiolos-orchestrator.spec.md`, `anemos-nvidia.spec.md`, `anemos-asrock16-2t.spec.md`.
- IPMI fan control on this board is verified (all-manual + non-zero `0x3a 0xd6`; release via
  `0x3a 0xd8` ×16 0x00). NVML fan set/reset verified by the existing C nvfd.
- Toolchain present: rustc/cargo 1.90, go 1.24.6, gcc; libfreeipmi + nvml libs present.
Inferences:
- std-threads Rust (no async) is sufficient at this scale and keeps the binary lean.
Unknowns:
- None blocking; the open parameter decisions below are defaults to confirm, not unknowns.

### Acceptance Criteria
- Orchestrator spawns/reconciles modules from `/opt/aiolos/etc/aiolos.conf`; routes `input=` data.
- A hung module is killed within ~timeout and respawned; siblings keep ticking (verified).
- `nvidia` controls per-GPU fans by curve; isolation per GPU process; restores firmware on exit.
- `asrock16-2t` drives all 8 board fans by `max(GPU,CPU,board)`; releases BMC auto on exit;
  never leaves fans claimed-but-undutied.
- Read-only status web page shows live readings, per-instance health, errors.
- Idle RSS single-digit MB; lean binaries. `cargo clippy` clean; `cargo fmt` applied.
- systemd `aiolos.service`; installs to `/opt/aiolos/{bin,etc}`; C `nvfd` kept running until cutover.

## Analysis
Sources checked: DESIGN.md; the four specs; session findings recorded in `/opt/nvfd/TODO.md`
(IPMI command sequence, fan roles, fail-safe).
Current state: greenfield repo (this bootstrap). C `nvfd` is in production cooling the GPUs.
Risks: chassis fail-safe (whole-system cooling) if asrock module mishandles release; NVML
fork-safety; clean cutover from nvfd without a cooling gap.

## Pre-Implementation Gate
Status: blocked (fill before moving to current/in-progress)
- Problem/root-cause model: greenfield build — N/A (new capability, not a defect).
- Evidence reviewed: DESIGN.md, four specs, `/opt/nvfd/TODO.md` findings.
- Affected contracts: the protocol spec (authoritative); `/opt/aiolos` layout; systemd; registry.
- Patterns to reuse: existing C nvfd curve/IPMI logic as reference; NVML usage patterns.
- Risk/blast-radius: production GPUs/CPUs cooling — keep C nvfd until aiolos validated on hardware.
- Sensitive-data plan: BMC IP/creds stay out of committed artifacts (operator config / *.local.md).
- Implementation plan (ordered): see Plan.
- Validation plan: see SOW-0002 (testing).
- Artifact-impact plan: AGENTS.md (commands), specs (keep current), project skills (protocol/create-anemos).
- Open decisions: parameter defaults below — confirm with user before/at gate fill.

## Implications And Decisions
Open parameter decisions (defaults proposed; confirm before gate):
1. Heartbeat `tick` / `timeout`: **3 s / 2 s**.
2. `detect_every`: **10 s**.
3. asrock fan model: **uniform** curve(max) over all 8 (per-fan optional later).
4. Curves: nvidia 0–80→0–100; asrock `{40:40,55:60,65:80,75:100}`.
5. asrock sensor set for max: GPU(inputs)+CPU+MB+card-side+DIMM (exclude TEMP_LAN?).
6. asrock IPMI binding: raw `/dev/ipmi0` ioctl (preferred) vs libfreeipmi FFI.

## Plan
1. Cargo workspace: `aiolos/` (orchestrator), `anemoi/nvidia/`, `anemoi/asrock16-2t/`. Shared
   protocol types crate (serde structs for the messages).
2. Orchestrator: registry parse → spawn detect → reconcile run instances → heartbeat fan-out +
   poll/timeout → blackboard + `input=` routing → SIGTERM shutdown. Then the status web page.
3. `nvidia` anemos: detect by UUID; run = NVML read temp + curve + set fans + report; fail-safe.
4. `asrock16-2t` anemos: detect=board; run = read sensors + max(inputs) + curve + all-manual
   `0xd6` + report; release on exit. Implement IPMI access (decision 6).
5. Packaging: `systemd/aiolos.service`, `packaging/install.sh` + `update.sh`, default
   `etc/*.conf`/`*.curve.json`.
6. Cutover (separate step, user-gated): stop C `nvfd`, enable `aiolos`, verify on hardware.

## Execution Log
(none yet)

## Validation
See SOW-0002. This SOW closes only when SOW-0002's validation passes and on-hardware cutover is
confirmed (or explicitly deferred by the user).

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
- Optional per-fan asrock curves (CPU vs case) if uniform proves insufficient.
- Additional anemoi (nvme, power-cap, alerting) — separate SOWs.

## Regression Log
None yet.
