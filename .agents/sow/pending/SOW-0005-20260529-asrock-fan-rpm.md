# SOW-0005 - asrock16-2t reports per-fan RPM (and truthful pwm readback)

## Status

Status: open

Sub-state: QUEUED behind SOW-0004 (NVMe). Decisions recorded; full Pre-Implementation Gate to be
written when this SOW is activated (moved to current/).

## Requirements

### Purpose

`asrock16-2t` should report each board fan's actual tachometer RPM in its `apply` readings (today
it reports only a commanded duty), so the status page shows real fan speeds. Also report the true
duty readback rather than the commanded percent.

### User Request

> "there is regression in asrock, rpm is not reported back - please also fix this"

### Assistant Understanding

Facts (evidence gathered 2026-05-29):

- NOT a regression: 0 commits in git history touched `rpm` under `anemoi/asrock16-2t/`; no
  tach/RPM code exists; `board.rs` has only duty commands (`0xd6`/`0xd8`/`0xda`). RPM was always a
  deferred enhancement — `anemos-asrock16-2t.spec.md:26-27` ("per-fan tach RPM ... planned
  enhancement (require SDR repository decoding)"). `nvidia` *does* report rpm (NVML), the likely
  source of the expectation.
- RPM IS readable via IPMI: BMC exposes `FAN1_1..FAN8_1` at sensor numbers `0x60..0x67` with live
  RPM (verified read-only via `sudo ipmitool sdr type Fan`: e.g. 600/1400/800 RPM). `FAN*_2` and
  `FAN_PSU*` read "No Reading" (ns) on this host.
- No hwmon shortcut: there are NO `fan*_input` hwmon sensors on this board (only `k10temp` +
  `nvme` chips) — RPM must go through IPMI.
- Related discrepancy: `main.rs:107` reports `pwm: pct` = the *commanded* percent, while the spec
  says report the `0xda` readback as each fan's pwm.

### Acceptance Criteria

- `asrock16-2t` `apply` readings include each readable fan's `rpm` (omit fans reading "No
  Reading"/ns), plus a truthful `pwm` from the `0xda` duty readback.
- RPM conversion correct against `ipmitool` values; verified on-hardware.
- Bounded `apply` (RPM reads must not blow the timeout); SDR conversion factors cached once at
  open, not re-scanned per tick.
- Fail-safe unchanged; tach reads validated to still work while fans are claimed-manual.

## Implications And Decisions

Resolved (user, 2026-05-29 — "I agree"):

1. **Sequencing → 1a:** finish SOW-0004 (NVMe) first; this RPM work is SOW-0005, started after.
2. **RPM acquisition → 2a:** full SDR decode over the existing raw-ioctl `ipmi` crate — one SDR
   repository scan at `open` to cache M/B/exponent conversion for sensors `0x60..0x67`, then
   `Get Sensor Reading` (`0x04/0x2d`) per fan per tick → convert to RPM. (Chosen over 2b
   hardcoded-factors and 2c shell-to-ipmitool.)
3. **pwm fix → yes:** also report the `0xda` duty readback as each fan's `pwm`, instead of the
   commanded pct (matches the spec).

## Analysis

Sources checked:

- `anemoi/asrock16-2t/src/board.rs` (no tach path), `src/main.rs:103-109` (pwm=commanded pct).
- `anemos-asrock16-2t.spec.md:26-27` (RPM deferred), `DESIGN.md:124` (example shows rpm).
- `tech/ipmi/src/lib.rs` (raw transport to extend with SDR + Get Sensor Reading).
- Live: BMC fan sensors `0x60..0x67`; no hwmon fan inputs; `ipmitool` present; `/dev/ipmi0` root.

Open items for the gate (when activated):

- Exact SDR repository protocol (reserve `0x0a/0x22`, Get SDR `0x0a/0x23` by record id, parse Full
  Sensor Record type 0x01 for sensor number + M/B/Bexp/Rexp/units), per-sensor `Get Sensor
  Reading` (`0x04/0x2d`), conversion `RPM = ((M*raw)+(B*10^Bexp))*10^Rexp`, and "No Reading" /
  unavailable-sensor handling. Validate tach readability while claimed-manual.

## Pre-Implementation Gate

Status: blocked (queued behind SOW-0004; full gate written at activation)

## Plan

Deferred until activation. High level: extend `tech/ipmi` with SDR scan + Get Sensor Reading;
`board.rs` gains cached fan-sensor factors + `read_rpms()` + duty readback; `main.rs` adds
`rpm`/`pwm` to fan readings; spec updated (RPM no longer "planned"); on-hardware validation.

## Execution Log

### 2026-05-29

- Created as a queued SOW. Evidence gathered, decisions 1a/2a/3 recorded. No code.

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
