# Spec: `asrock16-2t` anemos

Status: design. ASRockRack ROME2D16-2T board-fan controller via IPMI. Conforms to
`aiolos-protocol.spec.md`. Hardware findings verified on BMC firmware 3.03 (AST2500).

## Purpose
Drive the 8 motherboard fan headers by temperature. Registry: `asrock16-2t input=nvidia` — the
orchestrator routes GPU temps in; the module also reads its own CPU/board sensors. One `run`
instance (the board).

## Hardware facts
- BMC: ASPEED AST2500, MegaRAC; firmware ≥ 3.03 required for reliable fan duty control.
- 8 controllable headers `FAN1_1..FAN8_1` (SDR 0x60..0x67). Physically: **FAN1/FAN2 are large
  Noctua CPU coolers** (low RPM by size), **FAN3–FAN8 are 120 mm case fans**.
- Inband interface: `/dev/ipmi0`.

## detect
- Emit exactly one board ID: `{"id":"asrock16-2t","type":"board","name":"ROME2D16-2T"}`.

## run <board>
- Driving temperature = `max(` GPU temps from `inputs`, own CPU temps `)`. CPU temps come from
  **`k10temp` sysfs** (`/sys/class/hwmon/*` where `name == k10temp`), reading every `tempN_input`
  across **both** EPYC sockets (labeled via `tempN_label` where present). GPU temps are the
  `"type":"temp"` records relayed in `inputs`.
- *Board/DIMM IPMI SDR temps (`TEMP_MB1/2`, `TEMP_CARD_SIDE1`, `TEMP_DDR4_*`) and per-fan tach RPM
  are a planned enhancement (require SDR repository decoding) — see SOW follow-ups. They are not
  yet in the driving max; CPU + GPU dominate cooling demand on this host.*
- Interpolate `/opt/aiolos/etc/asrock16-2t.curve.json`; set all 8 fans (uniform); read back duty
  via `0xda` and report it as each fan's `pwm`, alongside the temp readings and a `driving` record.
- **Fan model (default): uniform** — apply `curve(driving_temp)` to all 8 fans. CPU fans following
  the global max is intended (more case airflow when GPUs are hot). *Per-fan curves
  (FAN1/2 by CPU temp, FAN3-8 by max) are a supported future option; config allows it.*
- **Duty is always clamped non-zero** (≥1%) so a valid-but-low temperature can never send a zero
  byte (the `0xcc` claimed-but-undutied minimum trap).

## IPMI control (verified) — netfn `0x3a`, inband `/dev/ipmi0`
| Action | Command |
|---|---|
| Claim (all manual) | `0x3a 0xd8` + sixteen `0x01` |
| Set duty | `0x3a 0xd6` + sixteen bytes (per-fan %, `0x64`=100, `0x32`=50) |
| Release (auto) | `0x3a 0xd8` + sixteen `0x00` |
| Query duty | `0x3a 0xda` → sixteen bytes |

**Critical rule:** `0xd6` is accepted **only when all 16 fans are in manual mode AND all 16 duty
bytes are non-zero.** Partial-manual or any zero byte → `0xcc invalid data field` (and unreliable
partial application). Bytes 0–7 = FAN1..FAN8; bytes 8–15 are unused tach slots (set non-zero
anyway). Manual mode without a valid duty drops a fan to its ~10–20% minimum.

## Fail-safe (critical — whole-system cooling)
While claimed, BMC auto control is OFF for **all** fans including the CPU Noctuas. The module
releases (`0x3a 0xd8` ×16 `0x00`) so the BMC reclaims auto control on:
- `shutdown` or stdin EOF (primary triggers; via an RAII guard that also fires on panic), AND
- **whenever the temperature is indeterminable** — all sensor reads fail AND no GPU `inputs`, or
  the curve is missing/empty. The module then reports `status:error` rather than holding manual
  control while blind (**user decision SOW-0001 #9**: never run manual fans without a temperature).

The RAII restore opens a fresh `/dev/ipmi0` handle (independent of the main loop), so it works
even if the main path failed. Because the kernel can't auto-release on `SIGKILL`, the
orchestrator's graceful stdin-close (EOF) path is the primary trigger. HW thermal throttle
(~90s °C) is the hardware backstop.

## Config — `/opt/aiolos/etc/asrock16-2t.curve.json`
Driving °C → fan %:
```json
{"40":40,"55":60,"65":80,"75":100}
```

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
- Receives GPU temps via `inputs`; computes max with its own sensors.
- Sets all fans via the verified all-manual + non-zero sequence; `0xda` readback matches.
- shutdown/EOF releases to BMC auto; verified by `0xda` + observing fans return to auto.
- Never leaves fans claimed-but-undutied (the ~10–20% minimum trap).
