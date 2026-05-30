# SOW-0005 - asrock16-2t reports per-fan RPM (and truthful pwm readback)

## Status

Status: in-progress

Sub-state: code-complete; 4-round external review converged (5/5 ready to ship); awaiting USER
on-hardware validation before completion. Not committed.

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
2. **RPM acquisition → 2a:** decode the real per-sensor conversion factors over the existing
   raw-ioctl `ipmi` crate (robust to firmware changes; chosen over 2b hardcoded-factors and 2c
   shell-to-ipmitool).
   - **2a mechanism REFINED (2026-05-30, read-only BMC probe — surfaced for confirmation):** use
     **`Get Sensor Reading Factors` (`0x04/0x23`, `[sensor, reading]`)** per fan sensor instead of
     walking the SDR repository (`0x0a` reserve + `Get SDR` + parse 64-byte Full Sensor Records).
     Verified working on this BMC: sensors `0x60` and `0x62` both return `20 64 00 00 00 00 00` →
     **M=100, B=0, Bexp=0, Rexp=0 → RPM = raw×100**. Same robustness (reads the device's real
     factors, not a hardcoded constant) with far less code and no SDR-repo reservation protocol.
     Cache the factors once at `open`; per tick only `Get Sensor Reading` (`0x04/0x2d`) per fan.
3. **pwm fix → yes:** also report the `0xda` duty readback as each fan's `pwm`, instead of the
   commanded pct (matches the spec).

## Analysis

Sources checked:

- `anemoi/asrock16-2t/src/board.rs` (no tach path), `src/main.rs:103-109` (pwm=commanded pct).
- `anemos-asrock16-2t.spec.md:26-27` (RPM deferred), `DESIGN.md:124` (example shows rpm).
- `tech/ipmi/src/lib.rs` (raw transport to extend with SDR + Get Sensor Reading).
- Live: BMC fan sensors `0x60..0x67`; no hwmon fan inputs; `ipmitool` present; `/dev/ipmi0` root.

Read-only BMC investigation (2026-05-30):

- Fan tach sensors `FAN1_1..FAN8_1` = sensor numbers `0x60..0x67` (Entity 29, Fan Device). Physically
  FAN1/FAN2 are the Noctua CPU coolers (low RPM by size), FAN3–FAN8 the 120 mm case fans. `FAN*_2`
  (`0x68..0x6F`) and `FAN_PSU1/2` (`0x70/0x71`) report "No Reading" (ns) on this host → skip.
- `Get Sensor Reading` (`0x04/0x2d`, `[sensor]`): e.g. `0x60` → `06 c0 c0 00` (raw byte 0 = `0x06`).
- `Get Sensor Reading Factors` (`0x04/0x23`, `[sensor, reading]`): `0x60` and `0x62` →
  `20 64 00 00 00 00 00` ⇒ M=100, B=0, Bexp=0, Rexp=0 ⇒ `RPM = ((M·raw)+(B·10^Bexp))·10^Rexp` =
  `raw·100`. (`0x60`: raw 6 → 600 RPM, matches `ipmitool`.) Factors are constant (linear sensor) →
  cache at open.

## Pre-Implementation Gate

Status: ready (D1 confirmed by user "yes" 2026-05-30 — use Get Sensor Reading Factors per sensor)

Problem / root-cause model:

- `asrock16-2t` reports only a commanded duty and no fan RPM because tach was a deferred enhancement
  (no IPMI sensor-read code in `board.rs`). RPM is reachable only via IPMI (no hwmon fan inputs on
  this board). The fix reads each fan's tach via standard IPMI sensor commands and reports it.

Evidence reviewed:

- `anemoi/asrock16-2t/src/board.rs` (duty-only OEM commands; `query_duty`=`0xda` exists, no tach).
- `anemoi/asrock16-2t/src/main.rs` (fan readings report `pwm: pct` = commanded, per fan, no rpm).
- `tech/ipmi/src/lib.rs` (`Ipmi::raw(netfn, cmd, data) -> Vec<u8>` returns the bytes AFTER the
  completion code, 2 s-bounded — exactly what `Get Sensor Reading`/`…Factors` need; no transport
  change required).
- `anemos-asrock16-2t.spec.md` (RPM listed as planned; pwm should be the `0xda` readback).
- Live read-only BMC probe (above). External OSS: none (standard IPMI v2.0 commands; no mirrored
  repo).

Affected contracts and surfaces:

- `tech/ipmi`: ADD two generic, standard-IPMI helpers — `read_sensor(num) -> raw+validity` and
  `read_sensor_factors(num) -> {m,b,b_exp,r_exp}` (netfn `0x04`). Generic (not board-specific).
- `anemoi/asrock16-2t/src/board.rs`: ADD the fan-sensor map (`0x60..0x67`), factor caching at open,
  `read_fan_rpms() -> Vec<(label, Option<rpm>)>`, and reuse `query_duty` for the pwm readback.
- `anemoi/asrock16-2t/src/main.rs`: fan readings become per-fan `{pwm: <0xda readback>, rpm: <tach>}`
  (omit `rpm` for unavailable sensors). No change to the control decision (set happens first; RPM +
  duty readback are observability read AFTER the set).
- Spec `anemos-asrock16-2t.spec.md` (RPM no longer "planned"; pwm = readback); `DESIGN.md` example.
- No protocol/orchestrator change; no curve/registry change.

Existing patterns to reuse:

- `Ipmi::raw` for the sensor commands (like `board.rs` already builds `0xd6/0xd8/0xda` on it).
- The pure-payload + trait-based unit-test style already in `board.rs` (`FanBus`, `duty_payload`).
- `Reading::new("fan", label, json!({...}))` reporting shape already in `main.rs`.

Risk and blast radius:

- LOW–MEDIUM. The control path is untouched (reads are additive, post-set). Risks:
  (1) per-tick latency — 8 `Get Sensor Reading` + 1 `0xda` per tick; factors cached at open; each
  IPMI call is ~ms (2 s-bounded). Must stay within the apply timeout — keep it to ≤9 reads, no
  retries. (2) A failed/`ns` sensor must degrade to "omit rpm", never fail the control tick. (3)
  **tach-while-claimed-manual**: the probe ran with `nvfd` in control; must validate on-hardware
  that tach still reads while aiolos holds manual mode (expected yes — tach is independent of duty
  control, but unverified). (4) fail-safe/restore unchanged.

Sensitive data handling plan:

- None exposed. Sensor numbers and IPMI commands are public/standard; no BMC IP/credentials touched
  (inband `/dev/ipmi0`). No secrets in artifacts.

Implementation plan (ordered):

1. `tech/ipmi`: `read_sensor(num)` (parse `0x2d` reply: raw byte + the "reading unavailable" status
   bit) and `read_sensor_factors(num)` (parse `0x23` reply into M/B/Bexp/Rexp, signed exponents);
   pure decode helpers + unit tests against the verified bytes (`20 64 00 …` → M=100,…).
2. `board.rs`: fan map `0x60..0x67` (labels FAN1..FAN8); cache factors at open; `read_fan_rpms()`
   → per fan `Option<rpm>` (None when unavailable/failed); RPM conversion as a pure, unit-tested fn.
3. `main.rs`: read `0xda` duty + `read_fan_rpms()` AFTER `set_all_fans`; report per fan
   `{pwm: readback[i], rpm: Some/omit}`; on a readback failure fall back to commanded pct for pwm.
4. Spec + DESIGN updates; external review; on-hardware validation (user-gated).

Validation plan:

- Unit: factor decode (M/B/exp incl. signed exponents), RPM formula (raw×100 ⇒ 600/1300/…),
  unavailable-sensor → None, pwm-readback parse, fallback path. `cargo fmt/clippy/build/test --no-run`.
- External review (5 models, iterated) per project process.
- On-hardware (USER-gated, NOT run by assistant): `asrock16-2t query`-style read shows RPMs matching
  `ipmitool sdr type Fan`; confirm tach reads while aiolos holds manual; confirm apply stays within
  timeout. Per the standing constraint, the assistant does not run/install/test.

Artifact impact plan:

- Specs: `anemos-asrock16-2t.spec.md` (RPM shipped; pwm = `0xda` readback). DESIGN.md (asrock §).
- Skills: likely none (no new module/protocol change); confirm at close.
- Docs: README asrock bullet may gain "+ per-fan RPM". SOW lifecycle: single SOW; SOW-0004 stays
  paused until its own user validation.

Open decisions:

- **D1 (confirm): 2a mechanism** — use `Get Sensor Reading Factors` (`0x04/0x23`) per sensor
  (verified) rather than walking the SDR repository. RECOMMEND yes (same robustness, far less code,
  no reservation protocol). If the user prefers the full SDR-repo walk anyway, say so.
- D2: report pwm per-fan from the `0xda` readback (decision 3) — resolved yes.
- D3: skip `ns`/unavailable sensors (FAN*_2, PSU) — resolved yes (probe shows no reading).

## Plan

1. `tech/ipmi` sensor-read + factors helpers (+ unit tests).            [D1]
2. `board.rs` fan-sensor map + cached factors + `read_fan_rpms()` + RPM conversion (+ unit tests).
3. `main.rs` apply: per-fan `{pwm: 0xda readback, rpm}`; failure-tolerant.  [D2/D3]
4. Spec + DESIGN + README; external review; user-gated on-hardware validation.

## Execution Log

### 2026-05-29

- Created as a queued SOW. Evidence gathered, decisions 1a/2a/3 recorded. No code.

### 2026-05-30

- Activated after SOW-0004 merged to master (5ad7828) + paused. Read-only BMC probe: fan sensors
  `0x60..0x67`; `Get Sensor Reading Factors` (`0x04/0x23`) works → M=100/B=0/exps=0 → RPM=raw×100.
  Wrote the Pre-Implementation Gate; refined 2a's mechanism to per-sensor factors (no SDR-repo walk)
  — surfaced as D1 for user confirmation before any IPMI code. No code written.
- D1 confirmed by user ("yes"). Implemented steps 1–4: `tech/ipmi` `read_sensor` /
  `read_sensor_factors` (+ 10/4-bit sign-extend decode), `board.rs` fan map + lazy-cached factors +
  `read_fan_rpms` + pure `fan_rpm`, `main.rs` per-fan `{pwm: 0xda readback, rpm}` read after the
  control decision. Specs/DESIGN/README updated. Build/clippy/fmt/test-compile clean (NOT run).
- External review round 1 (5 models): GLM + MiniMax READY TO SHIP; Kimi ship-after-doc; Qwen NOT-YET
  (factor cache could absorb a first-tick all-fail permanently); **MiMo NOT-YET claiming R_exp/B_exp
  swapped**. The swap claim was DISPROVEN with read-only live data: VOLT_3VSB (0x01) raw 0xab,
  factors `20 02 00 00 00 00 e0`, ipmitool=3.420 V — only upper-nibble=R_exp reproduces 3.42 V (the
  swap gives 342 V). The decode matches IPMI v2.0 + ipmitool; NOT changed. Validated fixes applied:
  per-sensor lazy factors with retry + skip-read-when-missing; negative→omit RPM; clarified
  `reading_available` doc; added the live-verified + negative-B decode tests and fan_rpm edge tests.
- Review round 2 (5 models): GLM + MiniMax + **MiMo** READY TO SHIP (MiMo retracted the swap:
  "proven by live hardware"). Two remaining items, both fixed: Qwen (MEDIUM) first-tick latency →
  factors **prefetched at instance open** (uniform ticks, no 17-call burst) + all observability
  reads use a short 150 ms timeout via new `Ipmi::raw_with_timeout` (control keeps the full 2 s);
  lazy retry kept; new `read_fan_status` bundles duty + RPM under the short timeout. Kimi (1-byte
  `Get Sensor Reading` reply) → `parse_sensor_reading` extracted, lenient on an absent status byte,
  + a contract unit test. Spec updated.
- Review round 3 (5 models): MiniMax/GLM/Qwen READY TO SHIP. Kimi NOT-YET (wanted the per-tick call
  count bounded by construction, not just by the timeout). **MiMo NOT-YET with a new HIGH claiming
  `parse_factors` should mask M/B high bits with `& 0x03` not `& 0xc0`** — DISPROVEN: per IPMI v2.0
  §35.5 the M/B MS 2 bits are byte-4/6 bits [7:6] (`& 0xc0`), matching ipmitool's `__TO_M`; `& 0x03`
  would read the tolerance/accuracy low bits. No live sensor here has a non-zero M/B MSB (all
  M≤255), so it was pinned with a synthetic test (`factors_decode_high_bits_of_m_and_b`). Fixes:
  (a) Kimi's structural bound — `read_fan_rpms` now does ≤1 IPMI call per fan per tick (fetch
  factors OR read tach, never both) → ≤9 calls/tick; (b) `OBS_TIMEOUT` 150→100 ms (Qwen/MiMo
  headroom); (c) `parse_sensor_reading` scanning-disabled test. NOT changed: the M/B mask (correct).
- Review round 4 (5 models): **all 5 READY TO SHIP.** GLM independently verified `& 0xc0` against
  ipmitool source (`__TO_M`); MiMo RETRACTED the mask claim ("`& 0xc0` is correct"); Kimi confirmed
  the ≤9-calls structural bound; MiniMax/Qwen clean. Only residual: cosmetic doc nits (fixed a
  spec-section reference §36.5→§35.5; kept the `FanStatus` alias — it is the `read_fan_status`
  return type, clippy clean). Review converged.

## Validation

**Constraint:** user directed NO runtime testing (production GPUs at risk) — verification is
compile/lint/format + external review only; the user drives all runtime testing.

Acceptance-criteria evidence (compile-time + review; runtime PENDING user):
- RPM reading: `tech/ipmi` `read_sensor`/`read_sensor_factors` + the pure `parse_factors`/
  `parse_sensor_reading`/`fan_rpm` decoders, unit-tested incl. a LIVE-verified case (VOLT_3VSB:
  factors `20 02…e0`, raw 0xab → 3.42 V, matching `ipmitool`), negative-B, high-MSB, sign-extend,
  availability, and raw-edge cases.
- `pwm` from the `0xda` readback (fallback to commanded pct); `rpm` omitted when unreadable.
- Bounded apply: factors prefetched at open; ≤1 IPMI call per fan per tick (≤9 total) at a 100 ms
  observability timeout → ~0.9 s worst case under the 2 s deadline; control keeps the full 2 s.
- Control path / fail-safe UNCHANGED (verified by all 5 reviewers): `regulate`/claim/set/release,
  `restore`, `Drop`, signal/EOF paths — no edits; observability runs only after a successful set.

Tests or equivalent validation (compile-only — NOT executed):
- `cargo fmt --all --check` clean; `cargo clippy --all-targets` clean; `cargo build --release` ok;
  `cargo test --workspace --no-run` compiles all unit tests. Nothing was run.

Real-use evidence:
- PENDING — user-gated on-hardware run. Must confirm: RPMs match `ipmitool sdr type Fan`; tach reads
  correctly while fans are claimed-manual (the read-only probes ran with `nvfd` in control); apply
  latency on the live BMC.

Reviewer findings (5 external models, full-scope, read-only, 4 rounds — iterated to unanimous):
- Round 1: design correct; fixed factor-cache retry, negative→omit RPM, doc, + live-verified tests;
  DISPROVED MiMo's R_exp/B_exp "swap" on live VOLT_3VSB data.
- Rounds 2–3: bounded observability (prefetch at open + short timeout + ≤9-calls/tick structural
  bound) and lenient 1-byte handling; DISPROVED MiMo's `& 0x03` mask claim (IPMI v2.0 §35.5 +
  ipmitool), pinned with a synthetic test.
- Round 4: **5/5 PRODUCTION QUALITY / READY TO SHIP.** GLM verified `& 0xc0` vs ipmitool source;
  MiMo retracted; Kimi confirmed the structural bound. Residual cosmetic doc nits fixed.

Same-failure scan:
- The only other IPMI decode site is the OEM duty path (`0x3a`); unaffected. The new generic sensor
  helpers are the single decode point for `0x04` sensor reads.

Sensitive data gate:
- No secrets. Sensor numbers / IPMI commands are public/standard; inband `/dev/ipmi0`, no BMC
  IP/credentials. Live evidence (voltages/RPMs) is non-sensitive.

Artifact maintenance gate:
- Specs: `anemos-asrock16-2t.spec.md` (RPM shipped; pwm = readback; prefetch/retry/short-timeout).
  Docs: `DESIGN.md` §12, `README.md`. Skills: none needed (no new module/protocol contract). SOW
  lifecycle: SOW-0004 stays paused pending its own user validation.

## Outcome

**Code-complete and review-converged (4 rounds, 5 models, unanimous READY TO SHIP); NOT yet
runtime-validated.** Per the user constraint nothing was run/installed/tested — verification is
compile + lint + format + external review. Two reviewer "blockers" (the R_exp/B_exp swap and the
`& 0x03` mask) were proven false against the IPMI spec, ipmitool, and live hardware, and pinned with
guard tests. The SOW stays **in-progress** until the user runs the on-hardware validation (RPMs vs
`ipmitool`, tach-while-claimed-manual, apply latency) and approves; `nvfd` keeps cooling the GPUs
meanwhile. Not committed (awaiting user).

## Lessons Extracted

Pending.

## Followup

None yet.

## Regression Log

None yet.
