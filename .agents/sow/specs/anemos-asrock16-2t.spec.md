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
- Driving temperature = `max(` GPU temps from `inputs`, own CPU temps (`TEMP_CPU1/2` via IPMI or
  `k10temp` sysfs), own board/DIMM temps (`TEMP_MB1/2`, `TEMP_CARD_SIDE1`, `TEMP_DDR4_*`) `)`.
- Interpolate `/opt/aiolos/etc/asrock16-2t.curve.json`; set fans; report temps + per-fan pwm/rpm.
- **Fan model (default): uniform** — apply `curve(driving_temp)` to all 8 fans. CPU fans following
  the global max is intended (more case airflow when GPUs are hot). *Per-fan curves
  (FAN1/2 by CPU temp, FAN3-8 by max) are a supported future option; config allows it.*

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
While claimed, BMC auto control is OFF for **all** fans including the CPU Noctuas. On `shutdown`
or stdin EOF, the module MUST release (`0x3a 0xd8` ×16 `0x00`) so the BMC reclaims auto control.
Because the kernel can't auto-release on `SIGKILL`, the module should also release on any caught
fatal signal where possible; the orchestrator's graceful stdin-close path is the primary trigger.
HW thermal throttle (~90s °C) is the hardware backstop.

## Config — `/opt/aiolos/etc/asrock16-2t.curve.json`
Driving °C → fan %:
```json
{"40":40,"55":60,"65":80,"75":100}
```

## Implementation note (language/binding)
Rust. IPMI via raw `/dev/ipmi0` ioctl (preferred — zero extra deps; we know the exact `0x3a`
bytes) or thin FFI to `libfreeipmi` (`ipmi_cmd_raw` + sensor reads). CPU temp may come from
`k10temp` sysfs to avoid SDR decoding.

## Acceptance criteria
- `detect` → one board ID.
- Receives GPU temps via `inputs`; computes max with its own sensors.
- Sets all fans via the verified all-manual + non-zero sequence; `0xda` readback matches.
- shutdown/EOF releases to BMC auto; verified by `0xda` + observing fans return to auto.
- Never leaves fans claimed-but-undutied (the ~10–20% minimum trap).
