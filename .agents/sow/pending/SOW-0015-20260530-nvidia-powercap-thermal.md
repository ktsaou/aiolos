# SOW-0015 - nvidia-powercap: thermal trigger (whole-system temperature backstop)

## Status

Status: open

Sub-state: idea captured 2026-05-30. Not started. **Extends the `nvidia-powercap` control anemos
introduced by SOW-0009** with a second, temperature-driven trigger. **Depends on SOW-0009**
(the power-cap module + NVML cap/restore plumbing), **SOW-0014** (typed kinds + `input=` validation —
the module now requires BOTH `power-state` AND `temp`, the first real multi-input consumer), and
benefits from **SOW-0013** (low-latency scheduler). Build those first. Also folds in the agreed
rename `gpu-powercap` → `nvidia-powercap`.

## Requirements

### Purpose
Add a **last-resort thermal backstop**: when a GPU stays too hot while its fans are already maxed and
the temperature is not falling, cap the GPU's NVML power limit to shed heat — and revert when it
recovers. The point is **whole-system** protection: aiolos sees GPU + board + NVMe + DIMM + LAN/VRM
temps (via `nvidia`, `nvme`, `ipmi-temps`), so it can cap GPU power to protect surrounding components
that have **no thermal throttle of their own** — something the GPU's built-in die throttle does not do.

### User Request
> 2026-05-30: "we could have dynamic powercap based on temperatures too. No? If temperature is >80
> degrees and fans at 100% and the thing does not cool down, lower power. hm... is this a pragmatic
> idea?"

Agreed shape (this SOW): a temperature trigger on the existing power-cap module, framed explicitly as
a **secondary backstop** to the GPU's hardware throttle, with whole-system temp awareness as the real
value-add.

### Assistant Understanding
Facts:
- NVIDIA GPUs **already** thermal-throttle clocks/power in hardware at their limit (≈83–88 °C, hard
  cutoff above). That hardware loop is instantaneous and always present; a software loop at 0.1–5 s
  ticks **cannot and must not** be the primary GPU-die protection.
- aiolos already controls the NVML **power limit** (SOW-0009 `nvidia-powercap`: `power_limits()`,
  `set_power_limit()` clamped to device min/max, `restore_power()`, restore on every exit path + Drop).
  Adding a temperature trigger reuses the **same actuator** — only the decision input changes.
- aiolos already collects the full thermal picture as routable `temp` readings: GPU die (`nvidia`),
  NVMe composite (`nvme`), and board/CPU/DIMM/LAN sensors (`ipmi-temps`, SOW-0006).
- The `anemos` SDK Controller already provides EMA + deadband (hysteresis), which a cap/lift loop needs
  to avoid oscillation.

Inferences:
- The genuine value over the hardware throttle is protecting **non-GPU** components (board, VRM, NVMe,
  DIMM) by reducing total GPU heat — the hardware throttle only guards the GPU die.
- "Fans at 100%" need not be sensed directly: with the shipped curves (100 % at the ceiling temp),
  "GPU temp past the curve ceiling" already implies the fans are maxed. Reading the GPU fan `pwm`/`rpm`
  (already in `nvidia` readings) can make the condition explicit/robust if desired.

Unknowns (resolve at activation, mostly user policy):
- Exact trigger thresholds and which temp sources gate the cap (GPU-only vs also board/NVMe).
- Cap depth / step policy and lift policy (hysteresis band, dwell).

### Acceptance Criteria
- `nvidia-powercap` accepts a routed `temp` input (in addition to `power-state`) and, on a declared
  thermal trigger (temp over threshold for N ticks AND fans maxed AND temp not decreasing), caps the
  GPU power limit; it **lifts** the cap with hysteresis when the temperature recovers.
- The cap is a **conservative trim**, never lower than a configured floor, and is explicitly documented
  as secondary to the GPU hardware throttle (which remains the real die-safety net).
- Fail-safe identical to SOW-0009: any failure / exit / SIGKILL restores the firmware-default power
  limit; a wrong/low temp reading can never *raise* the cap or harm the GPU.
- The two triggers (UPS on-battery from SOW-0009, thermal from this SOW) compose to the **most
  conservative** active limit, with no oscillation between them.
- Verified against a simulated/real sustained-thermal event (operator-gated, on real hardware).

## Analysis
Sources to check at activation: SOW-0009 deliverables (`anemoi/nvidia-powercap/`, `tech/nvml` power
methods), SOW-0014 typed `input=` (multi-input `requires`), SOW-0006 `ipmi-temps`, `anemos` Controller
(EMA/deadband), the routing path in `aiolos` (`build_inputs`, `module:id` keying).

This is the first module to **require two input kinds** (`power-state` + `temp`) — a concrete driver
for SOW-0014's open "multiplicity" decision. The cap actuator and restore plumbing already exist; the
new surface is the decision policy plus a second routed input.

Risks:
- **Loop interaction:** two actuators (fans, then power) on the same temperature → must cap only when
  fans are exhausted, with hysteresis, or the two loops fight. Mitigated by the "fans maxed AND temp
  not falling" gate + the Controller's deadband.
- **Racing the hardware throttle:** if mis-tuned aggressive, it duplicates the die throttle for no gain.
  Mitigated by conservative defaults and framing as a board/VRM/NVMe protector, not a die protector.
- **Job impact:** capping power slows a running job. Must be conservative, floored, logged, and clearly
  policy-gated (never a surprise hard cut).

## Pre-Implementation Gate
Status: blocked (open SOW; gate at activation, after SOW-0009/0013/0014 land)

Problem / root-cause model:
- The GPU die is already protected by hardware throttle, but surrounding components (board, VRM, NVMe,
  DIMM) are not, and fans alone may be insufficient under sustained dense-GPU load. A software power
  trim driven by the **whole-system** thermal picture fills that gap.

Affected contracts and surfaces:
- `nvidia-powercap` module (renamed from `gpu-powercap`): add a second routed input + thermal policy.
- Registry: `nvidia-powercap input=nut input=nvidia [input=ipmi-temps] [input=nvme]`.
- `nvidia-powercap.conf`: thermal thresholds, cap floor/step, hysteresis, dwell — live-reloaded.
- Specs: the `nvidia-powercap` / power spec(s) from SOW-0009; protocol spec only if the multi-input
  declaration wording needs it (owned by SOW-0014).

Existing patterns to reuse:
- SOW-0009 NVML cap/restore + every-exit-path + Drop fail-safe.
- `anemos` Controller EMA + deadband for the cap/lift hysteresis.
- `module:id`-keyed routing to attribute GPU vs board vs NVMe temps; reading-`type` filtering.

Risk and blast radius:
- Touches only the power-cap module + its config + its routing line; no change to fan control or the
  orchestrator core beyond a registry wiring. Worst case on bug = a too-low cap on a job (floored,
  reversible, logged), never a thermal-safety regression (hardware throttle remains underneath).

Sensitive data handling plan:
- No new sensitive data. UPS creds/endpoints already confined to operator config (SOW-0009). No
  BMC IP / serials / secrets in artifacts.

Implementation plan (sketch — finalize at activation):
1. Rename `gpu-powercap` → `nvidia-powercap` (binary, dir, registry, install/update lists, specs).
2. Add `temp` to the module's `requires` (SOW-0014) and wire `input=nvidia` (+ optionally
   `ipmi-temps`/`nvme`) in the registry.
3. Thermal decision: per-GPU EMA of temp; trigger when temp ≥ threshold for N ticks AND fans maxed AND
   temp not decreasing; cap toward a configured floor in conservative steps; lift with a hysteresis
   band + dwell on recovery. Compose with the UPS trigger as a most-conservative min.
4. Config keys in `nvidia-powercap.conf`; tests for the decision matrix (incl. compose-with-UPS,
   oscillation guard, floor clamp, fail-safe).

Validation plan:
- Unit tests for the thermal decision + composition + hysteresis (no hardware).
- Operator-gated on-hardware test: induce sustained GPU load, observe cap engage when fans maxed and
  temp plateaus high, observe lift on recovery, confirm restore on stop/kill.

Artifact impact plan:
- AGENTS.md: likely unaffected (module-level change).
- Runtime project skills: `project-create-anemos` may gain a note on multi-input control modules.
- Specs: update the SOW-0009 power spec to document the thermal trigger; defer multi-input declaration
  wording to SOW-0014.
- End-user/operator docs: document the thermal policy + that it is secondary to the hardware throttle.
- SOW lifecycle: standalone follow-on; no regression handling.

Open decisions (for the user, at activation):
1. **Temp sources that gate the cap:** GPU die only, or also board/NVMe/VRM (the whole-system value).
   Recommendation: GPU die primary; allow board/NVMe as additional, OR-ed triggers via config.
2. **Thresholds/depth:** trigger temp, N ticks, cap floor (% of default), step size, hysteresis band,
   dwell. Recommendation: conservative defaults (e.g. trigger ~85 °C sustained, floor not below a
   usable fraction of default), all in config.
3. **Notify-only mode:** ship a log-only mode first (observe before it ever caps)?
   Recommendation: yes — default to monitor+log, opt into capping, mirroring SOW-0009's `cap_on_battery`.

## Implications And Decisions
None recorded yet (open SOW). Decisions 1–3 above to be resolved with the user at activation.

## Plan
1. Activate after SOW-0009 (power-cap), SOW-0014 (multi-input typing), SOW-0013 (scheduler) land.
2. Rename to `nvidia-powercap`; add `temp` input + thermal policy; config + tests.
3. Operator-gated on-hardware validation with the user (sustained-thermal event).

## Execution Log
### 2026-05-30
- Created (open) from the 2026-05-30 design discussion ("dynamic powercap based on temperatures").
  Captured as a follow-on to SOW-0009 rather than expanding 0009 mid-build. No code.

## Validation
Pending.

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
None yet.

## Regression Log
None yet.
