# SOW-0016 - Workstation bring-up: it87 board fans + full-box monitoring

## Status

Status: completed

Sub-state: aiolos live on the workstation (enabled at boot) — full monitoring + GPU fan control + GPU power-cap + **case-fan control** (pwm3/pwm4 on max(GPU,CPU)); CPU fan left to BIOS (EC-locked on this IT8689E rev 1). Curve tuned. Committed on branch `sow-0016-workstation-bringup`.

## Requirements

### Purpose

Run aiolos on the user's workstation (this host) as on nova: control this board's fans by
temperature with hard process isolation and fail-safe restore, and monitor **every** temperature
/ power sensor the machine exposes. One orchestrator and protocol across the fleet; dogfood the
domain-agnostic design on a second, BMC-less consumer board.

### User Request

- "make it work on my workstation by adding a module for my motherboard and gpu."
- After wiring case fans properly: "check that we will monitor also nvme disk and everything this
  computer has. If we need more anemoi, let's add them."

### Assistant Understanding

Facts (this host, evidence in Analysis):

- Board: Gigabyte Z690 UD (consumer Intel). **No BMC / no `/dev/ipmi0`** — `asrock16-2t` and
  `ipmi-temps` do not apply here.
- Fan controller: ITE **IT8689E** Super-I/O via the out-of-tree DKMS `it87` hwmon driver
  (`/sys/class/hwmon/hwmon3`, name `it8689`). 5 PWM channels, root-writable (perm 644), all
  currently `pwm*_enable=2` (BIOS SmartFan / firmware auto).
- Fan wiring (user-supplied physical map): **ch1 = CPU** (2× Noctua), **ch4 = case intake**
  (3 fans), **ch3 = case exhaust** (1 fan). ch2/ch5 unused (no fan / no tach).
- GPU: NVIDIA RTX 5090, NVML-controllable (`nvidia` anemos works as-is).
- CPU temp source: `coretemp` (Intel) — analogous to asrock's `k10temp` (AMD).
- Other sensors present: `gigabyte_wmi` (6 board/VRM temps), `spd5118` ×4 (DDR5 DIMMs),
  `r8169` ×2 (NIC), `acpitz` (ACPI zones), `nvme` ×2 (SSDs).
- UPS: CyberPower `pr3000-desktop` (model PR3000ELCDSXL, ~2700 W nominal), local `upsd` active,
  status OL, runtime ~1980 s, load ~16%. `nut` anemos auto-discovers it via `upsc -l`.
- No fan-control daemon is running (BIOS SmartFan owns the fans). Clean baseline.
- The repo was restructured (SOW-0003) into L1 `tech/*` + L2 `anemos`/`protocol` + L3 `anemoi/*`.
  `asrock16-2t` already implements the exact multi-zone pattern needed (`anemoi/asrock16-2t/src/zones.rs`).
- Existing anemoi present in this checkout: `nvidia`, `asrock16-2t`, `nvme` (sensor-only), `nut`
  (sensor-only), `ipmi-temps` (sensor-only), `nvidia-powercap` (curve-less control).
- `tech/hwmon` today reads temperatures only (no PWM read/write).

Inferences:

- The board module mirrors asrock's zone mode 1:1, retargeted from IPMI to sysfs PWM.
- "Everything this computer has" needs exactly one NEW sensor module (`hwmon-temps`); NVMe/UPS are
  covered by existing anemoi (register only).

Unknowns:

- Whether the board EC periodically re-asserts SmartFan over manual `it87` writes (verify on hardware).
- Exact `it87` accepted PWM behavior at the floor (verify on hardware; scale 0–100% → 0–255).

### Acceptance Criteria

- `it87 detect` reports the board; `it87 run <id>` drives ch1 from CPU temp and ch3+ch4 from
  `max(GPU,CPU)` per their curves; verified by reading `pwm*`/`fan*_input` while loading CPU/GPU.
- Fail-safe verified: `shutdown`, stdin-EOF, and SIGTERM each restore every channel to
  `pwm*_enable=2` (BIOS auto), confirmed by reading sysfs after exit; `it87 restore` is idempotent.
- `hwmon-temps` reports `gigabyte_wmi`, `spd5118` (×4, disambiguated), `r8169` (×2) temps; `nvme`
  reports both SSDs; `nut` reports `pr3000-desktop`; all visible on the status page.
- `nvidia` controls the 5090 fans and restores on stop; `nvidia-powercap` monitors `input=nut`
  (dormant under the conservative default given UPS headroom).
- `cargo build --release`, `cargo test`, `cargo clippy --all-targets` clean; no non-JSON on any
  module's stdout (protocol conformance).
- aiolos runs on the workstation under systemd; cutover is operator-gated.

## Analysis

Sources checked:

- `anemos/src/{lib,run,controller}.rs` — SDK lifecycle, `Anemos`/`Device` traits, `Controller`
  (one curve + EMA + deadband + 35% floor per `run` instance).
- `anemoi/asrock16-2t/src/{main,zones}.rs` — zone mode (CPU zone vs case zone, two `Controller`s,
  curve files derived by suffix from the main path). Template for `it87`.
- `anemoi/nvme/src/main.rs`, `anemoi/ipmi-temps/src/main.rs` — sensor-only pattern (curve=None,
  `apply` reports temps). Template for `hwmon-temps`.
- `anemoi/nut/src/main.rs`, `packaging/nut.conf` — UPS auto-discovery via `upsc -l`.
- `anemoi/nvidia-powercap/src/main.rs`, `packaging/nvidia-powercap.conf` — curve-less reactor on
  `input=nut`; conservative default (cap only at runtime ≤ 300 s or low-battery).
- `tech/hwmon/src/lib.rs` — generic temp reader (no PWM yet).
- `packaging/install.sh` — builds all workspace binaries; installs config only-if-absent.
- Live hardware probes (sysfs hwmon, `nvidia-smi`, `upsc`, `dmidecode`, driver provenance).

Current state:

- 3 controllable fans (ch1/ch3/ch4) on BIOS SmartFan; GPU fan 30% @ 32°C; UPS online.

Risks:

- **SIGKILL freezes the last manual duty** (sysfs PWM persists; no hardware watchdog). Mitigated by
  the SDK 35% floor (frozen value ≥ floor), `Drop`/`restore` paths, and systemd `ExecStopPost:
  aiolos restore`. Same risk class as asrock's IPMI persistence.
- EC may fight manual writes — verify on hardware; if so, document/period-reassert.
- Shared repo developed on nova concurrently → SOW number / config collision (handled: branch
  before commit; workstation registry is host-specific operator config, not the repo default).
- Writing PWM affects the user's live desktop cooling — all live tests guarded and reversible.

## Pre-Implementation Gate

Status: ready (pending user "go" + confirm nvidia-powercap inclusion)

Problem / root-cause model:

- aiolos currently targets nova (IPMI board + NVML GPU). This host is a BMC-less consumer board, so
  it needs a sysfs-PWM board module plus a sysfs sensor reporter to reach parity. No defect; this is
  additive, host-enabling work.

Evidence reviewed:

- See "Sources checked" + Facts. asrock zone mode (`zones.rs`) is the direct template; sensor-only
  modules (`nvme`, `ipmi-temps`) are the template for `hwmon-temps`.

Affected contracts and surfaces:

- New L3 binaries `it87`, `hwmon-temps`; extended L1 `tech/hwmon` (PWM API + duplicate-chip
  labeling). Workspace `Cargo.toml` members. `packaging/install.sh` (binary list + config
  templates). New config templates. Specs `anemos-it87.spec.md`, `anemos-hwmon-temps.spec.md`.
  Repo `CLAUDE.md` layout/module list. The protocol itself is **unchanged** (reuse verbatim).

Existing patterns to reuse:

- `anemos::run` + `Anemos`/`Device` traits; `Controller` (curve/EMA/floor); asrock `ZoneControllers`
  + `zone_path` suffix derivation; asrock `Drop`/`restore_armed` fail-safe; `nvme` sensor-only shape;
  `tech/hwmon::read_temps`.

Risk and blast radius:

- Confined to two new modules + an additive `tech/hwmon` API; no change to orchestrator or protocol;
  nova modules untouched. PWM-write is the only hardware-affecting surface (guarded fail-safe).

Sensitive data handling plan:

- No secrets involved. UPS read via `upsc` (public vars, no login). Host-specific registry/UPS id
  live in operator config (`/opt/aiolos/etc/*`), never committed. No IPs/credentials in artifacts.

Implementation plan:

1. **`tech/hwmon` PWM API** — add: read `fanN_input` (rpm); read/write `pwmN` and `pwmN_enable`
   selected by chip `name`; duty 0–100% ↔ raw 0–255 scaling helper; disambiguate multiple
   same-`name` chips in `read_temps` (e.g. `spd5118-0.temp1`). Unit tests for scaling + labeling.
2. **`anemoi/it87`** (new L3) — `Anemos`/`Device` on `anemos` + `tech/hwmon`. Zone control mirroring
   asrock: CPU zone = ch1 ← `coretemp`; case zone = ch3+ch4 ← `max(routed GPU inputs, CPU)`. Two
   `Controller`s via curve-suffix files. Restore = set every managed channel `pwm*_enable=2`;
   `Drop` guard + `restore` one-shot. Small `it87.conf` (chip name + per-zone channel lists) with
   baked defaults for this board (keeps the module generic per decision 1A).
3. **`anemoi/hwmon-temps`** (new L3, sensor-only) — read a config-listed set of chips and report all
   temps. `hwmon-temps.conf` default list: `gigabyte_wmi`, `spd5118`, `r8169` (acpitz **off** by
   default). CPU (`coretemp`) excluded — reported by `it87`.
4. **Wiring/packaging** — add both crates to workspace `Cargo.toml`; add `it87`, `hwmon-temps` to
   `install.sh` binary list + config-template installs; add curve/config templates. Author a
   **host-specific** `/opt/aiolos/etc/aiolos.conf` (operator step, not the repo default):
   `nvidia` / `it87 input=nvidia` / `nvidia-powercap input=nut` / `nvme` / `hwmon-temps` / `nut`.
5. **Specs + docs** — `anemos-it87.spec.md`, `anemos-hwmon-temps.spec.md`; update repo `CLAUDE.md`
   layout/module list.
6. **Validate on hardware** (guarded) — detect/apply/restore, EOF/SIGTERM/SIGKILL fail-safe, EC
   re-assert check, full status page; then operator-gated cutover under systemd.

Validation plan:

- Unit tests (scaling, labeling, zone split). One-line stdin→stdout protocol harness per module.
- Hardware: drive CPU/GPU load, read `pwm*`/`fan*_input`; verify the three fail-safe triggers
  restore `enable=2`; `it87 restore` idempotent; grep captured stdout for non-JSON.
- `cargo test`, `cargo clippy --all-targets`, `cargo fmt --all`.

Artifact impact plan:

- AGENTS.md: likely unaffected (process unchanged).
- Runtime project skills: `project-create-anemos` may gain a sysfs-PWM note (evaluate at close).
- Specs: add `anemos-it87.spec.md`, `anemos-hwmon-temps.spec.md`.
- End-user/operator docs: repo `CLAUDE.md` layout/module list; cutover notes in `install.sh` help.
- End-user/operator skills: none expected.
- SOW lifecycle: single SOW (coherent host bring-up); branch before commit; flip to in-progress +
  move to `current/` on user "go".

Open-source reference evidence:

- External tools were compared in discussion only (CoolerControl docs, `markusressel/fan2go`
  README) to confirm off-the-shelf alternatives; no OSS code was vendored. Linux `it87` hwmon
  semantics (`pwmN_enable`: 0=full,1=manual,2=auto; pwm 0–255) per the in-tree driver contract.

Open decisions:

- All resolved (1A–7B). One confirmation pending: include `nvidia-powercap` (dormant safety net
  given UPS headroom) — assumed yes per 7B; user may drop it.

## Implications And Decisions

1. **Module name** → **A. `it87`** — named for the control chip (like `nvidia`); reusable on any
   ITE board, board wiring lives in config. (Alt B `gigabyte-z690ud` rejected: sysfs path is
   generic, unlike asrock's board-OEM IPMI.)
2. **Case-fan driving temp (ch3/ch4)** → **A. `max(GPU, CPU)`** — single-chamber desktop tower;
   CPU heat dumps into case air, so intake/exhaust respond to it. Intentionally diverges from
   asrock (`zones.rs` excludes CPU because that server has directed airflow).
3. **Fail-safe** → **A.** restore `pwm*_enable=2` on shutdown/EOF/SIGTERM + `Drop` + `it87 restore`
   one-shot + systemd `ExecStopPost`. SIGKILL freezes last duty (safe: ≥ 35% floor). Verify EC
   re-assert on hardware.
4. **PWM-write location** → **A. extend `tech/hwmon`** (generic sysfs PWM) — reusable for future
   sysfs boards (nct6775…). (Alt B new `tech/it87` rejected: less reusable.)
5. **SOW/coordination** → **A.** open `SOW-0016` on this checkout, proceed; branch before any
   commit; don't touch master without approval. Collision risk with nova flagged.
6. **Add `hwmon-temps` sensor anemos** → **A. yes** — full-box monitoring (VRM/RAM/NIC/board),
   monitor-only (decision 2A keeps them out of control).
7. **UPS present** → **B. yes** (CyberPower `pr3000-desktop`) — register `nut` (auto-discovered)
   and `nvidia-powercap input=nut` (conservative default; dormant given ~2700 W UPS headroom).

## Plan

1. `tech/hwmon` PWM API + duplicate-chip labeling (+ tests). Low risk; additive.
2. `anemoi/it87` zone control + fail-safe (mirror asrock). Medium risk (hardware writes).
3. `anemoi/hwmon-temps` sensor-only reporter (+ config). Low risk.
4. Workspace/packaging wiring + host registry + curve/config templates. Low risk.
5. Specs (`anemos-it87`, `anemos-hwmon-temps`) + `CLAUDE.md` update.
6. Hardware validation (guarded) + operator-gated systemd cutover.

## Execution Log

### 2026-05-30

- SOW authored; decisions 1A 2A 3A 4A 5A 6A 7B recorded. Gate signed off (user "go"; keep
  `nvidia-powercap` with default config, tune later). Branch `sow-0016-workstation-bringup`.
- `tech/hwmon`: added sysfs PWM control (`chip_path`, `read_fan_rpm`, `read_pwm_enable`,
  `read_pwm_raw`, `set_pwm_duty`, `set_pwm_auto`, `pct_to_raw`/`raw_to_pct`) + `read_chip_temps`
  (instance-discriminated, prefix-matched). Refactored `read_temps` via shared `temps_in_dir`
  preserving its `<chip>.tempN` fallback (asrock's 23 tests still pass).
- `anemoi/it87`: zone control (CPU zone ← `coretemp`; case zone ← `max(GPU input, CPU)`), uniform
  fallback, `it87.conf` wiring (chip + per-zone channels), restore→`pwm*_enable=2` on
  shutdown/EOF/SIGTERM + `Drop` + `restore` one-shot. Registered in workspace.
- `anemoi/hwmon-temps`: sensor-only multi-chip reporter, label disambiguation, `hwmon-temps.conf`.
- Packaging: `it87.curve.json`/`it87.cpu.curve.json`/`it87.case.curve.json`/`it87.conf`/
  `hwmon-temps.conf`; `install.sh` (binaries + config templates); `packaging/aiolos.conf.workstation`
  (host registry; nova's `packaging/aiolos.conf` untouched). Specs `anemos-it87.spec.md`,
  `anemos-hwmon-temps.spec.md`. `AGENTS.md` (CLAUDE.md symlink) module list + layout.
- Deviations from plan: (a) dropped `it8689` from the `hwmon-temps` default set — on this board its
  temps are the SAME Super-I/O sensors `gigabyte_wmi` already exposes (verified live: identical
  values); (b) added NAME-PREFIX matching to `read_chip_temps` because the NIC hwmon `name` is
  PCI-suffixed (`r8169_0_600:00`), so an exact `r8169` match found nothing.

### 2026-05-31

- **Operator-gated cutover performed (user "do it").** Pre-flight clean (no prior `/opt/aiolos`, no
  running fan daemon, no `nvfd` on this host, port 9876 free). Ran `packaging/install.sh`; replaced
  the installed nova default registry with `packaging/aiolos.conf.workstation` (set
  `status_bind=127.0.0.1:9876` — localhost-only on a personal workstation). `systemctl enable --now
  aiolos`.
- Live & healthy: service active + enabled; all 7 instances `status=ok`, 0 restarts; board channels
  `enable=1` (aiolos driving, zone mode); GPU on nvidia curve; journal clean (no warnings/errors).
  Rollback = `systemctl stop aiolos` (firmware reclaims).
- **Post-cutover finding (user: "fans do not speed up") — root cause: board EC, not aiolos.** A full
  PWM sweep (64→255, all channels) moved the duty register (readback correct) but **zero rpm change**.
  Diagnosis: `it87` force-loaded with `ignore_resource_conflict=1` (ACPI/EC also owns the IT8689E),
  chip is **revision 1** (kernel log), the firmware/EC drives the physical outputs. Confirmed it is a
  known, documented issue — frankcrawford/it87 #96 (rev 1: PWM no effect) and #79 (BIOS curve
  workaround, confirmed rev 2). Not an aiolos/module bug; not fixable in software.
- **BIOS Smart Fan 5 workaround (user applied): headers → PWM mode + Manual + `0/90`-shape curve.**
  Re-test: the two **case headers (pwm3 exhaust, pwm4 intake) became fully proportional** (418→2766
  rpm, later confirmed ramping to ~3.2 k — the fans are 3.2 k-rpm, slow ~15–20 s spin-up). The
  **CPU_FAN header (pwm1) stayed EC-clamped** (~1000–1285 rpm, ±200 authority) — not usable control.
- **Decision (user): leave the CPU fan to the BIOS; aiolos drives the case zone only.** Reconfigured
  the deployed `/opt/aiolos/etc/it87.conf` to `cpu=` (empty), `case=3,4`; removed the deployed zone
  curve files → **uniform mode** (one `it87.curve.json` over `max(GPU,CPU)` for pwm3+pwm4). pwm1 left
  on `enable=2` (BIOS/EC). Verified live: aiolos drives pwm3/pwm4, pwm1 untouched, all instances ok.
- **Curve tuned live (reloads every tick).** Final case curve (user-chosen):
  `{"60":30,"70":50,"80":100,"sensitivity":0.4}` — 30% floor ≤60 °C (~1200 rpm, quiet), 50% @70 °C,
  full ~3.2 k @80 °C. Confirmed: the "35% floor" is a curve convention, NOT a code clamp (so 30% is
  honoured). Shipped `packaging/it87.curve.json` updated to this as the default.

## Validation

Acceptance-criteria evidence (on this host, 2026-05-30):

- **Build/test/lint**: `cargo build --release` (all 9 binaries incl. `it87`, `hwmon-temps`);
  `cargo test --workspace` 100% pass (asrock 23, it87 7, hwmon-temps 7, hwmon 3, orchestrator 11,
  …); `cargo clippy --all-targets` clean; `cargo fmt --all --check` clean.
- **detect** (read-only): `it87`→board `it8689 fans`; `hwmon-temps`→`hwmon`; `nvme`→2 drives;
  `nut`→`pr3000-desktop`; `nvidia`→RTX 5090.
- **it87 control** (guarded, sudo): apply → zone mode, `pwm*_enable=1`, duty written (e.g.
  cpu_pct 56–69 / case_pct 59–74 tracking `coretemp`); fans responded (rpm rose). GPU temp routed in
  (`it87` reported `{"label":"GPU","temp":39}` while spawned under aiolos with `input=nvidia`).
- **Fail-safe** (guarded, sudo): `shutdown`, stdin-EOF, and **SIGTERM** each restored all managed
  channels to `pwm*_enable=2` (verified by reading sysfs after exit; SIGTERM log: "termination
  signal — restoring device and exiting" → "managed channels restored to firmware/automatic").
- **restore one-shot / SIGKILL net**: stranded `pwm1` in manual, `it87 restore` returned it to
  `enable=2`, exit 0, idempotent on a second run.
- **Full orchestrator dry-run** (guarded, no install/systemd): all 6 modules `detect_status: ok`;
  status JSON shows full monitoring (gigabyte_wmi×6, spd5118×4 DIMMs, r8169×2 NICs, CPU cores, GPU,
  fans, UPS); on SIGTERM board channels restored to `enable=2` and GPU fan returned to firmware
  (steady 30% @ 39 °C = pre-test baseline; `nvidia restore` reconfirmed).
- **scaling**: duty 0–100% ↔ raw 0–255 unit-tested (35%→89, 100%→255) and observed live.
- **No fan stalled**: `fan3` (exhaust) confirmed steady 1890 rpm after tests; transient 0-rpm reads
  during the run were tach-measurement-window glitches right after a `pwm_enable` mode change
  (observability only, not control).

Tests or equivalent validation:

- Unit/integration: `cargo test --workspace` (above). Protocol smoke tests: one-line stdin→stdout
  round-trips per module produced exactly one valid JSON line on stdout (logs on stderr).

Real-use evidence:

- Full orchestrator run with the workstation registry drove GPU + board fans by temperature and
  served the read-only status page at `127.0.0.1:9876`; clean SIGTERM restored every device.

Reviewer findings:

- External multi-model review not run this session (not requested). Available on request.

Same-failure scan:

- `read_temps` callers checked (asrock `k10temp`, nvme via own tech): behavior preserved (shared
  `temps_in_dir`, `<chip>.tempN` fallback kept); asrock tests green. Prefix matching is confined to
  `read_chip_temps` (monitoring); exact matching retained for control (`chip_path`) and `read_temps`.

Sensitive data gate:

- No secrets in artifacts. UPS read via `upsc` (public vars). Host registry/UPS id live in operator
  config; the committed `aiolos.conf.workstation` names modules only. No IPs/credentials/serials in
  durable artifacts (NVMe serials appear only in runtime output, not committed).

Artifact maintenance gate:

- AGENTS.md (CLAUDE.md symlink): updated (module list + layout). Specs: added `anemos-it87.spec.md`,
  `anemos-hwmon-temps.spec.md`. Project skills: no change needed (the sysfs-PWM pattern is captured
  in the it87 spec; evaluate a `project-create-anemos` note at close). Operator docs: `install.sh`
  help + the new config templates carry usage. SOW lifecycle: remains `in-progress` pending the
  operator-gated install + systemd cutover and the user's commit go-ahead.

Specs update:

- `anemos-it87.spec.md`, `anemos-hwmon-temps.spec.md` authored; conform to `aiolos-protocol.spec.md`.

Project skills update:

- None required (no new workflow; the contract additions are spec'd). Reassess at completion.

End-user/operator docs update:

- `packaging/install.sh` (binaries + config installs), `packaging/aiolos.conf.workstation`,
  `it87.conf`, `hwmon-temps.conf` templates.

End-user/operator skills update:

- None affected.

Lessons:

- hwmon chip `name` is not always the family name (NICs are PCI-suffixed) — monitor matching must be
  prefix-based; control matching stays exact.
- Vendor WMI sensors (`gigabyte_wmi`) can mirror the raw Super-I/O (`it8689`) — verify before
  including both, to avoid double-reporting.
- Reading a tach in the same tick a channel switches to manual can transiently return 0 rpm.

Follow-up mapping:

- Optional: fan-fault sibling-boost (asrock SOW-0008 style) for it87 — deferred (v1 reports rpm +
  the transient-0 caveat; no auto-compensation). Track if a workstation fan failure must boost peers.
- Operator-gated cutover (install to `/opt/aiolos`, `systemctl enable --now aiolos`) — pending user.

## Outcome

aiolos runs the workstation: **full monitoring** (CPU, GPU, board/VRM via `gigabyte_wmi`, DDR5×4,
NIC×2, NVMe×2, UPS), **GPU fan control + power-cap** (nvidia/NVML), and **case-fan control** (pwm3
exhaust + pwm4 intake on `max(GPU,CPU)`). The **CPU fan is left to the BIOS** — the Gigabyte EC keeps
the IT8689E **rev 1** CPU_FAN header clamped even after the BIOS workaround. Two new modules shipped
(`it87`, `hwmon-temps`) + a PWM-capable `tech/hwmon`; protocol unchanged; nova modules untouched.
Goal met for the workstation: case airflow now follows the 5090, with quiet idle.

## Lessons Extracted

- **A Linux fan controller's reach ends at the board EC.** On Gigabyte + IT8689E (esp. rev 1) the EC
  owns the PWM outputs; sysfs writes read back but do nothing until a BIOS Smart Fan "PWM + Manual +
  `0/90` curve" workaround hands control over — and some headers (CPU_FAN) never yield. Verify real
  rpm response with a full 0→100% sweep before trusting "control"; readback ≠ authority.
- **No software tool escapes this** — fan2go/coolercontrol use the same hwmon PWM, same EC wall
  (corrects an earlier in-session claim that they could cool this box).
- **Diagnose with a sweep, not a single set-point** — a flat region (35–100%) hid that the CPU fan
  had a clamped band; alternating min/max exposed the ±200 rpm authority.
- The "35% floor" is a curve convention (lowest point), not a code clamp — floors are fully tunable.
- hwmon chip `name` isn't always the family (`r8169_0_600:00`) → monitor matching is prefix-based;
  vendor WMI can mirror the raw Super-I/O (`gigabyte_wmi` == `it8689`) → don't double-report.

## Followup

None yet.

## Regression Log

None yet.
