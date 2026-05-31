# Spec: `hwmon-temps` anemos

Status: design (SOW-0016). Generic Linux sysfs temperature **sensor** — read-only; controls NO
device. Conforms to `aiolos-protocol.spec.md`. The BMC-less workstation analog of `ipmi-temps`:
reports board/VRM, DIMM, NIC (and any other configured) temperatures for the status page (and for
routing, if ever wired). This is a **sensor-only** anemos: no curve, sets nothing, fail-safe is a
no-op.

## Purpose
Surface every temperature a consumer board exposes through `/sys/class/hwmon` that is NOT already
covered by a dedicated module, so the operator sees the whole machine on the aiolos status page.
One `run` instance reads all configured chips in a single process (cheap sysfs register reads).

## detect
- Emit one entry: `{"id":"hwmon","type":"board","name":"hwmon sysfs temps"}`. (All configured chips
  are read in one process; the id is stable, so aiolos keys routed readings by `hwmon-temps:hwmon`.)

## run <id>
- Read every `tempN_input` from each configured chip (`hwmon-temps.conf` list, else the built-in
  default set: `gigabyte_wmi`, `it8689`, `spd5118`, `r8169`). Report one `temp` reading per sensor:
  ```json
  {"status":"ok","readings":[
    {"type":"temp","label":"gigabyte_wmi.temp1","temp":31},
    {"type":"temp","label":"spd5118@11-0050","temp":36},
    {"type":"temp","label":"spd5118@11-0051","temp":32},
    {"type":"temp","label":"r8169@0000:06:00","temp":38}]}
  ```
- **Label disambiguation** (so multiple chips sharing a `name` never collide):
  - one instance, one sensor → `chip`;
  - one instance, many sensors → `chip.<sensor>` (sensor = `tempN_label` else `tempN`);
  - many instances → `chip@<instance>` (+ `.<sensor>` when that instance has many sensors), where
    `<instance>` is the chip's stable `device`-symlink basename (e.g. i2c `11-0050`, PCI `0000:06:00`).
- `inputs` ignored (a pure sensor). No readable temperature from any configured chip →
  `{"status":"error","error":"…"}` (transient; reconciled next cycle). CPU (`coretemp`) and NVMe are
  deliberately excluded from the default set — reported by `it87` and `nvme` respectively.

## Fail-safe
None required: the module controls no device. `shutdown`/EOF/SIGTERM/`restore` simply exit; there is
no manual state to revert and no thermal risk from the module stopping.

## Config
- `hwmon-temps.conf` (`$AIOLOS_ETC_DIR` else `/opt/aiolos/etc/`): one hwmon chip `name` per line;
  `#` comments; blank lines ignored; de-duplicated. Absent/empty → the built-in default set. No
  curve (sensor-only). No secrets (chip names are public hardware identifiers).

## Modes
`detect` · `run <id>` · `restore` (one-shot: no-op — a sensor controls nothing; exits 0; idempotent;
implemented so `aiolos restore` can call it uniformly).

## Acceptance criteria
- `detect` lists the single `hwmon` instance.
- `run` reports temps from every configured chip; labels disambiguate duplicate chip names
  (`spd5118@<addr>`) and multi-sensor chips (`gigabyte_wmi.tempN`).
- No configured chip readable → `status:error` (never a crash, never silent).
- `hwmon-temps restore` exits 0 and is idempotent (no-op).
- No non-JSON on stdout; no secrets in committed artifacts.
