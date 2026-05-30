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
- Interpolate the curve and set the 8 fans (uniform OR per-zone, see **Fan model** below); then
  (observability only, read AFTER the control decision) report each fan's `{pwm, rpm}`: `pwm` from
  the `0xda` duty readback (falling back to the commanded duty if unavailable), `rpm` from the fan's
  tachometer (omitted if the sensor is unreadable). A **faulted** fan additionally carries
  `"fault":true` (see **Fan-fault detection**). Reported alongside the temp readings and a `driving`
  record (`{"mode":"uniform","raw","temp","pct"}` or `{"mode":"zone","cpu_raw","cpu_temp","cpu_pct",
  "case_raw","case_temp","case_pct"}`).
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
- **Fan model — default uniform, optional per-zone (SOW-0010):**
  - **Uniform (default):** apply `curve(max(all routed inputs, CPU))` to all 8 fans via the single
    `asrock16-2t.curve.json`. Back-compatible; the shipped config is unchanged.
  - **Per-zone:** when BOTH optional zone curve files load a non-empty curve, the board splits into
    two independently-curved zones, each with its own `anemos::Controller` (own EMA/deadband/
    sensitivity):
    - **CPU zone** — FAN1/FAN2 (Noctua CPU coolers) driven by `max(CPU k10temp)` via
      `asrock16-2t.cpu.curve.json`.
    - **Case zone** — FAN3..FAN8 (120 mm case fans) driven by `max(all routed inputs)` (GPU + NVMe +
      any future routed source) via `asrock16-2t.case.curve.json`. **CPU temp is deliberately
      excluded from the case max** so a CPU-only spike does not blast the case fans.
  - The per-fan duties go out through the same `0xd6` command (bytes 0–7 = FAN1..FAN8; see the
    critical rule below). The mode is re-decided **live every tick** from the presence of the two
    zone files (a pure config read; no control side effects), so zoning can be enabled/disabled
    without a restart. The two zone files sit next to the main curve, derived by suffix from the
    resolved main path so `$AIOLOS_ETC_DIR` is honoured. **Per-zone fail-safe:** because one `0xd6`
    sets all 8 fans at once, if EITHER zone has no driving temp or no usable curve the WHOLE board
    releases to BMC auto (identical to the uniform fail-safe — we cannot release one zone and hold
    the other).
- **Duty is always clamped non-zero** (≥1%) so a valid-but-low temperature can never send a zero
  byte (the `0xcc` claimed-but-undutied minimum trap). In a per-fan `0xd6`, the 8 unused tach slots
  (bytes 8–15) are held at `0x01`.

### Fan-fault detection (SOW-0008)
Module-local stall detection using the per-fan RPM already read each tick. A fan is *faulted* when,
for `FAULT_TICKS` (3) consecutive ticks past a `SPINUP_GRACE_TICKS` (2) spin-up grace, it is
commanded ≥ `FAULT_MIN_DUTY` (20 %) yet its tachometer reads a **present** RPM ≤ `FAULT_RPM_MAX`
(100) — i.e. driven but not spinning. Rules:
- An **unreadable** RPM (`None`) is NOT a fault (sensor read failing ≠ dead fan); it holds the
  per-fan state (neither confirms nor clears). A present RPM above the threshold clears immediately.
- A fan commanded below the duty threshold can legitimately read ≈0 and never faults (state resets).
- **Surfacing:** the faulted fan's `fan` reading carries `"fault":true` and a `tracing::warn!` names
  it. Richer delivery (webhook / Netdata `aiolos_fan_rpm==0` alarm, ties to the metrics SOW) is a
  documented **follow-on**, not implemented here.
- **Compensation:** on a confirmed fault, the surviving (non-faulted) fans **in the same zone** are
  commanded 100 % on subsequent ticks (more airflow is always safe); the dead fan keeps its normal
  commanded duty (never 0). This works in both uniform and per-zone mode (in uniform mode the two
  zones still define which siblings get boosted). The detector evaluates the duty actually
  *commanded* (post-compensation) against the measured RPM.

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
{"50":30,"80":100,"sensitivity":0.5}
```
Default: ≤50 °C → 30 %, ≥80 °C → 100 %, linear between — a **30 % floor** so a wrong low reading can
never stop/minimise the fans. The board floors until **50 °C** (not 30 °C like the GPU curve)
because its driving sensors — DIMM/NVMe/board/LAN — idle at ~45–50 °C, so flooring at 30 °C would
keep the chassis fans needlessly high. GPU heat still drives the fans up via the routed `max`.
(30 % matches the board's firmware idle; supersedes SOW-0001 #16.) `sensitivity` (0.5 default) is reloaded every
tick.

### Optional per-zone curves (SOW-0010) — back-compatible
The single `asrock16-2t.curve.json` above is the **uniform / fallback** curve and the shipped
default. To split the board into the CPU-cooler and case-fan zones, drop **both** of these next to
it (each a normal curve file with its own optional `sensitivity`):
- `asrock16-2t.cpu.curve.json` — drives FAN1/FAN2 (Noctua CPU coolers) from CPU temp.
- `asrock16-2t.case.curve.json` — drives FAN3..FAN8 (case fans) from `max(all routed inputs)`.

Zone mode activates only when BOTH load a non-empty curve; otherwise the uniform curve drives all 8
fans. The decision is live (re-read each tick), so adding/removing the files toggles zoning without a
restart. Paths derive from the resolved main-curve path by suffix, so `$AIOLOS_ETC_DIR` is honoured.
Example CPU-zone curve (Noctuas can floor lower than the case fans): `{"40":30,"75":100,"sensitivity":0.5}`.

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
- **Per-zone (SOW-0010):** with both zone curve files present, a CPU-only load raises FAN1/2 without
  over-driving FAN3–8, and GPU/NVMe heat raises FAN3–8 without over-driving FAN1/2; with the files
  absent, behaviour is byte-for-byte the uniform default. If either zone's temp/curve is
  indeterminable, the whole board releases to BMC auto.
- **Fan-fault (SOW-0008):** a fan commanded ≥ 20 % reading ≈0 RPM for 3 consecutive ticks (past a
  2-tick spin-up grace) is flagged `"fault":true` + warned, and its surviving zone siblings are
  boosted to 100 %; an unreadable tach or a lightly-commanded fan never false-positives.
