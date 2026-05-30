# Spec: `nut` anemos

Status: design (SOW-0009). UPS / utility-power state **sensor** — read-only; controls NO device.
Conforms to `aiolos-protocol.spec.md`. Its readings are routed (`input=nut`) into a power reactor
(e.g. `nvidia-powercap`) so a utility-power event triggers a declared action.

## Purpose
Report each monitored UPS's utility-power state for routing, as a new reading type **`power-state`**.
One `run` instance per UPS, bound by the NUT **id** (the upsc name, or `name@host[:port]` for a
remote upsd). This is a **sensor-only** anemos: it has no curve, sets nothing, and its fail-safe is
a no-op (there is nothing to hand back to firmware).

## Integration choice — `upsc` (documented)
The module shells out to the system **`upsc`** client (NUT's canonical read-only tool) rather than
re-implementing the upsd TCP protocol. Rationale:
- `upsc` already speaks the upsd socket, SSL, and `LIST`/`GET` framing — re-implementing it adds risk
  for no benefit to a read-only sensor.
- It needs **no credentials in code**: `upsc <id>` reads public UPS *variables* (no login). A
  remote/authenticated upsd is reached by configuring the id as `ups@host` in operator config and
  setting up NUT (upsd/upsmon) out-of-band. **No UPS host or credential is committed.**
- `upsc` writes the `key: value` variables to **stdout** and incidental notices (e.g. "Init SSL
  without certificate database") to **stderr**, so parsing stdout is clean.

The client binary is overridable via `$AIOLOS_UPSC_BIN` (off-hardware testing). Implemented in the
level-1 `nut` tech crate (`upsc -l` to list, `upsc <id>` to read + parse).

## detect
- Determine the UPS set: the operator config `nut.conf` list if it yields any id, else auto-discover
  via `upsc -l` (the local upsd). Emit one `found` per id:
  `{"id":"<ups-id>","type":"UPS","name":"<ups-id>"}`.
- Empty `found` is a real result (no UPS configured/discovered). Re-`detect` reflects the current
  set.

## run <id>
- Each tick: `upsc <id>`, parse the variables, and report ONE `power-state` reading:
  ```json
  {"status":"ok","readings":[
    {"type":"power-state","label":"pr3000-nova",
     "status":"OL","online":true,"on_battery":false,"low_battery":false,
     "charge":100,"runtime_s":697,"load_pct":36,"input_voltage":219.0,"model":"PR3000ERT2U"}]}
  ```
  - `status` = `ups.status` verbatim (space-separated NUT flags, e.g. `OL`, `OB`, `OB LB`, `OL CHRG`).
  - `online`/`on_battery`/`low_battery` = decision-ready booleans derived from the flags. `OB` is
    authoritative over `OL` during a transition (conservative: assume on-battery if the flag is set).
  - `charge` (%), `runtime_s`, `load_pct`, `input_voltage`, `model` are included **only when the
    driver reports them** (omitted otherwise — never null placeholders).
- `inputs` are ignored (a pure sensor). If `upsc` fails (binary missing, upsd unreachable, exit
  non-zero) respond `{"status":"error","error":"…"}` (transient; reconciled on the next tick).

## Modes
`detect` · `run <id>` · `restore` (one-shot: **no-op** — a sensor controls nothing — exits 0;
idempotent; still implemented so `aiolos restore` can call it uniformly).

## Fail-safe
None required: the module controls no device, so `shutdown`/EOF/`SIGTERM`/`restore` simply exit.
There is no state to revert and no risk from the module stopping.

## Config — `$AIOLOS_ETC_DIR/nut.conf` else `/opt/aiolos/etc/nut.conf`
Plain list: one UPS id per line; blank lines and `#` comments ignored; first-seen de-duplicated. An
id is a NUT name (`pr3000-nova`) or `name@host[:port]`. This is the place for the operator's
environment-specific UPS host — **no host/id is hardcoded or committed**. Absent/empty file → the
module auto-discovers via `upsc -l`. The shipped template is fully commented (installing it changes
nothing; auto-discovery applies until the operator edits it).

## Why a separate process (isolation)
`upsc` is an external command that talks to upsd over a socket; a hung upsd / slow UPS driver could
block the read. Running it in its own process means a stuck `upsc` only stalls this `nut` instance
(killed + respawned at the tick deadline); the power reactor keeps its prior-tick state and the rest
of aiolos is unaffected.

## Acceptance criteria
- `detect` lists one entry per configured/discovered UPS; ids stable across re-detect.
- `run <id>` reports a `power-state` reading within timeout; an `upsc`/upsd failure → `status:error`
  (never a crash, never silent).
- The booleans correctly reflect `ups.status` (incl. `OB` winning over `OL` in a transition).
- `nut restore` exits 0 and is idempotent (no-op).
- No secrets in committed artifacts (the UPS host/id lives only in operator `nut.conf`).
