# SOW-0009 - NUT / power-event reactor (+ GPU power-cap action)

## Status

Status: open

Sub-state: idea captured 2026-05-30. Not started. Gate at activation.

## Requirements

### Purpose
Make aiolos power-aware: read UPS / AC state, surface it, and on utility-power loss take a declared
action — e.g. cap GPU power via NVML to extend battery runtime. Showcases aiolos's domain-agnostic
"any device/signal" model (DESIGN §15 names a power-cap / alert anemos as the example).

### User Request
> Idea #4: the box has a 3 kVA UPS; aiolos is explicitly domain-agnostic — a power reactor is the
> showcase non-fan module.

### Assistant Understanding
Facts (inspection 2026-05-30):
- UPS via NUT (`upsc pr3000-nova`): `ups.status=OL`, `battery.charge=100`, `battery.runtime=5241 s`,
  `ups.load=9 %`, `input.voltage=224 V`. AC loss → `ups.status` changes (e.g. `OB`); the BMC also has
  an `STS_PSU1_AC_LOST` sensor.
- GPUs: `power.draw` 17–20 W idle, limit 400 W, **max 600 W**, throttle-reason bits — all
  readable/settable via NVML. Capping power under battery meaningfully extends runtime.

### Acceptance Criteria
- A module reads UPS state (and/or BMC AC-loss) and surfaces it (status page / readings).
- On a declared trigger (on-battery, or runtime < threshold) it performs the declared action
  (e.g. NVML power cap), and reverts on AC restore — with a fail-safe (action failure never harms).
- Verified against a real or simulated AC-loss event.

## Analysis
Sources: NUT (`upsc`/`upsd` socket), BMC SDR (`STS_PSU1_AC_LOST`), NVML (power limit get/set),
`~/CLAUDE.md` (UPS model/creds live in operator config, NOT artifacts).

This needs both a new **signal source** (NUT) and a new **controlled capability** (NVML power cap) —
the first aiolos module that controls something other than fans.

## Pre-Implementation Gate
Status: blocked (open SOW; gate at activation)

Open decisions:
- **Shape:** one combined reactor, or split — a `nut` *sensor* anemos (reports UPS state, routed)
  + a `gpu-powercap` *control* anemos that reacts to it via routing. The split fits the model better.
- **Signal source:** NUT (`upsc`/socket) vs the BMC AC-loss sensor vs both.
- **Action policy:** cap to what under battery? revert when? notify-only mode? Must be conservative
  (never cap so low it breaks a running job unexpectedly without a clear policy).
- **Fail-safe:** on module exit, restore the original power limit (like fans restore to firmware).
- **Sensitive data:** UPS creds / endpoints stay in operator config, never in committed artifacts.

## Plan
1. `nut` sensor anemos (UPS state) and/or BMC AC-loss reader.
2. `gpu-powercap` control anemos (NVML power limit set/restore) reacting to routed power state.
3. Surface power state; validate on a simulated AC-loss.

## Execution Log
### 2026-05-30
- Created (open) from the server-inspection idea list. No code.

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
