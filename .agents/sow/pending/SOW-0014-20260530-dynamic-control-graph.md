# SOW-0014 - aiolos v2: dynamic control-graph data model (publishers/sinks, claim·set·verify·release, config correlation)

## Status

Status: open

Sub-state: design captured 2026-05-31 from an extended user design discussion; **not started**, and the
user is still deciding whether to pursue it. This SOW **replaces** the original SOW-0014 framing ("typed
module kinds + `input=` validation") — that approach was rejected by the user in favour of a far more
dynamic model (see *Superseded approach* below). This is a large, **protocol-breaking aiolos v2**: it
relocates all correlation/control logic into aiolos, turns anemoi into pure publishers/sinks, and makes
the whole select→reduce→curve→act pipeline **user configuration**. The current production aiolos (v1)
keeps cooling nova and the desktop until v2 is built, tested, and explicitly cut over. **The data-model
half is being pulled forward by SOW-0017** (the stable `components[] → publishers[] + sinks[]`
detect/report shape, device `class`, icons — no engine) as an enablement. User clarification
2026-05-31: SOW-0017 should make `detect` and reports good enough for this SOW, so this SOW should not
require major detect/report schema changes; it should change the **apply/command methodology** and add
the central engine + config correlation on top of that shape. The optional live `info` command remains
a SOW-0014/follow-up command surface that must reuse the same schema rather than redesign it.

## Requirements

### Purpose
Make aiolos a **domain-blind, configuration-driven control graph**. aiolos must not know anything about
temperatures, fans, power, or any specific domain. Anemoi only **produce** measurements (tagged with an
open "kind") and/or **act** as **sinks**; aiolos owns all the logic — selecting signals by kind,
combining them (max/avg/ema/…), gating with conditions, shaping with curves, and deciding the value each
sink should be set to. A user models the entire behaviour in configuration; new domains (a Netdata
collector, a log-grep counter, anything) drop in without changing aiolos or any existing anemos.

### User Request
Faithful summary of the 2026-05-31 discussion (user quotes condensed; no constraints lost):

> "aiolos should NOT know anything about temperatures and fan speeds or whatever. There are 'kinds' of
> things — anemoi that provide measurements and anemoi that can act, or both. A user may create an anemo
> for collecting data from a local netdata, grep logs, whatever — we don't care. The user activates an
> anemo that says 'I collect this and it is of kind X', then the same user says 'find the max of all/some
> of the X and based on this apply this curve there'. Completely dynamic about what it does."

> "anemoi collect things and give them to aiolos. anemoi act based on what aiolos decides — they don't
> decide by themselves; aiolos applies curves, logic, correlations, matches, and decides 'this should be
> set to that value'."

> "`detect` should return producers and sinks and processes, like `units: [ { id, producers:[…],
> sinks:[…] } ]`. Each unit is a process; anemoi decide what is independent and what is grouped. `detect`
> should print to stdout and exit when stdin is a tty, so users can see what they can match. Also an
> `info` command to output everything with values. The web UI could have configuration, and we could
> also have a TUI for configuration."

> "claiming and releasing can be verified by the anemos. Example: I set PWM on FAN1 to 30% but I now see
> it at 40% — I didn't do it, so something else did; per user config, I should claim the device again. We
> need a data model that has claiming, setting, verifying, releasing, correlating, with curves,
> conditions, functions (max, avg, ema, etc). **The data model is king.**"

> "The power comes from configuration — one user prefers this, another prefers that; our anemoi should be
> agnostic. I prefer the relay [untyped] the way it is" (i.e. do **not** add hard typed contracts that
> reject configurations).

### Assistant Understanding

Facts:
- v1 today: `detect` returns bare IDs; aiolos spawns `run <id>` per ID; routing relays all of a source's
  readings (`input=`), and the **consuming anemos** filters by reading `type` and computes its own duty
  via the SDK `Controller` (curve+EMA+deadband, SOW-0003 D4). Combination (`max`) and zone mapping are
  **hardcoded in module code** (board-fan `regulate`, SOW-0010 zones).
- The user wants the opposite split: **mechanics in the anemos, policy in config, logic in aiolos**. The
  anemos exposes what it can measure (publishers/producers) and what it can drive (sinks); aiolos computes the
  values; the sink merely applies them.
- "Kinds" are **open, user-meaningful tags**, not a closed enum aiolos validates. aiolos matches a tag;
  it never interprets it. (This is why the original typed-validation SOW-0014 is rejected.)
- Verification is naturally a **correlation**: a sink's readback (the actual observed value) is itself a
  publisher signal, so aiolos can compare commanded-vs-observed with the same select machinery and apply a
  configured divergence policy (re-assert / re-claim / warn / ignore).
- This is now multi-host: nova (BMC/IPMI board) and a BMC-less desktop (it87, hwmon-temps) — concrete
  proof that correlation must be per-host **configuration**, with the same agnostic anemoi.

Inferences:
- Moving curve/EMA/deadband out of the `anemos` SDK into an aiolos-side engine is consistent with the
  "lean, domain-agnostic" identity: a curve is generic math, not fan knowledge. It is still a
  **protocol break** (the sink tick-contract flips from "here are readings, you decide" to "here is your
  commanded value, apply it") and an SDK refactor.
- The existing SOW-0013 scheduler already caches latest per-source results and ticks non-blocking — the
  engine is a synchronous per-tick evaluation on top of it; no async runtime needed.
- SOW-0015 (thermal powercap) collapses into **one more config pipeline** under this model
  (`select kind=temperature + kind=power-state → gate → curve/policy → target nvidia-powercap`); it needs
  no special multi-input typing.

Unknowns (design forks for the user — see Pre-Implementation Gate):
- Exact apply/command methodology; whether the curve runs as an engine stage or a sink-adjacent stage;
  how expressive the condition/function grammar should be at first; one-umbrella-SOW vs a sequence;
  migration/coexistence. The major detect/report schema should be settled by SOW-0017.

### Acceptance Criteria
- aiolos contains **zero domain knowledge**: no "temperature"/"fan"/"power" concepts in the orchestrator;
  it operates only on opaque `kind` tags + numeric `value`s + user pipelines.
- An anemos `detect` returns the SOW-0017 **capability map**
  (`components[] → publishers[] + sinks[]` with schemas); reports return the same shape **plus live
  values**. A live `info` command may be added here/follow-up, but must reuse this schema rather than
  redesign it.
- A user can, **entirely in configuration**, select signals by kind/source/label, reduce them
  (max/min/avg/ema/weighted/…), gate with conditions, shape with a curve, and drive any sink output — for
  any domain, with no aiolos or anemos code change.
- The **claim·set·verify·release** lifecycle is first-class: aiolos drives it; a sink declares whether it
  `needs_claim`; verification compares commanded vs the readback signal with a settle window + tolerance;
  divergence triggers the configured policy (re-assert / re-claim / warn / ignore).
- **Fail-safe is preserved/strengthened**: a sink that receives no fresh command within its timeout, or on
  aiolos death / EOF / SIGTERM, drives itself to its declared `safe` value (firmware auto / 100%) — never
  stale, never zero.
- Old behaviour is reproducible: nova's GPU+board fan cooling and the desktop's fans run identically under
  v2 config, validated on hardware before cutover (nvfd-style staged migration).

## Analysis

Sources checked / to re-check at activation:
- `protocol/` wire types (`detect`/`run`/component reports), `anemos` SDK (`run` driver, `Controller`,
  `Curve`/`Damper`), `aiolos/` (`main.rs::build_inputs`, scheduler `module.rs`/`instance.rs`,
  `status_page.rs`), the shipped anemoi (nvidia, rome2d-fans, nvme, ipmi-temps, nut, it87, hwmon-temps),
  the protocol spec + `project-anemos-protocol` / `project-create-anemos` skills.

Current state:
- Policy is split across config (`input=`), module code (`max`, zones), and per-module curve files. v2
  consolidates policy into config and a single aiolos-side engine.

Risks (high level; detailed in the gate):
- Protocol break + SDK relocation of safety-critical control math → blast radius is the whole project.
- Config becomes a small DSL → over-engineering / unsafe-config risk; mitigated by a tiny operator set +
  hard safe-fallback defaults.
- Verification false-positives from set latency → mandatory settle window + tolerance.

## Pre-Implementation Gate

Status: blocked (open SOW; user still deciding whether to pursue). Activation gate to be completed before
any code, after the forks below are decided.

Problem / root-cause model:
- v1 hardwires correlation/curve into each consuming anemos, so a different host/user needs code changes,
  and the same device knowledge is re-encoded per module. The fix is to make anemoi pure mechanics and
  move all correlation/control into a config-driven, domain-blind engine in aiolos.

Affected contracts and surfaces (a protocol-breaking v2):
- **Protocol**: reuse the SOW-0017 `detect`/report capability schema
  (`components[] → publishers[] + sinks[]`). The major protocol change left for this SOW is the sink
  tick-contract / apply methodology (inputs-in/module-decides → commanded-values-in/aiolos-decides) and
  any claim/set/verify/release command semantics needed to drive sinks.
- **`anemos` SDK**: `Controller` (curve/EMA/deadband) moves out to the engine; sinks gain a
  claim/set/verify/release device surface; publishers already normalise to `{id,label,kind,value?,…}` via
  SOW-0017.
- **`aiolos`**: a new config-driven **control-graph engine** (select→reduce→conditions→curve→command),
  per-output verification + divergence policy, fail-safe-on-no-command; config format change.
- **Config**: a new declarative `control` block format (replaces `input=`/per-module curve files).
- **Status page / UI**: render components/publishers/sinks/control-state; the new config UI + TUI.
- **Specs + skills**: protocol spec, `project-anemos-protocol`, `project-create-anemos` all rewritten.

The data model (proposal captured for memory — to refine at activation):
- **Capability model** (`detect` = schema, reports = schema + live values), supplied
  by SOW-0017 with no major redesign here:
  ```
  components: [{
    id: "board",
    label: "ROME2D16-2T",
    class: "board",
    publishers: [
      { id:"cpu1.temp",  label:"CPU1",     kind:"temperature", unit:"C", value:47 },
      { id:"fan1.rpm",  label:"FAN1.rpm", kind:"fan-rpm",               value:1500 },
      { id:"fan1.duty", label:"FAN1.pwm", kind:"fan-duty",    unit:"%", value:40 }   // readback publisher
    ],
    sinks: [
      { id:"fan1", label:"FAN1", kind:"fan-duty", range:[0,100], safe:"auto",
        needs_claim:true, readback:"fan1.duty", direction:"up=more-cooling" }
    ]
  }]
  ```
  component = device/entity grouping chosen by the anemos; publisher = `{id,label,kind(open tag),value?,
  unit?,range?}`; sink = `{id,label,kind,range?,unit?,value?,safe,needs_claim,readback,direction?}`.
- **Runtime (per tick)**: publishers emit `[{id,label,kind,value,…}]`; aiolos commands sinks
  `[{output:label, value:V}]`; sinks report control-state `released|claimed|diverged`.
- **Lifecycle (anemos mechanics, aiolos drives, config sets policy)**: `claim` (take manual control) →
  `set(v)` → `verify` (read back → emit `readback` signal) → `release` (→ `safe`). Divergence example:
  aiolos holds the claim, commanded 30%, readback 40% → after settle+tolerance, apply `on_divergence`
  (`reassert|reclaim|warn|ignore`).
- **Correlation model (config, one block per controlled output)**:
  ```
  control "board-case-fans":
    target:  rome2d-fans.FAN3..FAN8
    select:  kind=temperature                 # any unit, or from:[nvidia,nvme,ipmi-temps]; label glob
    when:    nvidia present                    # optional condition gate(s)
    reduce:  max                               # max|min|avg|ema|weighted|p95|…
    smooth:  ema(0.2), deadband(2)             # stateful stages
    curve:   board-case.curve.json             # number → output value
    verify:  tolerance=5%, settle=3, on_divergence=reclaim
  ```

Existing patterns to reuse:
- SOW-0013 non-blocking scheduler + cached per-source results (the engine ticks on top).
- The SOW-0003 `Curve`/`Damper`/`Controller` math (relocated to the engine).
- SOW-0008 grace/hysteresis pattern → the verify settle-window/tolerance.
- v1 restore-on-EOF/SIGTERM/Drop → extended to "no fresh command → safe".

Risk and blast radius:
- Whole-project protocol break; safety-critical control math relocates. Mitigated by: build v2 alongside
  v1, keep v1 in production, reproduce old behaviour under v2 config, hardware-validate, then cut over
  (the nvfd→aiolos pattern). Hard safe-fallback on every missing/broken-config path.

Sensitive data handling plan:
- No new sensitive data. BMC IP/IPMI creds/host serials/UPS host stay in operator/`*.local` config, never
  in committed artifacts (per AGENTS.md).

Implementation plan (sketch — a sequence under this umbrella; finalise at activation):
1. **Protocol + apply methodology**: reuse SOW-0017 detect/report schemas; define the commanded-sink
   apply contract and any claim/set/verify/release command semantics.
2. **Engine**: config parse → select/reduce/conditions/curve → per-sink commands (curve relocated here).
3. **Lifecycle + fail-safe**: claim/set/verify/release; verification + divergence policy; no-command→safe.
4. **Config format** (text first) + migrate the shipped anemoi from local policy to commanded sink
   mechanics.
5. Optional **`info` dump**, then the **web config UI** and **TUI** (if built, they reuse the same component schema and write the text config).
6. Hardware validation on nova + desktop; staged cutover from v1.

Validation plan:
- Unit tests for engine operators, verification settle/tolerance, fail-safe paths; reproduce v1 cooling
  under v2 config; operator-gated on-hardware validation on both hosts before cutover.

Artifact impact plan:
- AGENTS.md (architecture/layout/commands), DESIGN.md, README; protocol + anemos specs rewritten;
  `project-anemos-protocol` + `project-create-anemos` rewritten; new config/UI/TUI docs.
- SOW lifecycle: large umbrella; likely split into child SOWs per stage at activation. SOW-0015
  (thermal powercap) folds in as a config pipeline example; SOW-0009's "depends on SOW-0014 typed inputs"
  note to be reconciled (it now depends on this control-graph model instead).

Open decisions (forks — recorded from the discussion with recommendations; **not yet chosen** by the user):
1. **Lifecycle shape** — claim/set/verify/release as above, `needs_claim` per output, divergence policy
   (`reassert|reclaim|warn|ignore`) gated by `tolerance`+`settle`. *Recommend as written.*
2. **Verification** — model a sink's readback as a normal publisher signal that aiolos correlates against
   the command (uniform, reuses `select`), vs a dedicated verify channel. *Recommend readback-as-publisher.*
3. **Expressiveness now** — minimal fixed operators (reduce set + AND-ed comparison gates) vs a richer
   expression language. *Recommend minimal-first; grow on demand.*
4. **Delivery** — one big v2 SOW vs a sequence of child SOWs under this umbrella (old aiolos in prod until
   cutover). *Recommend a sequence under this umbrella.*

Plus, to settle at activation: the exact apply/command fields and where stateful smoothing
(EMA/deadband) sits (its own pipeline stage vs inside the curve). The major publisher/sink detect/report
schema comes from SOW-0017.

## Implications And Decisions

None locked yet — this SOW is design memory. Forks 1–4 (above) plus the schema details are to be decided
with the user before the activation gate is completed. The one rejected decision is recorded next.

### Superseded approach (rejected 2026-05-31)
The original SOW-0014 proposed **typed module kinds + hard `input=` validation**: each module would
declare `produces`/`requires` (a closed type enum) and the orchestrator would **fail at startup** on a
mismatch. The user rejected this: it bakes a *policy* ("a fan controller requires temp") into the module,
whereas correlation is a *user choice* that differs per host/user. v2 keeps kinds **open** and never
hard-fails a wiring; instead, a mismatch simply produces no correlation (safe), and configuration is what
expresses intent. The validation value (catching a useless wiring) is recovered softly via `info`/UI
showing what matches, plus warn-on-unmatched-reference — never a hard contract.

## Plan
1. User decides whether to pursue v2 and resolves forks 1–4 + apply/engine details. The detect/report
   schema should be inherited from SOW-0017 without major redesign.
2. Complete the activation gate; split into staged child SOWs (apply contract → engine → lifecycle/
   fail-safe → config → UI/TUI) under this umbrella.
3. Build v2 alongside v1; reproduce v1 behaviour under v2 config; hardware-validate on nova + desktop.
4. Staged cutover from v1; rewrite specs/skills/docs; fold SOW-0015 in as a config example.

## Execution Log

### 2026-05-31
- Replaced the original "typed module kinds + `input=` validation" SOW-0014 (never implemented; user
  rejected the typed-contract approach) with this **dynamic control-graph data model** SOW, capturing the
  full design discussion. Renamed slug `module-kinds-input-validation` → `dynamic-control-graph`. No code.
  Status kept `open` (user still deciding whether to pursue v2).
- User clarified that SOW-0017 should make `detect` and reports good enough for this SOW, avoiding major
  schema changes later. This SOW should change the apply/command methodology and add the central engine on
  top of SOW-0017's schema. No code.

## Validation

Pending (design-only; no implementation yet).

## Outcome

Pending.

## Lessons Extracted

- The agnosticism the project already espouses ("aiolos knows nothing about fans/GPUs") extends one level
  further: even **correlation/curve is policy**, so it belongs in configuration and an engine, not in the
  anemoi. A second, BMC-less host made this concrete — same agnostic anemoi, different per-host wiring.
- Hard typed contracts that reject configurations are the wrong tool when users legitimately want
  different correlations; keep kinds open and make misconfiguration *safe and visible*, not *fatal*.

## Followup

- SOW-0015 (nvidia-powercap thermal trigger): **closed 2026-05-31 and absorbed into this SOW** — under the
  v2 model it is a config pipeline, not a separate module. Its requirements/risks are retained here as
  design input.
- SOW-0009 (completed): its future powercap policy — re-enabling the cap with a **configurable baseline**
  (cap below 400 W without restoring the 600 W default) and the thermal trigger — also lands under this
  SOW. Its stale `SOW-0014`/`SOW-0015` references were repointed here (2026-05-31).
- SOW-0017 (created 2026-05-31): carries this SOW's **data model** forward — the component/publisher/sink
  report shape, device `class`, and icons — **without** the engine. When it ships, this SOW becomes just
  the central engine + config correlation on top of that shape.

## Regression Log

None yet.
