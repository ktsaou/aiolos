# Spec: `it87` anemos

Status: design (SOW-0016). Consumer-board fan **control** via the Linux `it87` hwmon driver (sysfs
PWM) — for BMC-less boards with an ITE Super-I/O (e.g. Gigabyte Z690 UD / IT8689E). Conforms to
`aiolos-protocol.spec.md`. The sysfs analog of `rome2d-fans`: same zone model, same fail-safe
discipline, but `/sys/class/hwmon` PWM writes instead of IPMI.

## Purpose
Regulate this board's fans by temperature. One `run` instance (the board). It drives a
config-declared set of PWM channels, splitting them into a **CPU zone** (the CPU-cooler headers,
following CPU temperature) and a **case zone** (intake/exhaust, following
`max(CPU, routed temperature producers)`), each via its own curve. Useful routed inputs include GPU
temperature from `nvidia`, board/VRM/DIMM/NIC temperatures from `hwmon-temps`, and NVMe
temperatures from `nvme`.

## detect
- Resolve the configured chip (`it87.conf` `chip=`, default `it8689`) under `/sys/class/hwmon`.
  Present → emit the board unit plus one component and `fan-duty` sink schema for each managed
  channel.
- Absent (driver not loaded / wrong name) → empty `units` (a real "nothing to manage" result, NOT
  an error — `error` is reserved for "could not perform detect").

## run <id> (apply)
Decided LIVE each tick from config (so dropping in / removing a zone curve switches mode next tick):
- **zone mode** (active iff BOTH `it87.cpu.curve.json` and `it87.case.curve.json` load a non-empty
  curve): CPU-zone channels (`cpu=`, default `1`) follow `coretemp` via the CPU curve; all other
  managed channels (`case=`, default `3,4`) follow `max(CPU, routed temperature producers)` via the
  case curve.
  Two internal `anemos::Controller`s (own EMA/deadband/sensitivity).
- **uniform mode** (fallback): one `it87.curve.json` over `max(CPU, routed temperature producers)`
  for every managed channel.
- **optional source-matched case policy (SOW-0022):** `it87.case.policy.json` adds demands only to
  case channels. The uniform/zone baseline always runs and CPU channels remain unchanged. Every
  rule independently sees every matching numeric producer, reduces its matches by maximum, and
  applies one independent curve. Each case channel uses the maximum of its existing baseline and
  all rule requests. A required rule without a fresh match, or any configured policy/curve fault,
  commands case channels to 100% and returns `status:"ok"` with a warning.

Per tick: put each managed channel under manual control (`pwmN_enable=1`) and command its duty
(`pwmN = round(pct * 255 / 100)`), re-asserting manual every tick to defend against a board EC that
reclaims SmartFan. Report the board unit, CPU temperature producer signals, and per-fan components.
Each managed fan has an RPM producer when readable plus a claimed duty sink with `control.driven_by`
and the generic `control.driving` decision.

In addition to the managed channels, the report includes **read-only `fanN.duty` + `fanN.rpm`
producer signals for every unmanaged
header that is currently spinning** (`fanN_input > 0`) — e.g. a BIOS-driven CPU fan on an EC-locked
header. These carry duty and RPM but **no sink**, so the UI shows the fan (and whether it has headroom)
without implying aiolos controls it. The duty is the firmware-reported `pwmN` register value, not an
aiolos command — a header in automatic mode may report a static placeholder (e.g. `255`) that does not
track the live duty, so it is informational. Empty/unwired headers (RPM `0` or unreadable) are omitted,
so the report lists real fans only. The same unmanaged-fan publishers appear in the read-only
`info`/`collect` report. (SOW-0017 addendum.)

If the driving temperature is indeterminable, or the active curve is empty, the module **releases
every managed channel to firmware/automatic** (`pwmN_enable=2`) and replies `status:error` — it
never holds manual-but-blind. The case zone follows `max(CPU, routed temperature producers)` (NOT
GPU-only): a desktop tower is a single airflow chamber, so case fans respond to CPU heat and hot
board/NVMe/DIMM/NIC/GPU sensors. Routed sink signals and non-temperature producers are ignored, so
fan output cannot feed back into the temperature decision.

## Fan control mechanism (sysfs)
Via the level-1 `hwmon` tech crate, addressing the chip's hwmon node:
- claim manual + set duty: write `pwmN_enable=1` then `pwmN=<0..255>`;
- restore: write `pwmN_enable=2` (firmware/automatic SmartFan);
- observe: read `fanN_input` (RPM). Duty scales 0–100% ↔ 0–255 (nearest).

## Fail-safe
Three equivalent triggers — `shutdown`, stdin EOF, SIGTERM/SIGINT — plus the `restore` one-shot and
a `Drop` backstop, each set every managed channel back to `pwmN_enable=2` (firmware/automatic). The
controlled (manual) state is more aggressive than firmware auto, so "module dies → firmware
reclaims" is the safe direction. **SIGKILL freezes the last manual duty** (sysfs PWM persists; the
IT8689 has no hardware watchdog) — bounded safe because the SDK's 35% duty floor keeps any frozen
value ≥ floor; systemd `ExecStopPost: aiolos restore` (which calls `it87 restore`) is the net.

## Config
- `it87.conf` (`$AIOLOS_ETC_DIR` else `/opt/aiolos/etc/`): `chip=`, `cpu=`, `case=` (1-based PWM
  channel lists). Absent → built-in defaults (`it8689`, `cpu=1`, `case=3,4`) for the reference host.
- Curves next to the main path: `it87.curve.json` (uniform) + `it87.cpu.curve.json` +
  `it87.case.curve.json` (zone). Reloaded every tick (live tuning); last-good kept on a partial
  write; an invalid curve at startup refuses to regulate (SDK SOW-0012). No secrets in any of these.
- Optional source policy: `it87.case.policy.json`, next to the main curve. No file at process
  startup or valid `"enabled":false` preserves baseline behavior. Once enabled, removing/breaking
  it fails the case channels high. Match fields are exact-list `module`, `instance`, signal `id`,
  `component`, `uom`, and signal `labels`; populated fields are ANDed, values within a field are
  ORed. Local CPU signals use source `module="self", instance="self"`. Curve references must be
  relative basenames in the same directory.
- The packaged disabled example places the NVMe rule first, marks it required, and references
  `it87.case.nvme.curve.json`: `{"50":30,"70":100,"sensitivity":1.0}`. Its fallback rule matches
  other temperature signals and uses `it87.case.curve.json`.
- Policy and every referenced curve are re-read/strictly validated every apply. Policy curves
  reject duplicate normalized temperatures, decreasing duty, duty outside 0..100%, and invalid
  sensitivity. Unlike baseline curves, configured policy faults do not retain last-good values.

## Modes
`detect` · `info [id]` / `collect [id]` · `run <id>` · `restore` (one-shot: set all managed channels to firmware/automatic; exits
0 on success, non-zero if any channel could not be released; idempotent).

## Acceptance criteria
- `detect` emits the board when the chip is present; empty `units` when absent (never an error).
- `run` drives CPU-zone channels from CPU temp and case-zone channels from
  `max(CPU,routed-temperature-producers)` per their curves; verified by reading `pwmN`/`fanN_input`
  under CPU, GPU, board, and storage thermal load.
- `run`/`info`/`collect` also report read-only `fanN.duty` + `fanN.rpm` publishers for every unmanaged
  header that is spinning (RPM > 0), with no sink; empty/unreadable headers are omitted. Verified on
  the reference host: the BIOS-driven CPU fan (`fan1`) appears alongside the managed case fans.
- `shutdown`, stdin-EOF, and SIGTERM each restore every managed channel to `pwmN_enable=2`
  (verified by reading sysfs after exit); `it87 restore` is idempotent.
- Indeterminable temp or empty curve → release to firmware/automatic + `status:error` (never holds
  manual-but-blind; never commands 0%).
- Duty 0–100% maps to raw 0–255 correctly (35%→89, 100%→255); the 35% floor holds sub-floor temps.
- An enabled NVMe source policy takes the hottest temperature producer across all routed NVMe
  instances and drives only case channels to 100% on the same apply at 70 C. CPU channels retain
  baseline behavior. Required telemetry loss and configured policy/curve errors fail case channels
  high with authoritative warning/provenance.
- No non-JSON on stdout; no secrets in committed artifacts (channel ids are config, not secrets).
