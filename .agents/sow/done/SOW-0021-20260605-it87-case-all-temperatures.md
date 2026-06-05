# SOW-0021 - it87 case fans follow all routed temperatures

## Status

Status: completed

Sub-state: closed on 2026-06-05 after implementation, tests, live installation, and service
verification.

## Requirements

### Purpose

Prevent CPU/board overheating on the workstation by making the `it87` case-fan zone respond to the whole-box thermal picture, not only CPU and GPU temperatures.

### User Request

The user confirmed that `it87` should regulate fans based on all relevant temperatures after learning that the current case-zone decision ignores `hwmon-temps` motherboard/VRM/DIMM/NIC readings and NVMe readings.

### Assistant Understanding

Facts:

- `it87` currently drives the case zone from `max(GPU routed inputs, CPU coretemp)`.
- `hwmon-temps` publishes `gigabyte_wmi`, DDR5 DIMM, and NIC temperature signals.
- `nvme` publishes NVMe drive temperature signals.
- aiolos routes only configured `input=` sources into an anemos.
- Linux hwmon `temp*_input` values are millidegree Celsius and aiolos already divides by 1000; live checks showed `sysfs`, `sensors`, and aiolos agree on the motherboard temperature scale.

Inferences:

- The useful fix is not libsensors scaling. The missing behavior is source coverage in `it87`.
- The case-zone control input should be `max(CPU coretemp, all routed temperature producer values)`.
- The CPU-zone control input should remain CPU-only.
- The live workstation registry should route `nvidia`, `hwmon-temps`, and `nvme` into `it87`.

Unknowns:

- Exact physical meaning of each vendor `gigabyte_wmi` temperature label is unknown; they are unlabeled board sensors. The safe policy is to treat them as valid case-airflow thermal inputs.

### Acceptance Criteria

- `it87` case zone uses the maximum of CPU temperatures and every routed temperature producer from configured input sources.
- `it87` CPU zone remains CPU-only.
- Routed non-temperature signals and sink signals do not influence fan decisions.
- Case fan sink `driven_by` reports CPU plus per-source routed temperature maxima, so the status page shows what affected the case fans.
- Workstation config wires `it87 input=nvidia input=hwmon-temps input=nvme`.
- Live `/opt/aiolos/etc/aiolos.conf` is updated and aiolos is restarted/verified.
- Tests cover source-grouped routed temperature extraction and case-zone driving metadata.

## Analysis

Sources checked:

- `anemoi/it87/src/main.rs`
- `anemoi/hwmon-temps/src/config.rs`
- `tech/hwmon/src/lib.rs`
- `packaging/aiolos.conf.workstation`
- `/opt/aiolos/etc/aiolos.conf`
- `/opt/aiolos/etc/hwmon-temps.conf`
- `/opt/aiolos/etc/it87.conf`
- Linux hwmon sysfs documentation: `https://www.kernel.org/doc/html/latest/hwmon/sysfs-interface.html`

Current state:

- `anemoi/it87/src/main.rs` reads `input_temps_from(inputs, "nvidia")` and `hwmon::read_temps("coretemp")`.
- Case-zone raw temperature is currently `max(gpu_max, cpu_max)`.
- `hwmon-temps` deliberately reports `gigabyte_wmi`, `spd5118`, and `r8169`, but these signals are not routed to `it87` in the live config.
- `nvme` is not routed to `it87` in the live config.

Risks:

- Noise risk: one hot DIMM/NIC/NVMe/board sensor can raise case fan speed. This is intended for case airflow, but it may be louder.
- Bad-sensor risk: an invalid high sensor could drive case fans high. This is fail-safe in the cooling direction; it cannot slow fans down.
- Feedback risk: routed fan-duty/RPM signals must not feed back into case control. Mitigation: consume only producer signals whose type/kind is `temperature`.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- The case fans are responsible for whole-box airflow, but `it87` currently ignores most whole-box thermal sensors. The missing inputs are not a scaling issue; live evidence showed `sysfs`, `sensors`, and aiolos agree on temperature units. The root cause is that the case-zone control path only considers CPU and routed NVIDIA GPU temperatures.

Evidence reviewed:

- `anemoi/it87/src/main.rs`: case-zone raw temperature is currently `max(GPU, CPU)`.
- `anemoi/hwmon-temps/src/config.rs`: default monitored chips are `gigabyte_wmi`, `spd5118`, and `r8169`.
- `tech/hwmon/src/lib.rs`: reads `temp*_input` and divides by 1000.
- Live `/opt/aiolos/etc/aiolos.conf`: `it87` currently has only `input=nvidia`.
- Live sampled values: `gigabyte_wmi` values matched between raw sysfs, `sensors`, and aiolos.

Affected contracts and surfaces:

- `anemoi/it87/src/main.rs`
- `packaging/aiolos.conf.workstation`
- `.agents/sow/specs/anemos-it87.spec.md`
- live `/opt/aiolos/etc/aiolos.conf`
- `it87` unit tests

Existing patterns to reuse:

- `rome2d-fans` consumes routed temperature inputs for case fan decisions.
- `nvidia` now filters routed inputs to temperature producers only and ignores invalid values.
- Existing `it87` zone mode should remain; only the case-zone input set changes.

Risk and blast radius:

- Runtime blast radius is limited to the workstation case fan zone and any host that configures extra `it87 input=` sources.
- CPU-zone behavior remains unchanged.
- No protocol or orchestrator changes are needed.

Sensitive data handling plan:

- No secrets, credentials, customer data, host serials, private endpoints, or raw sensitive data are required.
- Durable artifacts will reference only generic module names and config paths.

Implementation plan:

1. Add `it87` routed temperature extraction grouped by source module.
2. Drive the case zone from `max(CPU, routed temperature source maxima)`.
3. Keep CPU zone CPU-only.
4. Update `driven_by` provenance for case fans to show CPU plus per-source routed maxima.
5. Update workstation config template to route `nvidia`, `hwmon-temps`, and `nvme` into `it87`.
6. Update the `it87` spec.
7. Add focused tests.
8. Update live `/opt/aiolos/etc/aiolos.conf`, restart aiolos, and verify status output.

Validation plan:

- `cargo fmt --all`
- `cargo test -p anemoi-it87`
- `cargo clippy --all-targets`
- `cargo test`
- `git diff --check`
- Live service verification through `systemctl`, journal, and `status.json`.

Artifact impact plan:

- AGENTS.md: no update expected; workflow unchanged.
- Runtime project skills: no update expected; no protocol/lifecycle convention changed.
- Specs: update `.agents/sow/specs/anemos-it87.spec.md`.
- End-user/operator docs: update `packaging/aiolos.conf.workstation`.
- End-user/operator skills: no update expected.
- SOW lifecycle: keep current until implementation and live validation are complete.

Open-source reference evidence:

- No external open-source fan-control implementation was required. This is a project-local
  routing/control change over the existing aiolos signal model.
- Official Linux hwmon documentation was checked for temperature units.
- `influxdata/telegraf @ 0fd2d1fb0c45fe68194403f49850f77a7d2f6566`
  `plugins/inputs/temp/temp_linux.go` was checked as an open-source confirmation that Linux
  `temp*_input` readings are divided by 1000 before reporting Celsius.

Open decisions:

- Resolved by user on 2026-06-05: add all relevant temperature inputs to `it87` case-fan regulation.

## Implications And Decisions

1. **Case zone uses whole-box max temperature.**
   - Decision: `it87` case zone uses max of CPU and routed temperature producers from configured sources.
   - Implication: any hot board/VRM/DIMM/NIC/NVMe/GPU sensor can increase case fan speed.

2. **CPU zone remains CPU-only.**
   - Decision: CPU-zone channels continue to follow `coretemp`.
   - Implication: board/NVMe/DIMM sensors will not affect CPU-cooler headers.

3. **Only temperature producers count.**
   - Decision: sinks, fan duty, fan RPM, and other non-temperature signals are ignored.
   - Implication: no fan-output feedback loop.

## Plan

1. Implement source-grouped routed temperature extraction in `it87`.
2. Update case-zone decision and fan sink provenance.
3. Update config/spec/tests.
4. Validate locally and on the running workstation.

## Execution Log

### 2026-06-05

- Created SOW and recorded the user-approved behavior before implementation.
- Updated `anemoi/it87/src/main.rs` so the case fan decision uses CPU plus source-grouped routed
  temperature producer maxima.
- Updated `packaging/aiolos.conf.workstation` and live `/opt/aiolos/etc/aiolos.conf` to route
  `nvidia`, `hwmon-temps`, and `nvme` into `it87`.
- Installed rebuilt binaries with `./packaging/update.sh`, which stopped and restarted only
  `aiolos.service`.

## Validation

Acceptance criteria evidence:

- `anemoi/it87/src/main.rs:190-194`: extracts routed temperature maxima, reads CPU temperature,
  and uses their maximum as the case-zone raw input.
- `anemoi/it87/src/main.rs:520-539`: fan provenance reports `self:cpu` plus routed source maxima
  for case fans, while CPU-zone fans omit routed sources in zone mode.
- `anemoi/it87/src/main.rs:542-570`: routed inputs are accepted only when the signal is a
  temperature producer; sinks, RPM, duty, and out-of-range values are ignored.
- `packaging/aiolos.conf.workstation:12-15`: workstation example wires
  `it87 input=nvidia input=hwmon-temps input=nvme`.
- Live `/opt/aiolos/etc/aiolos.conf:12-15`: running workstation config uses the same routed inputs.
- Live `status.json`: `board:fan3:duty` and `board:fan4:duty` reported `how=zone:case` and
  `driven_by` values for `self:cpu`, `hwmon-temps (max)`, `nvidia (max)`, and `nvme (max)`.
- Live CPU-zone hardware check is not available on this host because `/opt/aiolos/etc/it87.conf`
  has `cpu=` empty; CPU_FAN is firmware-controlled. The CPU-zone rule is covered by unit tests.

Tests or equivalent validation:

- `cargo fmt --all`: passed.
- `cargo test -p anemoi-it87`: passed, 11 tests.
- `cargo clippy --all-targets`: passed.
- `cargo test`: passed, full workspace.
- `git diff --check`: passed.

Real-use evidence:

- `./packaging/update.sh`: completed successfully; rebuilt release binaries, stopped
  `aiolos.service`, installed binaries under `/opt/aiolos/bin/`, and restarted the service.
- `systemctl show aiolos.service`: `ActiveState=active`, `SubState=running`,
  `MainPID=3026342`, `NRestarts=0`,
  `ExecMainStartTimestamp=Fri 2026-06-05 04:21:00 EEST`.
- Live status sample after restart:
  - `board:fan3:duty`: `raw=67.0`, `how=zone:case`,
    `self:cpu=67`, `hwmon-temps (max)=59`, `nvidia (max)=34`, `nvme (max)=53`.
  - `board:fan4:duty`: same decision/provenance as `fan3`.

Reviewer findings:

- Self-review found no remaining code references to the old `gpu_max` helper or
  `max(GPU, CPU)` wording in the touched code/spec/config surfaces.
- External AI reviewers were not run because the project instructions allow those assistants only
  when the user explicitly asks for them.

Same-failure scan:

- `rg` scan over `anemoi/it87/src/main.rs`, `.agents/sow/specs/anemos-it87.spec.md`,
  `packaging/aiolos.conf.workstation`, and `/opt/aiolos/etc/aiolos.conf` found no remaining
  `gpu_max`, `input_temps_from`, `max(GPU, CPU)`, `max(GPU,CPU)`, `max(gpu,cpu)`, or
  `GPU routed inputs` matches.

Sensitive data gate:

- No sensitive data is required or recorded.

Artifact maintenance gate:

- AGENTS.md: no update required; workflow and repository instructions are unchanged.
- Runtime project skills: no update required; no protocol/lifecycle convention changed.
- Specs: updated `.agents/sow/specs/anemos-it87.spec.md`.
- End-user/operator docs: updated `packaging/aiolos.conf.workstation`.
- End-user/operator skills: no update required; no operator workflow changed.
- SOW lifecycle: completed and moved to `.agents/sow/done/`.

Specs update:

- Updated `.agents/sow/specs/anemos-it87.spec.md` to specify
  `max(CPU, routed temperature producers)` for case/uniform modes and to state that routed sinks
  and non-temperature producers are ignored.

Project skills update:

- No update required.

End-user/operator docs update:

- Updated `packaging/aiolos.conf.workstation` to route `nvidia`, `hwmon-temps`, and `nvme` into
  `it87`, and clarified that CPU-zone headers are whatever `it87.conf` declares.

End-user/operator skills update:

- No update required.

Lessons:

- The motherboard temperature scale was not the issue. The missing behavior was that `it87` did not
  consume the already-published whole-box temperature sources.

Follow-up mapping:

- No follow-up work is required for this SOW.

## Outcome

Implemented and installed on the workstation. Case fans now react to CPU plus routed
`hwmon-temps`, `nvidia`, and `nvme` temperature producers.

## Lessons Extracted

- When a host has separate sensor-only modules, fan-controller modules should document exactly which
  routed temperature publishers are control inputs, not just which sensors are displayed.

## Followup

None.

## Regression Log

None yet.
