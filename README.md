# aiolos

**A lean, domain-agnostic orchestrator for autonomous device modules — with hard process isolation.**

> In Greek myth, **Aiolos** (Αἴολος) is the keeper of the winds; the **anemoi** (ἄνεμοι) are the
> winds themselves. Here, `aiolos` is a small Rust orchestrator that spawns, supervises, and feeds
> data between **anemoi** — independent module processes that each command one device or signal.
> The flagship anemoi regulate airflow (fans) by temperature — but `aiolos` itself knows nothing
> about fans, GPUs, or IPMI.

aiolos was built to keep a dual-GPU, dual-CPU server cool: GPU fans by NVML and chassis fans by
inband IPMI, driven by a shared temperature→duty curve. But the orchestrator is generic — every bit
of device knowledge lives in a module, and a module can be written for *anything* that has a state
to read and an output to set.

---

## Why it's built this way

- **Hard process isolation.** Each module instance is its own OS process. A hung GPU, a wedged BMC,
  or a crashing module can never stall the orchestrator or its siblings — the worst case is one
  device missing a tick and being restarted. This is the core guarantee.
- **Fail-safe by construction.** A module's controlled state is always *more aggressive* than the
  device's firmware default, so "module dies → firmware/BMC reclaims control" is always the safe
  direction. Modules restore their device on shutdown, on stdin EOF (the parent died), and on
  `SIGTERM`/`SIGINT` — and `aiolos restore` is a belt-and-suspenders net for a hard kill.
- **Lean.** Rust with std threads and blocking I/O — **no async runtime**, no GC. Low-MB binaries,
  a few MB of RSS.
- **Language-agnostic modules.** Modules talk to the orchestrator over a one-line-JSON stdio
  protocol, so a module can be written in any language. (Rust modules get a zero-boilerplate SDK.)

---

## How it works

```
                         ┌──────────────────────────── aiolos (orchestrator) ───────────────────────────┐
                         │  • spawns one `detect` process + one `run` instance per device                │
                         │  • ticks every instance each heartbeat, collects readings                     │
   one-line JSON         │  • routes declared data flows between modules (the "blackboard")              │
   over stdio            │  • supervises: restart, backoff, graceful shutdown, read-only status page     │
                         └───────┬───────────────────────────────────────────────────┬──────────────────┘
                                 │ stdin: {"cmd":"apply", "inputs":{…}}                │
                                 │ stdout: {"status":"ok","readings":[…]}              │
                    ┌────────────▼────────────┐                        ┌──────────────▼──────────────┐
                    │  nvidia  (anemos)        │   GPU temps routed     │  asrock16-2t  (anemos)       │
                    │  per-GPU fans via NVML   │ ─────────────────────▶ │  8 chassis fans via IPMI,    │
                    │                          │     as `input=`        │  driven by max(GPU, CPU)     │
                    └──────────────────────────┘                        └──────────────────────────────┘
```

Each module is launched in three modes:

| Mode | Purpose |
|------|---------|
| `<module> detect` | report the IDs it manages (e.g. one entry per GPU, by UUID) |
| `<module> run <ID>` | bound to one device; on each heartbeat read sensors, apply the curve, report readings |
| `<module> restore` | one-shot: hand every device back to firmware/auto and exit (used by `aiolos restore`) |

**stdout is protocol-only; all logs go to stderr** (captured into the orchestrator's journal). Reads
and writes on every module pipe are deadline-bounded, so a module that floods or stalls is killed
within the timeout — never wedging anything else.

---

## The shipped anemoi

- **`nvidia`** — per-GPU onboard fan control via NVML (`nvml-wrapper`). One `run` instance per GPU,
  keyed by stable UUID. Restores firmware fan control on exit (NVML manual control persists
  otherwise).
- **`nvme`** — NVMe SSD temperatures via sysfs. A **sensor-only** anemos: one `run` instance per
  drive (keyed by stable serial), it reports per-drive temps and controls nothing. Routed into the
  fan controller so hot disks raise the chassis fans; it lives in its own process because an NVMe
  temp read can block on a wedged controller.
- **`asrock16-2t`** — ASRockRack ROME2D16-2T chassis fans via **inband IPMI** (raw `/dev/ipmi0`
  ioctls, zero extra deps). Driven by `max(GPU temps from nvidia, NVMe temps from nvme, its own CPU
  temps via k10temp)`. Releases to BMC auto control on exit, and whenever a temperature is
  indeterminable.

In practice the two together hold a heavily-loaded dual-RTX-PRO-6000 box in the low-to-mid 60s °C at
70–85% fan — with headroom to spare.

---

## The fan curve

Each module reads its curve **on every tick** (edit it live; no restart). A curve maps temperature
to fan duty, linear-interpolated and clamped, with an EMA "sensitivity" knob:

```json
{ "35": 35, "80": 100, "sensitivity": 0.5 }
```

- **≤ 35 °C → 35 %**, **≥ 80 °C → 100 %**, linear between. The 35 % floor means a single bad/low
  sensor reading can never stop or minimise the fans.
- **`sensitivity`** is the EMA weight (0–1). Lower = smoother / less reactive to noisy spikes
  (e.g. jittery AMD `Tctl`); higher = snappier. A small deadband suppresses fan "hunting".

| Temp | Duty |
|-----:|-----:|
| ≤35 °C | 35% |
| 50 °C | 57% |
| 65 °C | 78% |
| ≥80 °C | 100% |

---

## Install & run

Requires a recent Rust toolchain. Installs to `/opt/aiolos/` (binaries in `bin/`, config in `etc/`).

```bash
git clone https://github.com/ktsaou/aiolos
cd aiolos
sudo ./packaging/install.sh        # build + install binaries, default config (never clobbered), systemd unit
```

`install.sh` does **not** start the service or stop any existing cooling daemon — cutover is a
deliberate, operator-gated step:

```bash
# review /opt/aiolos/etc/aiolos.conf and the *.curve.json files, then:
sudo systemctl enable --now aiolos
journalctl --namespace=aiolos -f          # logs (orchestrator + every module's decisions)
xdg-open http://localhost:9876/           # read-only status page
```

Update in place (rebuild + restart only if running, config untouched): `sudo ./packaging/update.sh`.

The systemd unit logs to a dedicated journal namespace (`journalctl --namespace=aiolos`), and runs
`aiolos restore` on stop as a fail-safe — no module names are hardcoded in the unit.

---

## Configuration

**Registry + globals** — `/opt/aiolos/etc/aiolos.conf`:

```ini
# globals (defaults shown)
# tick=3                    # heartbeat period (s)
# timeout=2                 # max wait for an apply reply (s; must be < tick)
# detect_every=10           # hotplug re-detect period (s)
# status_bind=0.0.0.0:9876  # read-only status page (127.0.0.1:9876 to restrict)

# modules: `<binary> [input=<peer> ...]`
nvidia
nvme                                  # NVMe SSD temps (sensor-only; controls nothing)
asrock16-2t  input=nvidia input=nvme  # chassis fans follow max(GPU, NVMe, own CPU sensors)
```

`input=<peer>` wires one module's last readings into another's `apply` (one heartbeat stale), keyed
by `module:id` so the consumer can tell sources apart. Repeat it (or use a comma list) for multiple
sources. The orchestrator relays them verbatim and stays agnostic about what they mean.

**Curves** — `/opt/aiolos/etc/<module>.curve.json` (see above).

---

## Architecture: three reuse levels

A new anemos carries only its device logic; everything else is shared and maintained once.

```
protocol/            wire types (one-line JSON request/response) — shared by aiolos + the SDK
anemos/              the SDK: run() lifecycle driver (CLI, signals, logging, the protocol loops,
                     restore wiring), a signal-aware stdin reader, the curve + EMA Controller, and
                     the Anemos / Device traits a module implements
tech/ipmi/           generic inband IPMI transport
tech/nvml/           NVML GPU access
tech/hwmon/          generic hwmon (sysfs) temperature reader
tech/nvme/           NVMe enumeration + per-drive temperatures (sysfs)
aiolos/              the orchestrator (depends only on protocol wire types)
anemoi/nvidia/       a thin anemos: Anemos/Device on anemos + nvml
anemoi/asrock16-2t/  a thin anemos: anemos + ipmi + hwmon (board OEM commands in src/board.rs)
anemoi/nvme/         a sensor-only anemos: anemos + nvme (reports temps, controls nothing)
```

### Writing a new anemos

Pick the tech crates you need, implement two small traits, and write a one-line `main()`. The SDK
handles CLI dispatch, signals, logging, the stdio protocol, the curve/EMA, and the restore-on-exit
wiring — you write none of it:

```rust
use anemos::{Anemos, Applied, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo, Reading};

fn main() -> ! {
    anemos::run(
        ModuleInfo { name: "demo",
                     // `Some(path)` for a fan/curve module; `None` for a sensor-only module.
                     curve_default_path: Some("/opt/aiolos/etc/demo.curve.json"),
                     curve_env_filename: Some("demo.curve.json") },
        Demo,
    )
}

struct Demo;
impl Anemos for Demo {
    fn detect(&mut self) -> Detected { /* report the IDs you manage */ }
    fn open(&mut self, id: &str) -> anyhow::Result<Box<dyn Device>> { /* bind one device */ }
    fn restore_all(&mut self) { /* hand every device back to firmware/auto */ }
}
impl Device for MyDevice {
    fn apply(&mut self, _in: Option<&Inputs>, ctrl: &mut Controller) -> Applied {
        let t = self.read_temp();              // your tech crate
        match ctrl.duty(t).pct {               // SDK: curve + EMA + deadband + floor
            Some(p) => self.set(p),            // command the device
            None    => self.set_default(),     // no usable curve -> firmware/auto
        }
        Applied::ok(vec![Reading::new("temp", "demo", serde_json::json!({ "temp": t }))])
    }
    fn restore(&mut self) { /* fail-safe: back to firmware/auto */ }
}
```

---

## Build & test

```bash
cargo build --release          # all crates
cargo test --workspace         # unit + orchestrator integration tests
cargo clippy --all-targets --workspace
```

The integration suite spawns the real orchestrator against a mock anemos and asserts behaviour via
side-effect markers — process isolation (a hung/flooding sibling is killed and respawned while a
healthy one keeps ticking), `input=` routing, hotplug add/remove, graceful restore, signal-restore,
and the `aiolos restore` fail-safe.

---

## Status

Young but working: deployed and regulating a production GPU server. The orchestrator core and both
shipped anemoi are covered by tests and have been hardened through multiple independent reviews.
Contributions and new anemoi are welcome.

## License

MIT.
