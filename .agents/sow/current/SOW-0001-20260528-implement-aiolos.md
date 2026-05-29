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
12. Fan-output damping (user-confirmed 2026-05-29, after the on-hardware "wave"): **EMA smoothing
    of the driving temperature + a duty deadband**, in BOTH modules, via a shared `protocol::Damper`.
    Root cause of the wave (confirmed by aiolos logs + GLM/MiMo/MiniMax review consensus): noisy
    CPU `Tctl` (and bursty GPU temp) fed through a steep curve with no smoothing → duty hunts every
    tick. NOT a caching bug (H1 rejected) and NOT an active BMC fight (H2 rejected by the logs:
    `0xda readback == exactly the previous tick's command` = read-after-write latency, not an
    override). Also: claim manual ONCE (sticky, self-healing on error) instead of every tick; report
    the **commanded** duty (the immediate `0xda` readback is one cycle stale). Added a dedicated
    journal namespace (`LogNamespace=aiolos`) and per-tick decision/routing logging.
14. Explicit module error reporting (user-confirmed 2026-05-29 — supersedes the fail-stop +
    empty-strike approach, which the user rightly rejected: inferring faults from exit/silence or
    empty data is fragile and leaves the supervisor deciding blind). **Protocol change:** `detect`
    and `apply` carry an explicit `status` ∈ {`ok`,`error`,`fatal`} + optional `error` reason.
    `ok` = did its job (`found`/`readings` authoritative; empty is real; an `error` field alongside
    = non-fatal warning / "done with errors"). `error` = transient, couldn't do the job (NOT "no
    devices") → supervisor keeps existing instances, surfaces the reason, retries with backoff.
    `fatal` = cannot work on this host → see decision 15. Modules MUST report errors explicitly,
    never by exiting/returning empty. Crash/timeout detection remains ONLY as a last-resort
    backstop (a module too broken to report), surfaced as "unresponsive/crashed". Removed: nvidia
    fail-stop exit + the empty-strike heuristic. Scope: BOTH detect and apply.
15. `fatal` handling (user-confirmed 2026-05-29): **long-backoff retry** — never permanently give
    up. The supervisor keeps surfacing the fatal error and retries on a long backoff (minutes) in
    case the condition clears (driver reload, device appears); it does not tear down or hammer.
13. Curves (user-confirmed 2026-05-29): nvidia and asrock use the **same** curve so the board
    tracks the hottest GPU (modulo one-tick lag + CPU max). Reset both to `{"0":0,"80":100}`;
    live-tunable (per-tick reload works). The board ≠ hottest-GPU mismatch was (a) a live edit that
    diverged the asrock curve to `{"0":0,"90":100}` and (b) the structural one-tick-stale +
    `max(GPU,CPU)` routing — both expected, not bugs.
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

## Bug — FD leak in nvidia detect (found in soak, 2026-05-29)
Symptom: after ~hours, GPU fans stopped tracking the curve (GPU1 77°C @ 48%); user asked "is
aiolos not running?". aiolos WAS running, but the journal showed the nvidia **detect** process
repeatedly logging `NVML init failed during detect: libnvidia-ml.so.1: cannot open shared object
file: Too many open files` (EMFILE).
Root cause (a bug I introduced in this rebuild): `nvidia::enumerate()` called `Nvml::init()` on
**every detect cycle (10s)**; NVML opens `/dev/nvidia*` fds that are not all released on shutdown,
so the detect process leaked fds until EMFILE. Then detect returned empty → reconcile dropped both
GPU `run` instances (vanished) → GPU fan control fell back to firmware (safe but lazy) →
undercooled vs intent. asrock simultaneously lost its routed GPU temps (`gpu_max=None`).
Not caught by unit tests or the short on-hardware validation — it only manifests after hours.
Fixes (three layers; user rejected "just raise `LimitNOFILE`" — a ceiling only delays a leak):
1. **No leak:** nvidia detect inits NVML ONCE (`nvml::init()`) and holds it; never re-inits per
   cycle. Verified: detect-process fd count flat (58→58 across 2+ cycles); no further EMFILE.
2. **Module fail-stop:** the detect process now EXITS on an NVML fault (`init()`/`enumerate()`
   error) instead of replying `{"found":[]}`. A module that answers with valid-but-wrong data
   defeats supervision (the orchestrator can't tell "empty=broken" from "empty=no devices"); by
   exiting, it triggers the supervisor's restart path. `enumerate()` returns `Err` on an NVML fault
   vs `Ok([])` for a genuinely GPU-less host.
3. **Generic supervisor resilience (orchestrator, module-agnostic):** if a `detect` returns empty
   while that module still has running instances, the supervisor **recycles the detect process**
   and keeps the instances, requiring `EMPTY_STRIKE_LIMIT=2` consecutive empties before believing
   the devices are gone. Also: a failed/dead detect leaves `last_found` unchanged, so reconcile
   does not tear down healthy instances during a detect outage.
Removed the `LimitNOFILE` band-aid. asrock holds one `/dev/ipmi0` handle for life (no analogous
leak); run instances init NVML once each (fine).
Verified live: `kill -9` of the detect process → supervisor respawned it (~14s) while the GPU run
instances stayed `ok`/`restarts=0` and kept controlling.
Lesson: (a) long-running resource leaks evade short tests — **soak testing is essential** (why
aiolos is correctly NOT boot-enabled yet, with nvfd as boot fallback); (b) **a module must
fail-stop on unrecoverable faults** so the supervisor can restart it — limping along with valid
but wrong output is worse than crashing; (c) raising an fd ceiling is not a defense. The research
even flagged per-detect NVML re-init; I wrongly judged its cost "negligible" and missed the leak.

## Smoothing + curve floor (2026-05-29, decision 16)
User reviewed EMA behaviour on a real read-spike and judged it acceptable → **keep EMA** (it is not
the wrong tool; the earlier asymmetric/SMA/median proposals were not adopted). Changes:
- **Curve floor:** default curves are now `{"35":35,"80":100}` (≤35°C→35%, ≥80°C→100%, linear),
  so manual mode **never commands below 35%** — a wrong *low* reading can't stop/minimise the fans.
  (Old `{"0":0,"80":100}` was the bug.)
- **Sensitivity knob:** the EMA α is now a live-tunable `"sensitivity"` key in each module's curve
  file (default 0.5; lower = smoother/less spike-sensitive, higher = more responsive), reloaded each
  tick via `CurveCache` and applied with `Damper::set_alpha` (no fan blip on tune).
- Safety unit tests: curve never <35% / >100% for any temp (incl. absurd lows); single-spike
  dilution ≈ α·Δ; sensitivity parse + α clamp. Deployed + verified on hardware (idle GPUs at the
  35% floor; sensitivity 0.5 loaded from config).
- Honest caveat (kept as a known property, not a blocker): symmetric EMA still lags a *sustained*
  rise by a few ticks; the 35% floor + aggressive curve bound the risk. An instant-up/average-down
  variant remains available if ever desired.

## Rework — explicit error-reporting protocol (2026-05-29, decisions 14/15)
Replaced the fail-stop/empty-strike approach (rightly rejected by the user as fragile inference)
with explicit module error reporting:
- Protocol: `Status` = {ok,error,fatal}; new `Detected{status,found,error}` for detect; `Applied`
  gains `fatal`. Contextual parsing (skip optional `hello`, parse the expected type) replaced the
  ambiguous untagged `Response`. 18 protocol unit tests.
- Modules: nvidia detect reports `error` on NVML init/enumerate fault (no exit, no empty),
  self-recovers by lazily re-initing; nvidia run reports `fatal` if NVML can't open. asrock apply
  reports `fatal` if `/dev/ipmi0` can't open. mock gained detect/apply error+fatal behaviors.
- Supervisor reacts to DECLARED status: `ok`→reconcile (empty legitimately tears down); `error`→
  keep instances + surface + recycle detect proc; `fatal`→keep instances + surface + 300s backoff.
  Crash/timeout/unresponsive remain backstops (surfaced as such); a failed detect keeps `last_found`
  so instances are never torn down on a detect outage. Per-module detect health stored in AppState.
- Status page surfaces per-module detect status+error (HTML + JSON).
- New integration test `detect_error_keeps_instances` (declared error preserves instances). Total
  **41 tests** green; clippy 0; fmt clean. Deployed + verified on hardware (status page shows
  `nvidia:ok`, `asrock16-2t:ok`; GPUs under curve control).
Lesson: supervision must be **error-driven, not inference-driven** — a module that limps along with
valid-but-wrong output (or exits/goes silent) to signal a fault leaves the supervisor deciding
blind. The protocol must let modules say *what* is wrong; crash/timeout is only the last resort.

## Rework — self-sufficient module shutdown + config-agnostic fail-safe (2026-05-29, decisions 17-20)
User rejected the first cut of the SIGKILL fail-safe as bad practice on two counts: (a) modules must
catch signals and self-restore rather than depend on the parent killing them; (b) the systemd unit
must not hardcode module names — any `ExecStopPost` restore must route through `aiolos`, which reads
its own config and calls the modules. Confirmed decisions:

17. **Modules catch SIGTERM/SIGINT and self-restore (mechanism A1).** Each anemos installs handlers
    that do ONLY an async-signal-safe atomic flag store; the `run` loop restores the device in normal
    code, then exits. Implementation: stdin set non-blocking + `poll()` with a short step (~200 ms),
    checking the flag between polls (reaction ≤ ~step). Restoring inside the handler was REJECTED
    (NVML/IPMI allocate + take locks → not async-signal-safe → deadlock risk). The signal-aware
    stdin line-reader lives in the **`protocol`** crate (adds `libc` there) so every module behaves
    identically and it is governed by the protocol contract/skill (fail-safe-on-EOF *and* on-signal).
18. **Config-agnostic fail-safe via `aiolos restore`.** New `aiolos restore` subcommand reads the
    registry from its own config and runs `<module> restore` for every configured module (bounded
    timeout, best-effort). Every anemos exposes a uniform `restore` one-shot — module CLI contract
    becomes `detect` / `run <id>` / `restore` (asrock's `release` renamed to `restore`). systemd uses
    a single `ExecStopPost=-/opt/aiolos/bin/aiolos restore` (no module names); the hardcoded per-
    module lines and `ExecStartPre` are removed (aiolos re-claims/regulates on start).
19. **KillMode = systemd default (`control-group`).** On stop, aiolos AND every module get SIGTERM;
    each module self-restores (decision 17). No `KillMode=mixed`. Simplest, agnostic, exercises the
    handlers, and does not depend on aiolos orchestration. `ExecStopPost` covers the SIGKILL-the-
    whole-cgroup case (post-mortem cleanup).
20. **Soften `Instance::Drop`.** Escalate gracefully: close stdin (EOF → module restores) → grace →
    `SIGTERM` (caught → restore) → grace → `SIGKILL` only as an absolute last resort for a wedged
    child (SIGKILL cannot be removed — it is the only guaranteed stop for a hung process — but it is
    no longer the second step).

### Execution — danger-list fixes R1-R8 + decisions 17-20 (2026-05-29)
Workstream: "conditions where fans are set to manual mode but are NOT regulated" — verify each with
a mock/unit test, then fix. All landed; `cargo test --workspace` = **59 tests**, clippy 0, fmt clean.
- **R1** Instance::Drop closes stdin (EOF→restore) before any kill. Integration test
  `alive_module_drop_restores_device_via_eof`.
- **R6** `Damper::deadband` asymmetric: increases applied immediately, only small decreases held
  (never lag a needed ramp-up). Tests `deadband_asymmetric_*`, `deadband_never_holds_an_increase`.
- **R2** asrock `regulate` policy: on a persistent duty-set failure it RELEASES to BMC auto instead
  of holding manual-but-frozen. `FanBus` trait + mock; tests `regulate_releases_to_auto_*`.
- **R3** nvidia `apply_or_restore`: a mid-loop fan-set failure reverts ALL fans to firmware default;
  `run_loop` also restores on ANY tick error (temp-read/resolve). `FanControl` trait + mock; 3 tests.
- **R7** supervisor-thread watchdog: main respawns a supervisor whose thread panicked (backoff-
  bounded, never gives up). `Supervisor::Drop` shuts down + deregisters its instances on a non-
  shutdown drop so the replacement starts clean. Pure `should_respawn` unit-tested.
- **R8** `FanRestore::restore` disarms ONLY on a successful release (failed release retried by Drop).
  `still_armed_after` unit-tested.
- **Decision 17** every anemos catches SIGTERM/SIGINT and self-restores: new
  `protocol::StdinReader` (non-blocking stdin + `poll(2)` in 200ms steps, checking an async-signal-
  safe flag; no SA_RESTART so a signal wakes the poll). Handler does only an atomic store; restore
  runs in normal code (NVML/IPMI are not async-signal-safe). nvidia + asrock + the mock all use it.
  Protocol stdio tests (lines/EOF/CRLF/partial) + integration `module_self_restores_on_sigterm`
  (stdin held open so only the signal path can produce `.restored`).
- **Decision 18** config-agnostic fail-safe: new `aiolos restore` reads the registry and runs each
  module's uniform `restore` one-shot (concurrent, bounded). asrock `release`→`restore` (CLI now
  `detect`/`run <id>`/`restore`). systemd: single `ExecStopPost=-/opt/aiolos/bin/aiolos restore`;
  removed the hardcoded per-module lines, `KillMode=mixed`, and `ExecStartPre`.
- **Decision 19** KillMode = systemd default (control-group): on stop every process gets SIGTERM and
  each module self-restores (decision 17).
- **Decision 20** `Instance::Drop` escalates EOF → SIGTERM → SIGKILL (last resort only).
- **Supersedes** the earlier per-module `ExecStopPost`/`KillMode=mixed` wiring (user rejected it as
  hardcoding config + relying on the parent to kill modules).
- **Artifacts updated**: aiolos-protocol spec (uniform `restore` subcommand + signal/EOF self-
  restore, conformance items 5-7), orchestrator spec (`aiolos restore`, watchdog, Drop escalation,
  KillMode discipline), both anemos specs (modes, fail-safe triggers, R2/R3 behavior, curve defaults
  corrected to the decision-16 35% floor), and both project skills.
- **Deployed + verified on nova (user-approved, 2026-05-29 ~13:19):** `install.sh` (new binaries +
  config-agnostic unit, configs preserved) → `systemctl restart aiolos`. The restart's stop ran the
  new **config-agnostic `aiolos restore`** ExecStopPost on real hardware — journal showed
  `all GPU fans restored to firmware default gpus=2`, `restored module=nvidia`,
  `fans released to BMC auto`, `restored module=asrock16-2t`, `restore complete` (it iterated the
  registry, naming no modules). New aiolos came up healthy: both modules `status:ok`, curve +
  **35% floor confirmed** (GPU 30 °C → 35% via nvidia-smi, matching the journal pwm), no warn/error.
  Module self-restore-on-SIGTERM (decision 17) is proven by the integration test
  `module_self_restores_on_sigterm` and will be exercised on the next real stop now that the
  signal-aware modules are live. nvfd remains inactive + boot-enabled as the fallback.

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
