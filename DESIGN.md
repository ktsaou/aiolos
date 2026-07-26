# Aiolos — design & protocol specification

> Aiolos (Αἴολος), keeper of the winds, commands the **anemoi** (ἄνεμοι, the winds).
> Here: **`aiolos`** is an agnostic orchestrator; the **anemoi** are autonomous module
> binaries it spawns, monitors, and drives over a tiny line protocol. The flagship anemoi
> regulate airflow (fans) by temperature — but `aiolos` itself knows nothing about fans,
> GPUs, or IPMI.

Status: **IMPLEMENTED (SOW-0001).** Orchestrator + `nvidia` (NVML) + `rome2d-fans` (IPMI) built and
unit/integration-tested off-hardware. On-hardware validation + cutover from the C `nvfd` remain
operator-gated (see `.agents/sow/`). The authoritative contracts are the specs under
`.agents/sow/specs/`; where this rationale doc and the specs differ, the specs win.

---

## 1. Goal & philosophy

A small, lean, always-on **orchestrator** that:
- spawns and supervises a set of **autonomous module binaries** ("anemoi"),
- talks to each over **single-line JSON messages on stdio** (strict request/response),
- gives each module its own **OS process** (hard isolation — a hung/lost device in one
  module can never stall another),
- relays declared **data flows** between modules (e.g. feed GPU temperatures to the fan module),
- holds **all state** centrally and serves a **read-only status web page**.

`aiolos` is **domain-agnostic**: it does process lifecycle, the protocol, data routing, and
observability. *All* device knowledge (NVML, IPMI, …) lives in the anemoi. Anyone can write a
new anemos in any language that can read stdin and write stdout.

Non-goals: `aiolos` does not parse sensors, know curves, or understand temperature. Those are
module concerns.

---

## 2. Glossary

| Term | Meaning |
|---|---|
| **aiolos** | the orchestrator daemon (Rust) |
| **anemos** / **anemoi** | a module binary / the modules (e.g. `nvidia`, `rome2d-fans`) |
| **instance** | one running process of an anemos, bound to one detected **ID** |
| **registry** | config listing which anemoi to run, and their data wiring |
| **ID** | an opaque, stable identifier a module assigns to a thing it manages (e.g. a GPU UUID) |
| **component** | a reported device/entity with scalar `publishers[]` and controllable `sinks[]` |

---

## 3. Architecture

```
                ┌──────────────────────── aiolos (Rust, std threads) ───────────────────────┐
                │  registry • lifecycle • heartbeat • data routing • state • status webpage  │
                └───┬───────────────────────┬───────────────────────────────┬───────────────┘
        spawn+stdio │                        │ spawn+stdio                    │ HTTP :PORT (read-only)
            ┌───────▼────────┐      ┌────────▼─────────┐               ┌──────▼───────┐
            │ nvidia (detect)│      │ rome2d-fans      │               │  status page │
            └───────┬────────┘      │   (detect)       │               └──────────────┘
        per GPU UUID│               └────────┬─────────┘
        ┌───────────▼───┐ ┌─────────▼──┐   1 board ID
        │ nvidia run ID0│ │nvidia run ID1│  ┌────────▼─────────────┐
        └───────────────┘ └────────────┘    │ rome2d-fans run BOARD│  input=nvidia
                                             └──────────────────────┘
```

- One **detect** process per anemos (persistent — re-queried for hotplug).
- One **run** process per detected ID (persistent — the unit of isolation).
- aiolos drives the cadence (heartbeat); modules are reactive.

---

## 4. Registry

`/opt/aiolos/etc/aiolos.conf` — one anemos per line, optional `key=value` directives:

```
nvidia
nvme                             # NVMe SSD temps (sensor-only; controls nothing)
rome2d-fans  input=nvidia input=nvme   # feed GPU + NVMe temps into this anemos
```

Directives (extensible):
- `input=<anemos>` — aiolos relays the named anemos's fresh signals into this anemos's `apply`
  request (keyed by `module:id`). Repeatable and/or comma-listed for multiple sources.
- `every=<dur>` / `timeout=<dur>` — per-anemos schedule overrides (SOW-0013 decoupled scheduler;
  bare number = seconds). `args=…` is future.

Module binaries live in `/opt/aiolos/bin/<name>`. Per-module config (curves, etc.) in
`/opt/aiolos/etc/<name>.*`.

---

## 5. Protocol

**Transport:** the anemos's **stdin** (requests in) and **stdout** (responses out).
**Framing:** **one line = one complete JSON object.** Request, then response. Strict
half-duplex: read a line → it's your turn. Newline is the only delimiter.

**Hard contract:** stdout carries the protocol **only**. *All* logs/diagnostics → **stderr**
(aiolos captures stderr per-instance for the status page). A stray byte on stdout corrupts the
stream.

**Cadence:** aiolos writes one request, waits for one response within `timeout`. No response in
time → the instance is killed and restarted. Modules never speak unsolicited (except the
optional startup `hello`).

### Messages (each is exactly one line)

**hello** (optional, emitted by the module on startup):
```json
{"hello":{"proto":2,"name":"nvidia","modes":["detect","run"]}}
```

**detect** (to a `detect` process; re-sent periodically for hotplug):
```json
→ {"cmd":"detect"}
← {"status":"ok",
    "units":[{"id":"GPU-5f2…","labels":{"type":"gpu","name":"gpu0"}}],
    "components":[{"id":"GPU-5f2…:thermal","unit":"GPU-5f2…",
      "labels":{"type":"temperature","name":"thermal"}}],
    "signals":[{"id":"GPU-5f2…:thermal:temp","component":"GPU-5f2…:thermal",
      "role":"producer","uom":"C","labels":{"type":"temperature","name":"temperature"}}]}
```

**info / collect** (SDK one-shot companion mode, not used by aiolos heartbeat): `<module> info [ID]`
opens devices read-only (`OpenMode::Observe`), calls `Device::collect`, and emits the same
detect-shaped response with live signal values. It must not claim, set, release, or restore
hardware.

**apply** (to a `run <ID>` process when the anemos is due — aiolos wakes every `base_tick` and
dispatches each idle, due anemos (SOW-0013); `inputs` present only if `input=` wired —
each peer id maps to that peer's fresh signal array, relayed uninterpreted):
```json
→ {"cmd":"apply","inputs":{"nvidia:GPU-5f2…":[
     {"id":"GPU-5f2…:thermal:temp","component":"GPU-5f2…:thermal",
      "role":"producer","value":63,"uom":"C","labels":{"type":"temperature"}}]}}
← {"status":"ok","units":[…],"components":[…],"signals":[…]}
```
On trouble: `← {"status":"error","error":"device lost"}` (aiolos logs/counts; repeated → restart).

**shutdown** (graceful): `→ {"cmd":"shutdown"}` → module restores its device to safe/auto,
replies `{"status":"ok"}`, exits. **stdin EOF triggers the identical restore+exit** (covers
aiolos crashing).

The `run` instance knows its own ID from argv, so `apply` need not repeat it.

---

## 6. Data routing (`input=`)

aiolos keeps a **blackboard**: the fresh `signals` reported by every instance. For an anemos
configured `input=X [Y …]`, aiolos extracts every named source instance's signals and includes
them as `inputs` (keyed by `module:id`, so the consumer can attribute each signal to its source
module) in this anemos's next `apply`. aiolos does **not** interpret the values — it only relays.
The consumer decides how to use them (max, per-zone, per-source, …). This is how GPU and NVMe temps
reach the fan module while aiolos stays agnostic.

Timing: `inputs` carry each source's **most recent successful, non-empty completed** `apply` signals
(one cycle stale, never blocking on a peer) — irrelevant for thermal mass, and it keeps every
instance independent (no ordering dependency). A source `error`/`fatal` or authoritative
`ok`-empty report prunes its entry immediately, so consumers see telemetry loss rather than stale
last-good values. Under the decoupled scheduler (SOW-0013) sources and consumers run on their own
cadences; a consumer always sees the most recent fresh values the blackboard holds.

---

## 7. Lifecycle & failure handling

1. **Start:** read registry → for each anemos, spawn its `detect` process.
2. **Detect/reconcile** (every `detect_every`, e.g. 10 s): send `detect` → diff returned IDs
   against running instances → spawn new `run <ID>`, kill vanished ones. (Handles a GPU
   dropping off the bus and returning.)
3. **Scheduler wake:** for each due/idle instance, write `apply` (with routed `inputs`), then the
   worker polls its stdout for one line within that module's `timeout`. Collect components
   into the blackboard. Fan-out then collect — **no instance waits on another**.
4. **Timeout/exit:** missed deadline or process exit → `SIGKILL` (if needed), restore handled by
   the module's own EOF path, then respawn next cycle. Backoff on crash-looping.
5. **aiolos shutdown (SIGTERM):** close every instance's stdin → each restores its device →
   reap → exit.

**Supervision is error-driven, not inference-driven.** Modules declare faults explicitly via the
response `status` (`ok`/`error`/`fatal`) with a reason; the orchestrator reacts to the declared
status and surfaces it (per-module detect health + per-instance status on the status page). It does
NOT infer faults from empty data, a module exiting, or silence — those would make the supervisor
decide blind. Crash/timeout detection (step 4) is only a last-resort backstop for a module too
broken to report; an `error` keeps existing instances (a transient fault ≠ "no devices"), a `fatal`
retries on a long backoff (never permanently abandoned). See the protocol/orchestrator specs.

**Isolation guarantee:** each `run` instance is a separate process. A wedged syscall in one
cannot block aiolos or siblings; the worst case is that instance missing a tick and being
restarted. (A true uninterruptible-D-state hang is unkillable by anyone, but remains harmless to
others — it's orphaned, siblings keep ticking.)

**Fail-safe direction:** a module's curve should be *more aggressive* than the device's firmware
default, so "module dies → firmware/BMC reclaims control" is always the *safe* direction.

---

## 8. State & status web page

aiolos holds: registry, per-anemos detect results, per-instance last entities + status + last
error + restart count + last-seen time, captured stderr tail. It serves a **read-only** HTTP
status page (bind localhost by default) rendering all of the above — live components, which
instances are healthy, recent errors. Small, dependency-light (hand-rolled or `tiny_http`).

---

## 9. Repo & install layout

```
~/src/aiolos.git/                 # source (github.com/ktsaou/aiolos, public)
  DESIGN.md                       # this document
  aiolos/                         # the orchestrator crate (Rust)
  anemoi/
    nvidia/                       # nvidia anemos crate (Rust)
    rome2d-fans/                  # rome2d-fans anemos (Rust; IPMI via /dev/ipmi0 or libfreeipmi FFI)
    nvme/                         # nvme anemos (Rust; sensor-only NVMe temps via sysfs)
  systemd/aiolos.service
  packaging/                      # install.sh / update.sh

/opt/aiolos/                      # install root
  bin/aiolos
  bin/nvidia
  bin/rome2d-fans
  bin/nvme                        # sensor-only (no curve file)
  etc/aiolos.conf                 # registry
  etc/nvidia.curve.json           # per-module config
  etc/rome2d-fans.curve.json
```
systemd: `aiolos.service` (Type=simple, Restart=on-failure). The existing C `nvfd` keeps cooling
the GPUs until aiolos is built, tested, and cut over.

---

## 10. Language

- **aiolos**: Rust, **std threads + blocking I/O** (no async/tokio needed at this scale),
  `serde_json`, minimal HTTP. Lean (no GC; ~low-MB binary, ~few-MB RSS), memory-safe supervisor,
  `cargo` build (no cmake/headers). Chosen for lean + safe.
- **nvidia anemos**: Rust, `nvml-wrapper`.
- **rome2d-fans anemos**: Rust. IPMI via raw `/dev/ipmi0` ioctl (preferred — zero extra deps) or
  thin FFI to `libfreeipmi`. CPU temps may instead come from `k10temp` sysfs (trivial).

The protocol is language-agnostic; any anemos may be written in any language later.

---

## 11. Anemos: `nvidia`

- **detect:** enumerate GPUs by **UUID** (stable across renumbering); emit one `found` per GPU.
- **run <UUID>:** own `nvmlInit`; each `apply` → read this GPU's temp, interpolate
  `etc/nvidia.curve.json`, set the GPU's onboard fans (NVML `SetFanSpeed`), report
  `components:[{id,class,publishers:[…],sinks:[…]}]`.
- **fail-safe:** EOF/shutdown → `SetDefaultFanSpeed` (firmware auto).
- Curve (current production value): linear 0–80 °C → 0–100 %.
- Fork-safety: orchestrator never holds NVML; each instance inits its own.

---

## 12. Anemos: `rome2d-fans` (ASRockRack ROME2D16-2T, BMC AST2500, fw ≥ 3.03)

- **detect:** emit **one** ID (the board).
- **input=nvidia input=nvme:** receives GPU + NVMe temps from aiolos (attributed by `module:id`).
- **run <BOARD>:** driving_temp = `max(`GPU + NVMe temps from inputs, own CPU temps, own MB/board
  temps`)`; interpolate `etc/rome2d-fans.curve.json`; set all 8 board fans; report a board
  component. GPU/NVMe temps are not re-published; they appear as sink `driven_by` metadata.
- **CPU fans are real:** FAN1/FAN2 are large **Noctua CPU coolers** (low RPM by size), FAN3–FAN8
  are 120 mm case fans. User decision: all fans follow the global max (CPU fans speeding up on GPU
  heat is desirable). Default **uniform** duty = curve(driving_temp). *(Open: optional per-fan
  curves later — FAN1/2 by CPU temp, FAN3-8 by max — config supports it; default uniform.)*
- **Source-matched case overlay (optional):** `rome2d-fans.case.policy.json` can match
  routed/local numeric producer signals independently in multiple rules, reduce each rule by max,
  and apply one curve per rule. FAN3–FAN8 use the maximum of their existing baseline and every
  overlay request; FAN1/FAN2 keep their baseline behavior. The packaged disabled policy demonstrates
  a required NVMe rule whose
  `{"50":30,"70":100,"sensitivity":1.0}` curve reaches 100% immediately at 70 C. Configured
  policy/curve faults or missing required telemetry fail the case fans high to 100%.

**IPMI fan control (verified) — netfn 0x3a, inband /dev/ipmi0:**
- Claim (all manual): `0x3a 0xd8` + sixteen `0x01`
- Set duty: `0x3a 0xd6` + sixteen bytes (per-fan %, `0x64`=100, `0x32`=50). **Reliable ONLY when
  all 16 are manual, all duty bytes are non-zero, AND bytes 8–15 exactly mirror FAN1..FAN8 in
  bytes 0–7.** An `0x01`/low non-mirrored tail is rejected with `0xcc`.
- Release (fail-safe): `0x3a 0xd8` + sixteen `0x00`  (BMC reclaims auto)
- Query duty: `0x3a 0xda`
- Per-fan RPM (SOW-0005): standard IPMI on `FAN1_1..FAN8_1` (sensors `0x60..0x67`) — cache the
  conversion factors via `Get Sensor Reading Factors` (`0x04/0x23`) at open, then `Get Sensor
  Reading` (`0x04/0x2d`) each tick (no SDR-repo walk). Read-only; reported as each fan's `rpm`.
- Temps: `TEMP_CPU1/2`, `TEMP_MB1/2`, `TEMP_CARD_SIDE1`, `TEMP_DDR4_*` via IPMI sensor reads
  (or CPU temp via `k10temp` sysfs).
- **fail-safe:** EOF/shutdown → release (all `0xd8`=0x00). Critical: while claimed, the BMC's
  auto control is OFF for *all* fans incl CPU; release returns everything to the BMC.

---

## 13. Config — curves + smoothing

`etc/<anemos>.curve.json` — temperature → duty %, linear-interpolated, clamped, hold-outside, plus
an optional `"sensitivity"` knob (EMA α, 0–1) for noise smoothing. Per-module defaults: `nvidia`
`{"30":30,"80":100}`; `rome2d-fans` `{"50":30,"80":100}` — the board idles warmer (DIMM/NVMe/board/
LAN ~45–50 °C), so it holds the 30% floor until 50 °C, then ramps (GPU heat still drives it up via
the routed max). Example:
```json
{"30": 30, "80": 100, "sensitivity": 0.5}
```
- **Floor = the lowest curve point (30% on both modules); ceiling 100%.** The curve NEVER yields
  below its floor — a wrong/low sensor reading can't stop or minimise the fans in manual mode. (30%
  matches the board's firmware idle; lowered from the original 35% — supersedes SOW-0001 #16.)
- **`sensitivity`** (EMA α): lower = smoother / less reactive to noisy spikes; higher = more
  responsive. Live-reloaded each tick (no restart). A single bad reading is diluted to ≈ α·Δ.
- The file is re-read every tick, so curve and sensitivity edits take effect on the next tick.

Optional case policies for `rome2d-fans` and `it87` live beside the main curve as
`<anemos>.case.policy.json`. Each rule has exact-list `match` fields (`module`, `instance`,
signal `id`, `component`, `uom`, `labels`), a relative-basename `curve`, and optional `required`.
Populated fields are ANDed, alternatives within one list are ORed, and a signal may match multiple
rules. Rule maxima pass through independent curve/EMA/deadband state; rule duties combine by max,
then max-compose with the existing case-fan duty. Local signals are matched as `self:self`. Policy
files and all referenced curves are validated every tick. Unlike baseline curve live edits, a
configured overlay fault intentionally uses no last-good fallback: affected case fans command 100%
and the module returns an authoritative warning.

---

## 14. Open decisions (defaults proposed)

| # | Decision | Default |
|---|---|---|
| 1 | `base_tick` / per-anemos `every` / `timeout` (SOW-0013 decoupled scheduler) | 100 ms / 1 s / 5 s |
| 2 | `detect_every` (hotplug re-scan) | 10 s |
| 3 | rome2d-fans fan model | uniform curve(max) over all 8 (per-fan optional later) |
| 4 | nvidia curve | 0–80 °C → 0–100 % (as today) |
| 5 | rome2d-fans curve | 40→40, 55→60, 65→80, 75→100 |
| 6 | sensor set for rome2d-fans max | GPU(inputs) + CPU + MB + card-side + DIMM (exclude TEMP_LAN? it floors ~45 °C) |
| 7 | status page bind | `0.0.0.0:9876` (SOW-0001 decision; configurable, `127.0.0.1` to restrict) |

---

## 15. Extensibility

New behaviour = new anemos binary, any language, that implements detect/collect/apply/shutdown over
the line protocol and is added to the registry. The `nvme` sensor anemos (SOW-0004) is a worked example
of a **sensor-only** module — it reports temperatures and controls nothing, routed into the fan
controller via `input=nvme`. Further examples: a `power-cap` anemos, an `alert` anemos that emails
on threshold. aiolos needs no changes.
