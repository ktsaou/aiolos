# SOW-0020 - NVIDIA fans follow routed temperature inputs

## Status

Status: completed

Sub-state: completed 2026-06-05. Optional routed-temperature support for `nvidia` is implemented and locally validated. Live workstation policy keeps `nvidia` GPU-only because routed board/CPU temperatures did not materially help cooling; case fans were tuned more aggressively instead.

## Requirements

### Purpose

Prevent CPU overheating on the user's watercooled desktop. The implementation adds optional routed-temperature support to `nvidia`, but the live workstation policy now keeps `nvidia` GPU-only and relies on stronger case-fan response because NVIDIA fan response did not materially help the CPU/board cooling problem.

### User Request

The user explained that the desktop NVIDIA GPU is watercooled and includes an array of fans that affect system airflow, but those fans do not speed up when the CPU heats up. The user asked to implement routed-temperature input support for the `nvidia` anemos.

### Assistant Understanding

Facts:

- `nvidia` currently ignores routed inputs and uses only its own GPU temperature.
- aiolos routes `input=` data from the latest completed blackboard report, so mutual routing does not deadlock.
- The user explicitly accepted wiring `it87` and `hwmon-temps` into `nvidia`.
- After live testing, the user reported that NVIDIA fan response did not help enough and requested that board temperatures no longer be sent to `nvidia`.
- `it87` already reads CPU temperatures from `coretemp` and can publish them.
- `hwmon-temps` can publish configured hwmon temperature sensors and is read-only.

Inferences:

- The safest behavior is to keep the old behavior when no inputs are configured.
- When inputs are configured, `nvidia` should drive from `max(own GPU temperature, routed temperature inputs)`.
- `nvidia` must ignore routed fan duty/RPM or other non-temperature signals to avoid feedback through fan outputs.

Unknowns:

- Live workstation config under `/opt/aiolos/etc/` may differ from packaging examples. Do not edit live operator config in this SOW unless explicitly requested.

### Acceptance Criteria

- `nvidia` uses its own GPU temperature when no routed temperature inputs are present.
- `nvidia` uses the maximum of GPU temperature and routed `kind=temperature` signal values when inputs are present.
- `nvidia` ignores routed non-temperature signals.
- `nvidia` reports sink `driven_by` and `driving` using the actual temperature that drove the fan duty.
- Workstation example config keeps `nvidia` GPU-only while preserving `it87 input=nvidia`.
- Live workstation config does not send board or CPU temperatures to `nvidia`.
- Live and packaging case-zone curve is `50 C -> 30%`, `80 C -> 70%`, `100 C -> 100%`.
- `anemos-nvidia` tests cover no inputs, higher external input, lower external input, and ignored non-temperature signals.
- Specs are updated to describe the new behavior.

## Analysis

Sources checked:

- `anemoi/nvidia/src/main.rs`
- `anemoi/it87/src/main.rs`
- `anemoi/hwmon-temps/src/main.rs`
- `aiolos/src/main.rs`
- `aiolos/src/registry.rs`
- `packaging/aiolos.conf.workstation`
- `.agents/sow/specs/anemos-nvidia.spec.md`
- `.agents/sow/specs/aiolos-orchestrator.spec.md`
- `.agents/sow/current/SOW-0017-20260531-component-report-model.md`
- `.agents/sow/current/SOW-0018-20260601-signal-label-schema.md`
- NVIDIA NVML API Reference Guide, device commands and queries, read 2026-06-05:
  - `https://docs.nvidia.com/deploy/nvml-api/group__nvmlDeviceCommands.html`
  - `https://docs.nvidia.com/deploy/nvml-api/group__nvmlDeviceQueries.html`
- Open-source reference checks:
  - `HackTestes/NVML-GPU-Control @ 1a4ca9061e17e851d9e41464a5d011a562baccd7`
    - `README.md`
    - `src/caioh_nvml_gpu_control/helper_functions.py`
  - `WickedLukas/nvidia-tuner @ bdc68445eebc26e797a73d699f05036af5b8551a`
    - `README.md`
    - `src/main.rs`
    - `src/nvml.rs`

Current state:

- `nvidia` declares `_inputs` in `apply` and ignores it.
- aiolos already supports `input=<module>` and sends latest completed source signals to consumers.
- `it87` publishes CPU temperature while also consuming `nvidia`; this is safe because aiolos routes stale/latest-completed data, not synchronous calls.

Risks:

- Cooling risk: a bad implementation could leave fans too slow. Mitigation: keep GPU temperature as an always-present floor and keep current NVML restore-on-error behavior.
- Noise risk: CPU spikes may make GPU radiator fans louder. Mitigation: the live workstation now keeps `nvidia` GPU-only; routed-temperature support remains opt-in for hosts where it helps.
- Feedback/coupling risk: mutual `nvidia`/`it87` routing could accidentally consume fan outputs. Mitigation: `nvidia` consumes only `temperature` producer signals.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- Working theory before live tuning: the NVIDIA radiator/fan array might contribute enough system airflow that routing CPU/board temperatures into `nvidia` would help. Live feedback later showed this did not materially help on this workstation, so the current live policy keeps `nvidia` GPU-only and makes the case fans more aggressive.

Evidence reviewed:

- `anemoi/nvidia/src/main.rs`: `apply` ignores `_inputs`, reads `self.gpu.temperature()`, and calls `ctrl.duty(temp)`.
- `aiolos/src/main.rs`: `build_inputs` gathers latest completed blackboard signals for configured input sources.
- `anemoi/it87/src/main.rs`: reads `coretemp` and publishes CPU temperature signals.
- `packaging/aiolos.conf.workstation`: already wires `it87 input=nvidia`; adding the reverse route is stale-data routing, not a direct call cycle.

Affected contracts and surfaces:

- `anemoi/nvidia/src/main.rs`
- `packaging/aiolos.conf.workstation`
- `.agents/sow/specs/anemos-nvidia.spec.md`
- Unit tests for `anemoi-nvidia`

Existing patterns to reuse:

- `rome2d-fans` and `it87` already extract routed temperature inputs by checking `Signal::kind() == Some("temperature")`.
- `nvidia` already records `driven_by` and `driving` metadata on claimed fan-duty sinks.
- Existing controller smoothing/curve handling should remain unchanged.

Risk and blast radius:

- Runtime blast radius is limited to NVIDIA fan duty decisions when `nvidia` has `input=` wiring.
- Default no-input behavior remains unchanged.
- No protocol wire-format changes and no orchestrator changes are needed.

Sensitive data handling plan:

- No secrets, credentials, private endpoints, customer data, host serials, or raw live sensor dumps are required.
- Durable artifacts will reference only file paths and generic sensor/module names.

Implementation plan:

1. Add helper logic in `nvidia` to extract temperature values from routed inputs.
2. Use `max(gpu_temp, routed_temps...)` as the driving temperature in `apply`.
3. Preserve fail-safe behavior when GPU temperature cannot be read.
4. Update report metadata to show the actual driving temperature and include routed-temperature provenance.
5. Initially update the workstation example config to route `it87` and `hwmon-temps` into `nvidia`; later revert that live/template wiring when testing shows it does not help this workstation.
6. Update the `nvidia` spec.
7. Add focused tests.

Validation plan:

- `cargo fmt --all`
- `cargo test -p anemoi-nvidia`
- `cargo test`
- `git diff --check`
- Same-failure scan for other modules that ignore routed inputs where docs claim they consume them.

Artifact impact plan:

- AGENTS.md: no update expected; project-level workflow unchanged.
- Runtime project skills: no update expected; no protocol or new-anemos workflow change.
- Specs: update `anemos-nvidia.spec.md`.
- End-user/operator docs: update workstation packaging example comments.
- End-user/operator skills: no update expected.
- SOW lifecycle: keep this SOW current until validation and review are complete; close with the implementation if successful.

Open-source reference evidence:

- `HackTestes/NVML-GPU-Control @ 1a4ca9061e17e851d9e41464a5d011a562baccd7` documents and implements manual fan control, automatic fan policy restore, and curve/temperature-loop control.
- `WickedLukas/nvidia-tuner @ bdc68445eebc26e797a73d699f05036af5b8551a` uses a temperature/fan curve loop and documents automatic fan-control restore on termination.
- These references were used only to confirm the safety shape: manual fan control must keep monitoring temperatures and must restore default/auto policy on exit/failure. The aiolos implementation remains project-local and reuses existing aiolos routing/controller patterns.

Open decisions:

- Resolved by user on 2026-06-05: implement routed-temperature support for `nvidia`, including `it87` and `hwmon-temps` as acceptable sources.

## Implications And Decisions

1. **Use routed temperature max for NVIDIA fan duty when inputs are configured.**
   - Decision: `nvidia` supports driving from `max(own GPU temp, routed temperature inputs)`.
   - Implication: CPU/board heat can increase NVIDIA radiator/fan speed on hosts that explicitly wire inputs.

2. **Ignore non-temperature routed signals.**
   - Decision: only signals with `kind == temperature` are consumed.
   - Implication: mutual `nvidia`/`it87` wiring does not feed fan output values back into the controller.

3. **Preserve default and fail-safe behavior.**
   - Decision: no-input behavior stays GPU-only; GPU temperature read failure still restores firmware/default fan control.
   - Implication: this change is opt-in through `input=` wiring and does not weaken the existing safety fallback.

4. **Current workstation policy after live test.**
   - Decision: keep live and packaged workstation `nvidia` GPU-only; do not send board/CPU temperatures to `nvidia`.
   - Implication: case fans, not NVIDIA radiator fans, are the primary live response to CPU/board heat.

## Plan

1. Implement input-temperature extraction and driving-temperature selection in `nvidia`.
2. Update workstation registry example and `nvidia` spec.
3. Run focused and workspace tests.
4. Record validation, same-failure scan, and close or leave follow-ups.

## Execution Log

### 2026-06-05

- Created SOW and recorded the user-approved design before implementation.
- Implemented routed-temperature extraction in `nvidia`.
- Changed `nvidia` fan-duty selection to use the maximum of GPU temperature and routed temperature producer values.
- Preserved GPU temperature as the always-present floor and preserved restore-on-read/set failure behavior.
- Added fan sink `driven_by` provenance for routed temperature inputs and `driving.how` metadata.
- Updated the workstation registry example to route `it87` and `hwmon-temps` into `nvidia`.
- Updated the `nvidia` spec.
- Added focused unit tests for routed temperature extraction, max-temperature selection, lower routed input behavior, ignored non-temperature signals, and reported routed provenance.
- Backed up the live workstation registry to `/opt/aiolos/etc/aiolos.conf.bak-20260605-nvidia-inputs`.
- Changed live `/opt/aiolos/etc/aiolos.conf` from `nvidia` to `nvidia input=it87 input=hwmon-temps`.
- Ran `./packaging/update.sh`; it built release binaries, stopped only `aiolos.service`, replaced `/opt/aiolos/bin/*`, and restarted `aiolos.service`.
- Verified `aiolos.service` active/running with `NRestarts=0`.
- Verified the status API reports the NVIDIA fan sink as driven by routed CPU/board temperature values with `how=max(self,routed)→curve`.
- Tuned the live board case-zone curve after user instruction:
  - backed up `/opt/aiolos/etc/it87.case.curve.json` to `/opt/aiolos/etc/it87.case.curve.json.bak-20260605-case-curve`
  - changed live and packaging case curve to `50 C -> 30%`, `80 C -> 60%`, `100 C -> 100%`, `sensitivity=0.4`
  - verified the status API reported case fans driven by `zone:case` with output matching the new curve.
- After the user reported that NVIDIA fan response did not help:
  - backed up `/opt/aiolos/etc/aiolos.conf` to `/opt/aiolos/etc/aiolos.conf.bak-20260605-disable-nvidia-inputs`
  - backed up `/opt/aiolos/etc/it87.case.curve.json` to `/opt/aiolos/etc/it87.case.curve.json.bak-20260605-case-80-70`
  - changed live and packaging `nvidia` workstation line back to GPU-only
  - changed live and packaging case curve to `50 C -> 30%`, `80 C -> 70%`, `100 C -> 100%`, `sensitivity=0.4`
  - restarted only `aiolos.service` so the registry input removal took effect
  - verified `nvidia` fan sink now reports `how=self→curve` and `driven_by` only `gpu0`
  - verified case fans report `how=zone:case` and use the stronger curve.

## Validation

Acceptance criteria evidence:

- `nvidia` no-input behavior remains GPU-only:
  - `anemoi/nvidia/src/main.rs:76` keeps read-only `collect` based on GPU temperature only.
  - `anemoi/nvidia/src/main.rs:236` always includes the GPU temperature in `driving_temperature`.
  - `anemoi/nvidia/src/main.rs:449` tests no routed inputs and lower routed input behavior.
- `nvidia` uses maximum routed temperature when inputs are present:
  - `anemoi/nvidia/src/main.rs:93` extracts routed temperatures.
  - `anemoi/nvidia/src/main.rs:94` selects the driving temperature.
  - `anemoi/nvidia/src/main.rs:95` feeds that driving temperature into the existing controller.
  - `anemoi/nvidia/src/main.rs:465` tests a higher routed temperature.
- `nvidia` ignores routed non-temperature signals:
  - `anemoi/nvidia/src/main.rs:212` accepts only producer signals with `kind == "temperature"`.
  - `anemoi/nvidia/src/main.rs:218` ignores routed temperature values that do not fit the controller's integer range.
  - `anemoi/nvidia/src/main.rs:423` tests that fan RPM, fan duty, and out-of-range values are ignored.
- `nvidia` reports the actual fan driver:
  - `anemoi/nvidia/src/main.rs:257` records GPU and routed temperature provenance in `driven_by`.
  - `anemoi/nvidia/src/main.rs:282` reports raw/smoothed driving temperature and output duty.
  - `anemoi/nvidia/src/main.rs:487` tests routed provenance and `max(self,routed)→curve`.
- Workstation example config matches the current live policy:
  - `packaging/aiolos.conf.workstation:11` keeps `nvidia` GPU-only.
  - `packaging/aiolos.conf.workstation:12` preserves `it87 input=nvidia`.
- Specs describe the new behavior:
  - `.agents/sow/specs/anemos-nvidia.spec.md:5` records the watercooled/radiator airflow purpose.
  - `.agents/sow/specs/anemos-nvidia.spec.md:22` records `max(own GPU temperature, routed temperature inputs)`.
  - `.agents/sow/specs/anemos-nvidia.spec.md:93` records acceptance criteria.

Tests or equivalent validation:

- `cargo fmt --all` passed.
- `cargo test -p anemoi-nvidia` passed: 6 tests.
- `cargo clippy --all-targets` passed after refactoring the fan sink helper argument list.
- `cargo test` passed for the full workspace.
- `git diff --check` passed.

Real-use evidence:

- Performed on the workstation after explicit user request.
- Live config evidence:
  - backup: `/opt/aiolos/etc/aiolos.conf.bak-20260605-nvidia-inputs`
  - earlier test line: `nvidia input=it87 input=hwmon-temps`
  - later backup before disabling inputs: `/opt/aiolos/etc/aiolos.conf.bak-20260605-disable-nvidia-inputs`
  - current active config line: `nvidia`
- Install evidence:
  - `./packaging/update.sh` completed successfully.
  - `systemctl show aiolos.service --property=MainPID,ActiveState,SubState,NRestarts,ExecMainStartTimestamp` reported `ActiveState=active`, `SubState=running`, `NRestarts=0`.
- Behavior evidence:
  - Service logs showed a NVIDIA fan decision with GPU temperature lower than the selected driving temperature, proving routed temperature input affected the fan command.
  - `http://127.0.0.1:9876/status.json` showed the NVIDIA fan duty sink with `driving.how=max(self,routed)→curve`, routed CPU/board `driven_by` values, and output fan duty derived from the routed maximum.
  - After reverting live NVIDIA inputs, service logs and the status API showed `nvidia` fan duty with `driving.how=self→curve` and only GPU temperature provenance.
  - After increasing the case curve, service logs and the status API showed case fans in `zone:case` using the stronger case curve.

Reviewer findings:

- No external reviewers were run. The user did not request external reviewer agents for this SOW.
- Local review found one issue before final validation: routed JSON temperature values were initially cast directly to `i32`; fixed by ignoring out-of-range values with `i32::try_from`.

Same-failure scan:

- Ran:
  - `rg -n "fn apply\(.*inputs|_inputs: Option<&Inputs>|input_temps|routed_temperature|power_signal\(|kind\(\) == Some\(\"temperature\"\)|inputs are ignored|inputs ignored" anemoi .agents/sow/specs`
- Result:
  - Expected consumers found: `nvidia`, `rome2d-fans`, `it87`, and `nvidia-powercap`.
  - Sensor-only modules still use `_inputs` only for read-only collection/default apply behavior.
  - No stale `nvidia` spec text saying inputs are ignored remains.

Sensitive data gate:

- No sensitive data is required or recorded.

Artifact maintenance gate:

- AGENTS.md: no update needed; project workflow did not change.
- Runtime project skills: no update needed; no protocol, lifecycle, or new-anemos workflow changed.
- Specs: updated `.agents/sow/specs/anemos-nvidia.spec.md`.
- End-user/operator docs: updated `packaging/aiolos.conf.workstation`.
- End-user/operator skills: no update needed; no reusable operator workflow changed.
- SOW lifecycle: completed; moved to `.agents/sow/done/` with the implementation commit.

Specs update:

- Updated `.agents/sow/specs/anemos-nvidia.spec.md`.

Project skills update:

- No update needed.

End-user/operator docs update:

- Updated `packaging/aiolos.conf.workstation`.
- Updated `packaging/it87.case.curve.json`.

End-user/operator skills update:

- No update needed.

Lessons:

- The existing aiolos blackboard routing model already supports mutual-looking configuration like `nvidia input=it87` and `it87 input=nvidia` because modules consume the latest completed peer report, not a synchronous call.
- For safety, the fan controller should never consume routed fan outputs; filtering to temperature producers keeps feedback out of the control loop.

Follow-up mapping:

- Live activation completed:
  - inspected `/opt/aiolos/etc/aiolos.conf`
  - added `input=it87 input=hwmon-temps` to the live `nvidia` line
  - restarted aiolos through `./packaging/update.sh`
  - confirmed the status API shows `nvidia` fan sinks driven by routed CPU/board temperatures
- Live policy adjustment completed:
  - removed `input=it87 input=hwmon-temps` from the live `nvidia` line
  - updated `packaging/aiolos.conf.workstation` to keep `nvidia` GPU-only
  - restarted only `aiolos.service`
  - confirmed the status API shows `nvidia` fan sinks driven only by GPU temperature
- Case-zone curve tuning completed:
  - updated live `/opt/aiolos/etc/it87.case.curve.json`
  - updated `packaging/it87.case.curve.json`
  - increased the 80 C point from 60% to 70% after live feedback
  - confirmed the status API uses the new curve
- Remaining follow-up:
  - monitor under sustained CPU-heavy load

## Outcome

Completed. `nvidia` supports optional routed temperature inputs, but the live workstation currently keeps `nvidia` GPU-only and uses a stronger case-fan curve.

## Lessons Extracted

- Routed input support should be treated as a general module capability, but each controller must explicitly whitelist the signal kinds it consumes.
- Fan-control changes need both behavior tests and metadata tests, because the status page is the operator's proof that the correct temperature source is driving the output.

## Followup

- Monitor under sustained CPU-heavy load.

## Regression Log

None yet.
