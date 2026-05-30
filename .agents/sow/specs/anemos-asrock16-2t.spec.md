# Spec: `asrock16-2t` anemos

Status: design. ASRockRack ROME2D16-2T board-fan controller via IPMI. Conforms to
`aiolos-protocol.spec.md`. Hardware findings verified on BMC firmware 3.03 (AST2500).

## Purpose
Drive the 8 motherboard fan headers by temperature. Registry:
`asrock16-2t input=nvidia input=nvme` — the orchestrator routes GPU and NVMe temps in; the module
also reads its own CPU/board sensors. One `run` instance (the board).

## Hardware facts
- BMC: ASPEED AST2500, MegaRAC; firmware ≥ 3.03 required for reliable fan duty control.
- 8 controllable headers `FAN1_1..FAN8_1` (SDR 0x60..0x67). Physically: **FAN1/FAN2 are large
  Noctua CPU coolers** (low RPM by size), **FAN3–FAN8 are 120 mm case fans**.
- Inband interface: `/dev/ipmi0`.

## detect
- Emit exactly one board ID: `{"id":"asrock16-2t","type":"board","name":"ROME2D16-2T"}`.

## run <board>
- Driving temperature = `max(` all routed `inputs` temps, own CPU temps `)`. Routed temps arrive
  keyed by `module:id`, so the module attributes them by source module: GPU temps (`nvidia:*`) and
  NVMe temps (`nvme:*`) are reported under distinct `temp` labels (`GPU`, `NVMe`). The driving max
  uses **all** routed temps (robust if more sources are wired later) plus CPU. CPU temps come from
  **`k10temp` sysfs** (`/sys/class/hwmon/*` where `name == k10temp`), reading every `tempN_input`
  across **both** EPYC sockets (labeled via `tempN_label` where present).
- NVMe temps are relayed by the `nvme` sensor anemos (`input=nvme`): hot SSDs raise the board fans.
  Routed temps are the `"type":"temp"` records relayed in `inputs`.
- *Board/DIMM IPMI SDR temps (`TEMP_MB1/2`, `TEMP_CARD_SIDE1`, `TEMP_DDR4_*`) remain a planned
  enhancement; they are not in the driving max — CPU + GPU + NVMe dominate cooling demand here.*
- Interpolate `/opt/aiolos/etc/asrock16-2t.curve.json`; set all 8 fans (uniform); then (observability
  only, read AFTER the control decision) report each fan's `{pwm, rpm}`: `pwm` from the `0xda` duty
  readback (falling back to the commanded pct if unavailable), `rpm` from the fan's tachometer
  (omitted if the sensor is unreadable). Reported alongside the temp readings and a `driving` record.
- **Per-fan RPM (SOW-0005):** read via standard IPMI sensor commands on `FAN1_1..FAN8_1` (sensor
  numbers `0x60..0x67`). The linear conversion factors come from `Get Sensor Reading Factors`
  (`0x04/0x23`) — **prefetched at instance open** and cached (constant for these linear sensors;
  verified on this board: M=100, B=0, exponents 0 → `RPM = raw·100`; a fan whose prefetch failed is
  retried lazily on later ticks) — then `Get Sensor Reading` (`0x04/0x2d`) each tick. This avoids
  walking the SDR repository. All observability reads (RPM + the `0xda` duty readback) use a **short
  timeout** so a slow/unresponsive BMC degrades a reading to "absent" rather than blowing the apply
  deadline; control commands keep the full IPMI timeout. `FAN*_2`/`FAN_PSU*` report "No Reading" and
  are skipped. The raw byte is treated as unsigned (universal for fan tach). RPM is read-only; a
  sensor failure never affects fan control or fails the tick.
- **Fan model (default): uniform** — apply `curve(driving_temp)` to all 8 fans. CPU fans following
  the global max is intended (more case airflow when GPUs are hot). *Per-fan curves
  (FAN1/2 by CPU temp, FAN3-8 by max) are a supported future option; config allows it.*
- **Duty is always clamped non-zero** (≥1%) so a valid-but-low temperature can never send a zero
  byte (the `0xcc` claimed-but-undutied minimum trap).

## IPMI control (verified) — OEM netfn `0x3a` + standard sensor netfn `0x04`, inband `/dev/ipmi0`
| Action | Command |
|---|---|
| Claim (all manual) | `0x3a 0xd8` + sixteen `0x01` |
| Set duty | `0x3a 0xd6` + sixteen bytes (per-fan %, `0x64`=100, `0x32`=50) |
| Release (auto) | `0x3a 0xd8` + sixteen `0x00` |
| Query duty | `0x3a 0xda` → sixteen bytes |
| Get fan reading | `0x04 0x2d <sensor>` → `[raw, status, …]` (sensor `0x60..0x67`) |
| Get fan factors | `0x04 0x23 <sensor> 0x00` → `[next, M_lsb, M_msb/tol, B_lsb, B_msb/acc, acc/dir, Rexp/Bexp]` |

**Critical rule:** `0xd6` is accepted **only when all 16 fans are in manual mode AND all 16 duty
bytes are non-zero.** Partial-manual or any zero byte → `0xcc invalid data field` (and unreliable
partial application). Bytes 0–7 = FAN1..FAN8; bytes 8–15 are unused tach slots (set non-zero
anyway). Manual mode without a valid duty drops a fan to its ~10–20% minimum.

## Modes
`detect` · `run <board>` · `restore` (one-shot: release all fans to BMC auto and exit; idempotent;
called by `aiolos restore`).

## Fail-safe (critical — whole-system cooling)
While claimed, BMC auto control is OFF for **all** fans including the CPU Noctuas. The module
releases (`0x3a 0xd8` ×16 `0x00`) so the BMC reclaims auto control on:
- `shutdown`, stdin EOF, OR `SIGTERM`/`SIGINT` (the module catches the signal itself — it does not
  rely on the parent to kill it; via an RAII guard that also fires on panic), AND
- **whenever the temperature is indeterminable** — all sensor reads fail AND no GPU `inputs`, or
  the curve is missing/empty. The module then reports `status:error` rather than holding manual
  control while blind (**user decision SOW-0001 #9**: never run manual fans without a temperature),
  AND
- **whenever a duty cannot be set** — if `0xd6` persistently fails (even after re-claim), the module
  releases to BMC auto rather than holding manual-but-frozen (never leave the fans claimed without a
  fresh duty).

The RAII restore opens a fresh `/dev/ipmi0` handle (independent of the main loop), so it works even
if the main path failed, and it disarms only on a SUCCESSFUL release (a failed release is retried on
drop). Because the kernel can't auto-release on `SIGKILL`, `aiolos restore` (systemd ExecStopPost) is
the net for a hard kill. HW thermal throttle (~90 °C) is the hardware backstop.

## Config — `/opt/aiolos/etc/asrock16-2t.curve.json`
Driving °C → fan %, linear-interpolated, clamped, hold-outside, plus a `sensitivity` key (the live
EMA α, not a curve point):
```json
{"35":35,"80":100,"sensitivity":0.5}
```
Default (decision SOW-0001 #16): ≤35 °C → 35 %, ≥80 °C → 100 %, linear between — a **35 % floor** so
a wrong low reading can never stop/minimise the fans. `sensitivity` (0.5 default) is reloaded every
tick. (Per-fan/per-zone curves remain a possible future extension; the shipped model is uniform.)

## Implementation note (language/binding)
Rust, IPMI via raw `/dev/ipmi0` ioctl — zero extra deps (user decision SOW-0001 #6). The Linux
char interface is asynchronous: `IPMICTL_SEND_COMMAND` (`_IOR(IPMI_IOC_MAGIC,13,struct ipmi_req)`
= `0x8028690D`) queues the request; then `poll(POLLIN)` + `IPMICTL_RECEIVE_MSG_TRUNC`
(`_IOWR(IPMI_IOC_MAGIC,11,struct ipmi_recv)` = `0xC030690B`) fetches the reply. Address =
`ipmi_system_interface_addr{addr_type=0x0c, channel=0x0f, lun=0}`, `addr_len=8`. The response's
first data byte is the completion code (`0x00` = OK); replies are correlated by `recv_type==1`
and a per-request `msgid`. The `repr(C)` structs are sized 8/16/40/48 bytes with compile-time
asserts, and the two ioctl numbers are asserted against the values above. CPU temp comes from
`k10temp` sysfs to avoid SDR decoding.

## Acceptance criteria
- `detect` → one board ID.
- Receives GPU + NVMe temps via `inputs` (attributed by `module:id`); computes the driving max with
  its own CPU sensors; reports distinct `temp/GPU` and `temp/NVMe` readings.
- Sets all fans via the verified all-manual + non-zero sequence; `0xda` readback matches.
- Each fan reading carries `pwm` (from the `0xda` readback) and, when the tach is readable, `rpm`
  (matching `ipmitool sdr type Fan`); an unreadable sensor omits `rpm` and never fails the tick.
- shutdown/EOF/SIGTERM each release to BMC auto; verified by `0xda` + observing fans return to auto.
- A persistent duty-set failure releases to BMC auto (never holds manual-but-frozen).
- `asrock16-2t restore` releases to BMC auto and is idempotent.
- Never leaves fans claimed-but-undutied (the ~10–20% minimum trap).
