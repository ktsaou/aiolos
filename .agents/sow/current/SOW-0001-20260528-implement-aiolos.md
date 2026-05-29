# SOW-0001 - Implement aiolos (orchestrator + nvidia & asrock16-2t anemoi)

## Status

Status: in-progress
Sub-state: clean rebuild underway. A prior implementation was reviewed (2026-05-29) and found
unfit: both device layers were stubs (nvidia NVML returned fake values / empty detect; asrock
IPMI used a wrong `/dev/ipmi0` ioctl ABI and a hardcoded fake response), plus several
orchestrator robustness bugs (partial-line read defeating the timeout, blackboard never pruned,
stderr-tail registration race, config not actually parsed, status page showing only a readings
count, systemd discarding all logs, optional `hello` unhandled). Re-authoring all crates; reusing
only personally-verified-correct logic (protocol types, registry parser, curve interpolation) as
reference. See Execution Log.

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
Status: passed (all items resolved — decisions confirmed above)
- Problem/root-cause model: greenfield build — N/A (new capability, not a defect).
- Evidence reviewed: DESIGN.md, four specs, `/opt/nvfd/TODO.md` findings.
- Affected contracts: the protocol spec (authoritative); `/opt/aiolos` layout; systemd; registry.
- Patterns to reuse: existing C nvfd curve/IPMI logic as reference; NVML usage patterns.
- Risk/blast-radius: production GPUs/CPUs cooling — keep C nvfd until aiolos validated on hardware.
- Sensitive-data plan: BMC IP/creds stay out of committed artifacts (operator config / *.local.md).
- Implementation plan (ordered): see Plan.
- Validation plan: see SOW-0002 (testing).
- Artifact-impact plan: AGENTS.md (commands), specs (keep current), project skills (protocol/create-anemos).
- Open decisions: all resolved — see §Implications And Decisions above.

## Implications And Decisions
Confirmed decisions (recorded before gate fill):
1. Heartbeat `tick` / `timeout`: **3 s / 2 s**.
2. `detect_every`: **10 s**.
3. asrock fan model: **uniform** curve(max) over all 8 (per-fan optional later).
4. Curves: **linear 0→0%, 80→100%** for both nvidia and asrock (asrock: `{"0":0,"80":100}`).
5. asrock sensor set for max: **own sensors (all) + nvidia inputs** (GPU temps routed via `input=nvidia`).
6. asrock IPMI binding: **raw `/dev/ipmi0` ioctl** (zero extra deps, exact bytes known).
7. Status web page bind: **configurable** via `aiolos.conf` (`status_bind=`); **default
   `0.0.0.0:9876`** (user-confirmed 2026-05-29 — matches other nova services reachable as
   `nova:PORT`; the page is read-only telemetry with no secrets). `127.0.0.1` available via config.
8. Workspace layout: **Option A — cargo workspace** with shared `protocol` lib crate.
   Members: `["protocol","aiolos","anemoi/nvidia","anemoi/asrock16-2t"]`.
   Rationale: shared protocol types as single source of truth; one build/test/clippy cmd;
   matches DESIGN.md layout; runtime isolation unaffected.
9. asrock16-2t fail-safe when temperature is indeterminable (all own sensor reads fail AND no
   GPU `inputs` available): **release to BMC auto** (`0x3a 0xd8` ×16 `0x00`) and report
   `status:error` — never hold manual control while blind (user-confirmed 2026-05-29). The
   per-tick duty bytes are also always clamped non-zero to avoid the `0xcc` claimed-but-undutied
   trap when a valid-but-low temperature yields 0%.
10. Implementation approach: **clean rebuild** (treat as from scratch). Re-author every crate;
    reuse only personally-verified-correct logic (protocol types, registry parser, curve
    interpolation) as reference. Device layers implemented for real: nvidia via NVML (binding TBD
    pending research — `nvml-wrapper` vs raw FFI, modeled on the working C `nvfd`), asrock via the
    correct `/dev/ipmi0` raw ioctl ABI (decision 6). The SOW-0002 mock-anemos + orchestrator
    integration tests are built as part of this work (no-hardware subset).
11. NVML binding (research-confirmed 2026-05-29): **`nvml-wrapper` 0.12.1** — all needed methods
    present since 0.11.0 (`set_fan_speed`, `set_default_fan_speed`, `fan_speed`, `fan_speed_rpm`,
    `num_fans`, `temperature`, `uuid`, `name`); Blackwell + CUDA 13.0 supported; loads
    `libnvidia-ml.so.1` at runtime (no link-time dep, no raw FFI). **Manual fan control persists
    after process exit** (driver does NOT auto-revert) — restore-on-shutdown/EOF is a hard
    fail-safe (per-fan loop `0..num_fans`). SIGKILL cannot restore in-process (same gap as the C
    `nvfd`); a systemd `ExecStopPost`/watchdog belt-and-suspenders is a follow-up consideration.
    Stable id = GPU UUID.

## Plan (clean rebuild — supersedes the prior implementation)
1. Workspace hygiene: root `Cargo.toml` — drop unused `tokio`/`once_cell` (no async per project
   override); keep `serde`/`serde_json`/`anyhow`/`tracing`/`libc`. Prune unused per-crate deps.
2. `protocol` crate: serde wire types. Fixes: `apply.inputs` omitted when absent
   (`skip_serializing_if`), tolerate/parse the optional `hello`. Keep round-trip tests + add
   malformed-input and hello cases.
3. `registry` + `config`: parse `aiolos.conf` for the registry AND real `key=value` globals
   (`tick=`, `timeout=`, `detect_every=`, `status_bind=`) with the confirmed defaults
   (3/2/10, `0.0.0.0:9876`). timeout<tick invariant enforced.
4. Orchestrator core, fixing every reviewed bug:
   - Framed reads that respect the timeout WITHOUT blocking on a partial line (read bytes with a
     deadline; never call a blocking `read_line` after `poll`). A module that writes a partial
     line or floods stdout is killed within ~timeout — preserves the isolation guarantee.
   - Prune the blackboard when an instance is removed/dies (no stale `input=` relay).
   - Eliminate the stderr-tail registration race (supervisor owns the entry; tail handle created
     before the entry is published).
   - Bounded shutdown (per-instance shutdown deadline; SIGKILL fallback; no unbounded `child.wait`).
   - Handle the optional `hello` line in both detect and run paths.
   - Status page renders ACTUAL readings (temps/pwm/rpm), per-instance health, errors, stderr tail.
5. `asrock16-2t`: detect=board; run = read own sensors (k10temp both sockets + IPMI SDR temps) +
   `max(inputs, own)` + curve + claim-once + `0xd6` all-manual non-zero duty + `0xda` readback +
   report. Correct `/dev/ipmi0` raw ioctl ABI (research-verified: SEND `0x8028690D` → poll →
   RECV_TRUNC `0xC030690B`; `repr(C)` structs sized 16/40/48 with `const` asserts; completion code
   = `data[0]`). Fail-safe: release to BMC auto on shutdown/EOF AND when temp indeterminable
   (decision 9); per-tick duty clamped non-zero.
6. `nvidia`: detect by UUID; run = NVML read temp + curve + set fan % + readback + report;
   restore default fan control on shutdown/EOF. NVML binding chosen per in-flight research
   (`nvml-wrapper` vs raw FFI to libnvidia-ml, modeled on the working C `nvfd`).
7. Tests (SOW-0002 no-hardware subset, built here): a `mock` anemos with injectable behaviors
   (hang, partial-line flood, error, detect-set change, stderr noise) + orchestrator integration
   tests (reconcile, timeout-kill+respawn, isolation, `input=` routing, SIGTERM) + footprint check.
8. Packaging: `systemd/aiolos.service` (logs to journal, not `/dev/null`); `install.sh` that does
   NOT clobber existing `etc/` configs; `update.sh`. Default `etc/*.conf`/`*.curve.json`.
9. Cutover (separate step, user-gated): on-hardware validation, then stop C `nvfd`, enable
   `aiolos`. NOT part of this build; gated.

## Execution Log
- 2026-05-29: Reviewed the prior implementation; found both device layers were stubs and the
  orchestrator had real robustness bugs (see Status). Decided on a clean rebuild.
- 2026-05-29: Ran two read-only research agents → recorded the exact `/dev/ipmi0` ABI (SEND
  `0x8028690D`, RECV_TRUNC `0xC030690B`, async send→poll→recv, `ipmi_system_interface_addr`,
  completion code at `data[0]`) and the NVML plan (`nvml-wrapper` 0.12.1 has all setters since
  0.11.0; manual fan control persists → restore mandatory). User decisions 7/9 confirmed.
- 2026-05-29: Rebuilt all crates:
  - `protocol`: serde wire types; `apply.inputs` omitted when absent; `inputs` is
    `HashMap<id, Vec<Reading>>` (relays the peer's full readings, uninterpreted); optional `hello`
    parses distinctly; `Reading` flatten via `Map` + helpers. 10 unit tests.
  - `aiolos`: real config parsing (globals + registry, `0<timeout<tick` clamp, `tick`≥2 floor,
    env path/bin-dir overrides); supervisor with prompt sub-`detect_every` respawn + per-id
    backoff + blackboard pruning + no stderr-tail race; **deadline-bounded non-blocking stdio on
    both read AND write** (kills partial-line / stdout-flood / stuck-stdin within ~timeout);
    `hello` skipped; SIGTERM+SIGINT → bounded graceful shutdown waiting on a live-instance count;
    status page renders real readings as HTML (`/`) + JSON (`/status.json`), HTML-escaped.
  - `anemoi/asrock16-2t`: correct raw `/dev/ipmi0` ioctl (repr(C) structs sized 8/16/40/48 with
    compile-time asserts; ioctl numbers asserted against the verified values); claim/setduty
    (non-zero clamp)/release/query(0xda readback); k10temp (both sockets); fail-safe release on
    shutdown/EOF/panic AND on indeterminable temp (decision 9). 10 unit tests.
  - `anemoi/nvidia`: `nvml-wrapper` 0.12.1; detect by UUID; per-fan set/readback; restore on
    shutdown/EOF and via `Gpu::Drop` (persist-after-exit safety net); empty curve → firmware. 3 tests.
  - `mock` anemos + 5 orchestrator integration tests (real processes, marker-file assertions):
    reconcile+tick+graceful-restore, hung-sibling isolation, **partial-line-flood killed (the fix)**,
    `input=` routing, detect-set hotplug add/remove.
  - Packaging: systemd unit logs to journal (not `/dev/null`); `install.sh` builds + installs and
    never clobbers existing `etc/` config; `update.sh` restarts only if active. `run()` transparency.
- 2026-05-29: `cargo build --release`, `cargo clippy --all-targets --workspace`, `cargo fmt --all
  --check`, `cargo test --workspace` all clean — **38 tests pass** (33 unit + 5 integration).
  Release binaries: aiolos 1.3M, nvidia 1.2M, asrock16-2t 1.1M.
- 2026-05-29: **On-hardware cutover executed (user-approved) and validated on nova:**
  - Added a read-only `asrock16-2t query` (sends only `0xda`) as a safety gate. It returned 16
    sane bytes matching `ipmitool raw 0x3a 0xda` → IPMI ioctl ABI validated with zero side effects.
  - Isolated asrock write test (before involving the orchestrator): claim+setduty drove duty
    40%→46% (readback `0x2e` matched the curve for the 37°C driving temp); stdin EOF released back
    to BMC auto (exact prior value). Both EPYC sockets' k10temp sensors read correctly.
  - Installed to `/opt/aiolos` (binaries + default curves + systemd unit), `nvfd` left untouched.
  - Stopped `nvfd` (GPUs → firmware), started `aiolos`. Both GPUs detected by UUID; asrock board
    instance up; status page on `0.0.0.0:9876`; no errors; `restart_count=0`.
  - **nvidia verified:** per-GPU fans track the curve (idle 27/28°C→33/35%; under GPU load
    47-55°C→56-67%), real pwm+rpm readback, cross-checked with `nvidia-smi`.
  - **asrock verified:** board fans claimed + set to curve, independent `ipmitool 0xda` readback
    matches every sample; never the claimed-but-undutied trap.
  - **`input=nvidia` routing verified live:** under GPU load the routed GPU temp became asrock's
    driving temp and ramped all 8 board fans 50%→67-68% (GPU heat → case/CPU airflow, as designed).
  - **Fail-safe verified:** `systemctl stop aiolos` → journal "all instances shut down and devices
    restored" in ~50ms; GPUs returned to firmware, board released to BMC auto. Restarted clean.
  - **Footprint:** orchestrator RSS ~3.5 MB; asrock instances ~2.5 MB each (lean, as targeted).
    nvidia instances ~23 MB each — dominated by NVML/`libnvidia-ml` mapping (comparable to the C
    `nvfd`'s 17 MB), not orchestrator overhead.
  - **Boot enablement intentionally NOT changed** (pending user decision): `aiolos` active but
    `disabled`; `nvfd` inactive but still `enabled` → a reboot currently falls back to `nvfd`
    (a safe soak posture). Full cutover = enable aiolos + disable nvfd.

## Validation
Off-hardware (done, green):
- Acceptance via tests: protocol round-trips/omit-inputs/hello/malformed; curve interpolation;
  config clamps; IPMI ioctl-number + payload asserts; orchestrator reconcile/tick/shutdown,
  isolation (hung + partial-line-flood killed & respawned while sibling keeps ticking), `input=`
  routing, hotplug. `cargo test --workspace` is the CI-runnable subset and is green.
- Reviewer findings: the original review's defects (stub NVML/IPMI, partial-line wedge, blackboard
  staleness, stderr race, fake config, readings-count-only page, lost logs, unhandled hello) are
  each addressed and covered by a test or asserted invariant.
- Footprint: release binaries low-MB (above); idle RSS to be measured on-hardware.
- Sensitive data: no BMC IP/creds/serials in any artifact (no secrets needed; `/dev/ipmi0` is
  inband). Sanitized throughout.

Remaining (on-hardware, operator-gated — tracked in SOW-0002):
- nvidia: fan % tracks curve on the real GPUs; per-GPU isolation; shutdown/EOF restores firmware
  (read back fan policy after exit).
- asrock16-2t: all-manual+non-zero `0xd6` sets fans; `0xda` readback matches; shutdown/EOF releases
  BMC auto; never the claimed-but-undutied trap; monitored CPU-fan test.
- Idle RSS measured; cutover from C `nvfd` (stop nvfd, enable aiolos) with temps monitored.

## Outcome
Software complete and validated off-hardware; **Status stays `in-progress`** pending the
operator-gated on-hardware validation + `nvfd` cutover (do not stop `nvfd` without user approval).

## Lessons Extracted
- Stubs that answer `status:ok` are worse than errors — they look healthy while doing nothing.
  Device layers must be real or explicitly fail; tests asserting real effects (markers/readback)
  catch this.
- `poll()` readiness ≠ a complete line. Any timeout guarantee built on `poll` + blocking
  `read_line` is false; enforce the deadline across partial reads (and writes).
- NVML/IPMI both require explicit restore — neither auto-reverts on process exit; wire restore
  into every exit path incl. `Drop`/panic and an indeterminable-input fail-safe.
- Verify kernel ioctl numbers by computing+asserting them; the prior `_IOWR` vs `_IOR` mistake is
  exactly the class of bug a `const assert` catches.

## Followup
- On-hardware validation + cutover (SOW-0002).
- asrock board/DIMM IPMI SDR temps + per-fan tach RPM (currently CPU via k10temp + GPU inputs only).
- Optional per-fan asrock curves (CPU vs case) if uniform proves insufficient.
- Belt-and-suspenders restore for SIGKILL (systemd `ExecStopPost`/watchdog) — neither aiolos nor
  the C `nvfd` restores on a hard kill.
- Additional anemoi (nvme, power-cap, alerting) — separate SOWs.

## Regression Log
None yet.
