# SOW-0017 - Component report model (SOW-0014 data model, no engine) + reworked web UI

## Status

Status: in-progress

Sub-state: activated 2026-05-31. User approved implementation after clarifying that this SOW must freeze the detect/report schema for SOW-0014; SOW-0014 should change apply/command methodology only. The live `info`/`collect` companion surface is implemented through an SDK read-only `collect` path and `OpenMode::Observe`, not by calling state-changing `apply`. D8 charting path accepted as hand-rolled SVG for zero runtime dependencies. Production service cutover remains a separate operator-approved action. This SOW pulls **SOW-0014's data model forward
without its centralized engine** — anemoi report a structured `components[] → publishers[] + sinks[]`
shape, and this shape is intended to be the **stable detect/report schema SOW-0014 builds on**. SOW-0014
should change the **apply methodology** (aiolos computes and commands sinks) without major detect/report
schema churn. Control logic stays in the modules for now. This SOW is an **enablement**: once shipped,
SOW-0014 becomes "add the central engine + new apply/command semantics" with no module re-churn for
detect/reports. It also delivers the device-grouped web UI, fixes reading-duplication confusion, and
renames the old `asrock16-2t` module to the functional board-family name `rome2d-fans`. The production aiolos (v1 shape) keeps
cooling nova + the desktop until cutover.

## Requirements

### Purpose
Restructure the anemos↔aiolos report from today's flat per-instance `readings[]` into a structured
**`components[]`** array, where each component (a device/entity) carries **`publishers[]`** (what it
measures) and **`sinks[]`** (what it can drive). This is SOW-0014's capability shape **minus the
centralized correlation/curve engine**: modules keep deciding their own outputs, but they *report* in
the v2 shape. The SOW has **two co-equal deliverables**: (1) the new report shape (the SOW-0014 data
model, no engine), and (2) a **reworked web UI — more beautiful and more functional** — built on it:
a device-centric grouped layout, rich per-device detail, interactive multi-axis time-series, a refined
backdrop, and the elimination of duplicated readings. The same shape makes SOW-0014 a drop-in later.

### User Request
Faithful summary of the 2026-05-31 discussion (quotes condensed; constraints preserved):

> "I would prefer to add a structured **'components' array** to the output, each component having
> **publishers and sinks**. Take from SOW-14 everything **except the centralized logic**. The shape of
> the feedback anemoi report should be like what SOW-14 needs. This will be an **enablement**, so that
> SOW-14 can be made feasible later."

> "The home page is the list of modules, which has little value. It would be much nicer to see the list
> of **devices**, as a **vertical menu, grouped by type** (GPUs, CPUs, …) — grouped by **device type, not
> anemos name**. Anemoi should provide these types/kinds and probably **SVG icons** for the grouping to be
> beautiful."

> "The time-series are **not interactive**; you cannot see them **all together on a chart with multiple
> y-axes**; the background animation should change **speed instead of color intensity**."

> "`asrock` reports back the GPU temps it received, so there is **duplication** of information on the UI
> which makes things confusing."

> "The name `asrock` is odd. It should be related to **the way this works**, not to the mobo make."

### Assistant Understanding

Facts (from the current code):
- Today `status.json` is module/instance-centric: `instances[] → readings[]`, each reading
  `{type, label, …flattened fields}` where `type ∈ {temp, fan, driving, powercap, power-state}`. There is
  **no device-class / kind / icon** dimension and no structured component grouping.
- The pre-SOW board-fan module (`asrock16-2t`) deliberately re-publishes routed GPU/NVMe temps as its own `temp/GPU`, `temp/NVMe`
  readings (SOW-0004), so the UI shows the GPU temp twice (once from `nvidia`, once echoed) — the
  reported duplication.
- Control logic (curve/EMA/deadband, claim/set/release) lives in the modules / `anemos` SDK
  (SOW-0003 D4). This SOW does **not** move it; it only restructures the **report**.
- Module names are inconsistent: `nvidia` (vendor), `nvme`/`nut`/`it87` (tech), `ipmi-temps`/
  `hwmon-temps` (functional), `asrock16-2t` (board make — the outlier).

Inferences:
- A component is the device-level grouping the UI wants; the anemos decides its own component
  decomposition ("anemoi decide what is independent and what is grouped").
- Duplication disappears *by construction* if a component is **published by exactly one owner** and
  consumers do not re-publish foreign devices — the consumed value becomes sink `driven_by` metadata.
- Because aiolos + all anemoi ship from one repo/version, the shape can flip atomically (clean break);
  behaviour is unchanged (control stays in modules), so behavioural risk is low.

Unknowns (decisions — see Pre-Implementation Gate):
- No schema-direction unknown remains for SOW-0014: detect/reports from this SOW must be good enough
  for SOW-0014, which should only change apply/command methodology.
- Remaining UI checkpoint: visual polish/mockup review remains a user-facing checkpoint before final UI polish, but it does not block the protocol/schema implementation.

### Acceptance Criteria
- **Report shape / SOW-0014 schema freeze:** `detect` and per-tick reports use
  `components[] → {publishers[], sinks[]}`; no flat `readings[]`. This schema is the intended
  SOW-0014 detect/report schema. SOW-0014 may change apply/command methodology, but should not require
  major detect/report schema changes. Control behaviour is **byte-for-byte unchanged** in this SOW
  (this is a reporting refactor) — validated by reproducing v1 cooling on hardware.
- **Component schema:** each component carries a `class` (device kind, open tag) for grouping + icon;
  components, publishers, and sinks all have stable local `id`s. Publishers are normalised scalar
  streams `{id, label, kind, value?, unit?, range?}`. Sinks carry
  `{id, label, kind, range?, unit?, value?, readback?, safe?, needs_claim, state, direction?, driven_by?}`.
  `value` is absent in pure schema surfaces and present in live report/status surfaces.
- **No duplication:** each device is published by exactly one anemos (its owner); `asrock`'s successor no
  longer publishes GPU/NVMe components — consumed inputs appear only as sink `driven_by` metadata.
- **Companion surface:** `detect` returns the component map. A live `info` one-shot is now in scope via an SDK-level read-only `collect` path, separated from state-changing `apply`, so `info` can report live values without claiming/setting hardware.
- **UI — a beautiful, functional rework (a co-equal deliverable, not a side effect):**
  - **Home = device-centric**, not a module list: a vertical menu / grid **grouped by device `class`**
    (GPUs, CPUs, SSDs, Board, NIC, Power), each group iconned, each device showing its live primary value
    + status at a glance.
  - **Per-device detail**: its publishers (live value + unit + sparkline) and sinks (current value,
    readback, claim/verify `state`, and the `driven_by` inputs that drove it) — the "what drives what"
    relationship made visible, replacing the duplicated-reading confusion.
  - **Time-series**: one **interactive** chart that can show series **together on multiple y-axes**
    (°C / % / RPM), with hover tooltips, series toggle, zoom/pan, and a time-range selector.
  - **Curves**: live curve view with the current operating point (carried over, refreshed).
  - **Icons**: a built-in, **data-driven inline-SVG** library keyed by `class` — fans spin at real RPM,
    the UPS battery fills to charge (color by state), GPU/CPU tint with temperature; SSD/NIC stay static.
    Hand-authored, themeable, with an optional per-anemos override.
  - **Animation**: the backdrop driven by **speed** (load → faster), color held steady.
  - **Craft**: cohesive typography/spacing, dark/light themes, smooth transitions, responsive layout —
    "impressive," self-served, **zero external runtime dependency**.
  - Detailed visual design is **iterated with the user** (mockups/screenshots before build) — "beautiful"
    is the user's call, so the look is reviewed, not assumed.
- **Icons:** aiolos ships a built-in SVG set keyed by `class`; an anemos may optionally override.
- **Rename:** `asrock16-2t` → `rome2d-fans` across binary, crate dir, registry config,
  `*.curve.json`, specs, skills, docs — in one coordinated, approved cutover.
- **Specs + skills** updated to the component contract; **SOW-0014** reduced to "add the central engine
  + new apply/command methodology."

## Analysis

Sources checked / to re-check at activation:
- `protocol/src/lib.rs` (wire types: component detect/report), `anemos/` SDK (report building, `run`
  driver, `Controller`), `aiolos/src/main.rs` + `status_page.rs` (status.json / history / metrics),
  all anemoi (`nvidia`, `rome2d-fans`, `nvme`, `ipmi-temps`, `nut`, `it87`, `hwmon-temps`),
  `aiolos/src/assets/*`, the protocol spec + `project-anemos-protocol` / `project-create-anemos`,
  packaging (`install.sh`/`update.sh`, `aiolos.conf`).

Current state:
- Flat readings; module-centric UI; control in modules; one board-make module name; routed temps echoed.

Risks (detailed in the gate):
- Wide blast radius (protocol + every module + UI + specs + skills + live config); low behavioural risk
  (control unchanged); production-cutover risk (live registry + rename) mitigated by staged build +
  approved cutover + v1 fallback.

## Pre-Implementation Gate

Status: ready/in-progress. The SOW-0014 detect/report schema direction is decided: SOW-0017 must ship the
stable component/publisher/sink detect/report schema, and SOW-0014 should change apply methodology only.
D7 is decided (`rome2d-fans`). D8 is decided: hand-rolled SVG charts. Visual mockup/polish remains a
user-facing checkpoint before final UI polish, not a schema blocker.

Problem / root-cause model:
- The flat, module-centric `readings[]` shape cannot express devices, device-class, provenance, or
  controllable outputs, so the UI cannot group by device, duplicates routed values, and SOW-0014's
  engine has nothing structured to drive. Restructuring the report to components/publishers/sinks fixes
  all three and is the data-model half of SOW-0014.

Evidence reviewed:
- Live/current `status.json` shape in code (GPU temp published by both `nvidia` and `rome2d-fans`);
  `status_page.rs` reading fields (`type` + `label` only, no class/icon); the SOW-0003/0004/0006 module
  reports; SOW-0014's captured capability model. User decision 2026-05-31: SOW-0017 should make
  `detect` and reports good enough for SOW-0014, so SOW-0014 should not make major schema changes there;
  only the apply methodology should change.

Affected contracts and surfaces:
- **Protocol** wire types: `detect` payload + per-tick report (`readings[]` → `components[]{publishers[],
  sinks[]}`); `apply.inputs` in this SOW should route the same
  component/publisher report data to keep consumers off legacy flat readings; SOW-0014 later changes the
  apply methodology from "inputs in, module decides" to "commands in, aiolos decides."
- **`anemos` SDK**: component publisher/sink types and `detect`/`apply` plumbing.
- **Every anemos**: restructure its report into components (control logic untouched); the board-fan
  module drops foreign-device echoes (→ `driven_by` metadata).
- **`aiolos`**: `status.json`, `/history.json`, `/metrics` re-mapped to the component shape.
- **status_page + assets**: device-grouped UI, icon set, interactive multi-axis time-series, animation.
- **Packaging + live config**: the `asrock16-2t` → `rome2d-fans` rename (binary/registry/curve filename); `aiolos.conf`.
- **Specs + skills**: protocol spec, `project-anemos-protocol`, `project-create-anemos`.

Existing patterns to reuse:
- The SOW-0003 SDK report path + `Controller` (kept in modules); SOW-0004 `module:id` routing (control
  still consumes inputs internally); SOW-0011 status-page rendering + history ring buffer + theme system;
  the SOW-0006/0005 per-device sensor knowledge that maps cleanly to components.

Risk and blast radius:
- Touches the whole project + the live production config. Behavioural risk is low (a reporting refactor;
  control unchanged), but a botched cutover could strand cooling — mitigated by: build the v2-shape
  alongside v1, unit-test the per-module component mapping, reproduce v1 readings/behaviour, and do an
  **operator-approved on-hardware cutover** with v1 as the fallback. The rename rides the same cutover.

Sensitive data handling plan:
- No new sensitive data. BMC IP / IPMI creds / host serials / UPS host stay in operator/`*.local` config,
  never in committed artifacts. Component/publisher labels are device names, not secrets.

Implementation plan (staged; finalise at activation):
1. **Protocol + SDK**: define stable `components`/`publishers`/`sinks` wire types + the SDK
   wire types; route component report data in `apply.inputs` while
   modules still decide locally. Unit-test the wire types.
2. **Migrate anemoi** to publish components (control logic unchanged): `nvidia`, `nvme`, `ipmi-temps`,
   `nut`, `it87`, `hwmon-temps`, and the board-fan module — which also **drops foreign-device echoes**
   (routed inputs → sink `driven_by`).
3. **Orchestrator**: re-map `status.json` / `/history.json` / `/metrics` to the component shape.
4. **UI rework** (co-equal deliverable): device-centric grouped home + per-device detail (publishers /
   sinks / `driven_by`); the icon set; one interactive multi-axis time-series chart (hover / toggle /
   zoom / time-range); live curve view; speed-driven backdrop; theming + polish. Visual design iterated
   with the user (mockups before build).
5. **Rename** `asrock16-2t` → `rome2d-fans` (binary/crate/config/curve/specs/skills/docs).
6. **Specs + skills** rewrite; reduce SOW-0014 to "the engine + apply/command methodology."
7. **On-hardware validation** on nova (+ desktop) and an **approved cutover**.

Validation plan:
- Unit tests for the new wire types + each module's component mapping; schema-drift check that no legacy
  `readings[]` remains in protocol/status/history/inputs; reproduce v1 readings/behaviour (same
  temps/duties/RPM, same control); UI visual check (grouping, no duplication, interactive charts);
  operator-gated on-hardware cutover with v1 fallback; same-failure scan for other readers of the old
  shape.

Artifact impact plan:
- AGENTS.md (layout / anemoi list / commands / the rename), DESIGN.md, README.
- Specs: protocol spec rewritten to the component contract; per-anemos specs updated; the board-fan spec
  renamed.
- Skills: `project-anemos-protocol` (component report contract) + `project-create-anemos` (publishers/
  sinks/class/icon) rewritten.
- Packaging: `install.sh`/`update.sh` + `aiolos.conf` updated for the rename.
- SOW lifecycle: this is the data-model enablement; **SOW-0014** is updated to depend on it and shrinks to
  the engine + apply/command methodology. May split into child SOWs (protocol/SDK → modules → UI) at
  activation.

Open decisions (recorded from the discussion; recommendations given — confirm before implementation):
- **D1 Publisher shape** — one normalised scalar per publisher
  `{id, label, kind, value?, unit?, range?}`; multi-value devices = multiple publishers. **DECIDED
  2026-05-31** as the SOW-0014-ready detect/report schema: `value` is optional so `detect` can return
  schema-only publishers and live reports can return values.
- **D2 Sink shape** — `{id, label, kind, range?, unit?, value?, readback?, safe?, needs_claim,
  state:released|claimed|diverged|unknown, direction?, driven_by?}`. **DECIDED 2026-05-31** as the SOW-0014-ready
  detect/report schema: modules set/report `value` now; SOW-0014 changes apply/command methodology so
  aiolos computes and commands sink targets later without changing detect/reports.
- **D3 Class + icons** — `class` (device kind, open tag) on the component drives grouping; aiolos ships
  a built-in, **data-driven inline-SVG icon library keyed by `class`** (hand-authored, consistent
  line-art); an anemos may optionally override with its own inline SVG. **DECIDED 2026-05-31.** Icons are
  parametric/live from publisher/sink values: fan blades spin at real RPM (paused at 0), the UPS battery
  fills to `charge%` (color by online/on-battery/low), GPU/CPU/board tint with temperature; SSD/NIC stay
  static (no fake motion). Inline SVG (not `<img>`) + CSS `animation-duration`/transform from JS;
  `currentColor`/CSS-vars theming; animations pause when the tab is hidden / icon is off-screen. The
  actual glyphs are iterated visually with the user (mockups before build).
- **D4 Dedup rule** — a device is published by its owner only; consumed inputs become sink `driven_by`
  metadata, never foreign components. **DECIDED 2026-05-31.**
- **D5 Companion surface** — `detect` component maps plus a live read-only `info` one-shot are in scope. **UPDATED 2026-05-31:** user approved changing the SDK to separate `collect` from state-changing `apply` and requested fixing all anemoi. This remains an SDK/internal split, not SOW-0014's commanded-sink protocol.
- **D6 Migration** — clean break (flip protocol + all anemoi + aiolos + UI together; approved cutover;
  v1 fallback) vs dual-emit transition. **DECIDED 2026-05-31: clean break for this repo/version, with
  production cutover separately operator-approved.**
- **D7 Module rename** — `asrock16-2t` → a functional name. **DECIDED 2026-05-31: `rome2d-fans` for now**
  (board-family tag; safer — the OEM raw-IPMI sequences stay in `board.rs`). A generic `ipmi-fans` is
  viable later only if those exact sequences (claim / set-with-mirror / release / query+parse) move into
  a config "board profile" — tracked as a follow-up. Rename only this outlier; other names unchanged.
  Options considered:
  - **a (recommend): `ipmi-fans`** — parallels `ipmi-temps`; names the mechanism (fan control over IPMI).
    The board-specific OEM raw commands stay inside (`board.rs`); `detect` simply finds nothing on other
    boards. Con: a future *generic* IPMI fan module couldn't reuse the name.
  - b `ipmi-oem-fans` / `bmc-fans` — more precise that it is vendor-OEM / BMC-driven; reserves `ipmi-fans`
    for a future generic one; slightly clunkier.
  - c keep a board tag but cleaner (`romed-fans`) — still hardware-ish.
  - **Sub-question:** rename only `asrock16-2t`, or normalise the others too (e.g. `nvidia`→`nvidia-fans`)
    for a consistent convention? *Recommend: rename only the outlier now; leave the rest.*
- **D8 Charting tech** — **DECIDED 2026-05-31: hand-rolled SVG.** Enhance the existing hand-rolled SVG
  charts (zero-dependency, full control, matches the lean single-binary ethos) rather than embed a tiny
  library. Vanilla SVG must deliver hover/zoom/multi-axis and keep the no-runtime-dependency principle.
  The detailed visual design is a collaborative, iterated step (mockups reviewed with the user before final
  polish), not a fixed spec here.
- **D9 SOW-0014 schema freeze direction** — **DECIDED 2026-05-31 by the user:** SOW-0017's `detect` and per-tick report schemas must be good enough for SOW-0014. SOW-0014 should not require major
  schema changes to those surfaces; only apply/command methodology should change.

## Implications And Decisions

Agreed in discussion (2026-05-31) and tightened by the user's schema-freeze direction: the
component/publisher/sink report shape (D1–D5), data-model-only scope (no engine), clean-break migration
(D6), and the requirement that SOW-0014 should reuse this detect/report schema and only change
apply/command methodology (D9). **D3's icon model is also decided (2026-05-31): a built-in, data-driven
inline-SVG library keyed by `class` — live fan-spin / UPS-fill / temp-tint, hand-authored, themeable,
with an optional per-anemos override.** The shape sketch:

```
components: [
  { id:"gpu-0", label:"RTX 6000 (GPU-0)", class:"gpu",
    publishers: [
      { id:"temp", label:"Temperature", kind:"temperature", value:42, unit:"C" },
      { id:"fan0.rpm", label:"fan0", kind:"fan-rpm", value:1623, unit:"rpm" }
    ],
    sinks: [
      { id:"fans", label:"fans", kind:"fan-duty", range:[0,100], unit:"%",
        value:46, readback:"fan0.rpm", safe:"auto", needs_claim:true, state:"claimed",
        direction:"up=more-cooling",
        driven_by:[ {from:"nvidia:gpu-0", value:42} ] }
    ] }
]
```

**Open before final UI polish:** the visual mockup/polish checkpoint. Schema direction is decided:
SOW-0017 should freeze detect/reports for SOW-0014. D8 is decided as hand-rolled SVG. **D7 decided
2026-05-31:** rename to `rome2d-fans` for now (only the outlier; other module names unchanged; OEM
sequences stay in `board.rs`).

## Plan
1. Record the SOW-0014 schema-freeze decision and D8 charting decision (done 2026-05-31); activate the
   SOW; implement as one SOW in staged phases (protocol/SDK → modules → orchestrator → UI → rename/docs).
2. Build the component-shape report alongside v1; reproduce v1 readings/behaviour; unit-test.
3. Rebuild the UI on the new shape (grouping, icons, interactive multi-axis charts, animation).
4. Rename the board-fan module; update specs/skills/docs/packaging.
5. Operator-approved on-hardware cutover on nova (+ desktop), v1 as fallback.
6. Reduce SOW-0014 to "add the central engine."

## Execution Log

### 2026-05-31
- Activated SOW-0017 and moved it to `.agents/sow/current/`. User approved implementation and clarified
  that SOW-0017 must freeze `detect`/report schema for SOW-0014; SOW-0014 should only change the
  apply/command methodology. Recorded D8 as hand-rolled SVG charting. No production service or `/opt`
  cutover approved.

- Created (open) from the UI / data-model discussion. Scope: pull SOW-0014's data model forward
  (components/publishers/sinks) **without** the centralized engine, as an enablement; deliver the
  device-grouped UI + interactive time-series + speed-driven animation; fix the routed-temp duplication;
  rename `asrock16-2t` to `rome2d-fans`. No code. One open decision (the module name).
- User clarified the SOW-0014 relationship: SOW-0017 should make `detect` and reports good enough for
  SOW-0014, avoiding major schema changes later. SOW-0014 should change only the apply/command
  methodology. Recorded as D9; no code.
- Implemented the component report model across protocol, anemos SDK exports, aiolos state/routing/status/history/metrics, all shipped anemoi, and the mock integration binary. `apply.inputs` now relays source component lists keyed by `module:id`; consumers extract publishers and report consumed provenance through sink `driven_by` instead of re-publishing foreign devices.
- Reworked the embedded dashboard on the new shape: device-class grouped home, component/publisher/sink details, built-in inline SVG class icons, speed-driven wind backdrop, and a combined multi-axis time-series chart with range controls, pan buttons, wheel zoom, legend toggles, and hover tooltips.
- Renamed the old board-fan module from `asrock16-2t` to `rome2d-fans` across crate path, binary name, workspace, packaging, registry config, curve filename, specs, docs, and project skills.
- Updated SOW-0014 to depend on SOW-0017's detect/report schema and to leave only apply/command methodology + central engine work for the future SOW.
- User approved adding an SDK-level read-only `collect` path separate from state-changing `apply`, and requested fixing all shipped anemoi. Decision: implement this as an internal SDK/API split in SOW-0017, not as the SOW-0014 commanded-sink protocol. Add a read-only observe open mode for `info`/collection where needed; keep normal `run <id>` using control mode and `apply`.
- Implemented the SDK split: `Anemos::open(id, OpenMode::{Observe,Control})`, mandatory
  `Device::collect`, default `Device::apply -> collect` for sensor-only modules, and one-shot
  `info`/`collect`/`schema` SDK modes. All shipped anemoi now implement observe-safe collection:
  `nvidia` and `nvidia-powercap` suppress restore-on-drop side effects in observe mode; `it87` and
  `rome2d-fans` report live readbacks without claiming or releasing fans; sensor-only anemoi use
  `collect` directly.

### 2026-05-31 (evening) — measurement-level data-model rethink (design; not yet built)
User feedback: the shipped UI still treats each device as a flat "pool of key-value data". Pivot to a
**bottom-up, data-driven** model; the UI must render whatever structure the data declares — with **no
device-organisation rules hardcoded** in aiolos or the page. Grounding (read 2026-05-31): the wire
types already support this (component granularity is free; `Component.icon` + per-`Publisher`/`Sink`
`extra` exist; `FoundEntry` already is a "unit"). Every anemos currently collapses a device into ONE
flat component (nvidia=1 "gpu" with both fans flattened; ipmi-temps=22 temps in one "bmc";
rome2d-fans=~25 publishers+8 sinks in one "board").

Target model:
- **Measurement** = a producer OR a sink: `{id, kind, friendly name, value, unit, labels{}}`; sinks add
  claiming `state`/range/safe/driven_by. Icon comes from a **kind→icon registry** (overridable per
  measurement / via config) — a generic visual vocabulary, not a device rule.
- **Component** = a logical sub-thing grouping related measurements (a fan = rpm producer + duty sink;
  a "CPUs" group = many temp producers). **The anemos owns the grouping** (device knowledge); config can
  override. e.g. nvidia → {Temperature, Fan 0, Fan 1}; ipmi-temps → {CPUs, DIMMs, LAN, Board}.
- **Unit** = a device (`FoundEntry`), grouped on the home by type; **may be shared across anemoi** — the
  orchestrator merges instances reporting the same unit identity (ipmi-temps board + rome2d-fans board
  → one "ROME2D16-2T"), stamping each measurement with its source anemos as a label.

Forks **decided 2026-05-31 (user delegated to assistant):** **D-A** fine components, grouping owned by
the anemos (multi-temp groups keep all temps; UI shows the max); **D-B** unit merge = **hybrid** (anemos
derives a best-effort unit id from DMI/serial, operator config can override/declare); **D-C** measurement
identity `{id,name,kind,value,unit,labels}` + orchestrator stamps `anemos`/source provenance on merge;
**D-D** built-in **kind→icon** registry, per-measurement/config override; **D-E** config scope (start) =
unit-merge/identity + friendly names + hide/show; **D-F** generic per-component layout + **pressure-
dominant** visuals. Phased: data-model+anemoi → orchestrator merge → config → UI. The simplistic "Sky"
home (deployed) is a milestone to be superseded by this structured render.

Phase 1 (in progress): the data model, proven bottom-up on `nvidia` first — a GPU unit reports
`Temperature` + `Fan i` components (each fan = rpm producer + duty sink), with a human unit name
(product + index), validated on real hardware via `nvidia info` before the UI/other modules follow.

## Validation

Implementation-pass validation (2026-05-31, refreshed after the SDK collect/apply split):
- `cargo fmt --all` — passed.
- `cargo check --all-targets` — passed.
- `cargo test` — passed (workspace unit tests, orchestrator integration tests, and doc tests).
- `cargo clippy --all-targets --workspace` — passed with no warnings.
- `cargo build --release` — passed.
- `node --check aiolos/src/assets/aiolos.js` — passed.
- `bash -n packaging/install.sh packaging/update.sh` — passed.
- `.agents/sow/audit.sh` — passed; SOW framework clean and sensitive-data guardrail clean.
- `git diff --check` — passed.
- Protocol smoke check with `target/debug/mock`:
  - `detect` returned `found[].components[]` with a schema-only publisher.
  - `info` returned a `detect`-shaped response with live component publisher values through the
    read-only collect path.
  - `run self` returned `status:ok` with live `components[]`; `shutdown` returned `status:ok`.
- Schema/API drift search: no stale two-argument `Anemos::open` impls remain; all shipped anemoi expose
  `collect`; no legacy flat `readings[]`, `last_readings`, or `Reading::` remain outside historical SOW
  context / the explicit protocol note that `components[]` replaces `readings[]`.
- Rename search: no lowercase `asrock` references remain in code/docs except historical SOW context;
  hardware-vendor references remain as `ASRockRack ROME2D16-2T` where they describe the board.

Production install / live validation (2026-05-31):
- Backed up the previous production install before changes under `/opt/aiolos/backup-sow17-20260531-120832/`
  (binaries, `/opt/aiolos/etc`, and the systemd unit).
- Stopped the active `aiolos` service cleanly. Evidence from systemd/journal: instances restored on
  shutdown, `aiolos restore` ran the one-shot restore hooks, and the service stopped successfully.
- Installed the SOW-0017 release binaries to `/opt/aiolos/bin/` and added
  `/opt/aiolos/etc/rome2d-fans.curve.json` by preserving the operator's existing board curve.
- Migrated only the active board-fan registry line in `/opt/aiolos/etc/aiolos.conf` from
  `asrock16-2t` to `rome2d-fans`; deliberately kept `nvidia-powercap` disabled per the existing
  operator comment about preserving the host's deliberate 400 W GPU cap.
- Restarted `aiolos`; service is active/running with zero restarts and processes for:
  `nvidia`, `nvme`, `ipmi-temps`, `nut`, and `rome2d-fans`.
- `/status.json` validates as component-shaped data: 7 instances, modules
  `ipmi-temps`, `nut`, `nvidia` (2 GPUs), `nvme` (2 drives), and `rome2d-fans`; every configured
  instance has `status:"ok"` and `components[]`; no legacy `readings[]` field appears.
- Direct read-only `info` smoke passed for installed anemoi:
  `nvidia`, `nvme`, `ipmi-temps`, `nut`, `rome2d-fans`, and disabled `nvidia-powercap` reported live
  values; `it87` reported no devices on this host; `hwmon-temps` returned a non-fatal warning because
  its default workstation chip list is not present/configured on this server.
- Post-install journal scan found no concerning `error`/`fatal` statuses, protocol errors, timeouts,
  panics, kills, unresponsive modules, permission errors, or divergent sinks.
- GPU check after the disabled `nvidia-powercap info` smoke showed both GPUs still capped at 400 W,
  confirming the read-only observe path did not restore the firmware 600 W default.

Not yet completed / still gated:
- No visual/user review of the dashboard was performed in this pass.
- No external reviewer pass was run; the project review gate is still pending before closing the SOW.

Sensitive data gate:
- Passed via `.agents/sow/audit.sh` and manual artifact review. No raw BMC IP/IPMI credentials, UPS
  private endpoints, host serials, secrets, personal identifiers, or proprietary incident details were
  added to durable artifacts.

## Outcome

Implementation pass complete in the working tree, but the SOW remains `in-progress` pending review,
visual acceptance, and operator-approved on-hardware validation/cutover.

## Lessons Extracted

- Do not implement live `info` by calling `apply`: for control modules that would be observability with
  side effects. The safe surface is the implemented SDK `collect` path plus `OpenMode::Observe`, with
  observe-mode handles explicitly suppressing claim/set/release/restore-on-drop behavior.
- The component model removes routed-signal duplication only if consumers report provenance as
  `sink.driven_by` and never re-publish foreign components.
- The board-fan rename is safest as a board-family functional name (`rome2d-fans`) until OEM IPMI byte
  sequences become config-driven board profiles.

## Followup

- **Generic `ipmi-fans` via config (deferred):** a future board-agnostic IPMI fan module whose exact OEM
  sequences (claim / set-with-mirror / release / query + response parsing) live in a config "board
  profile" — would let any board be supported without code. Feasible but non-trivial (byte templating +
  the duty-mirror rule + response decode); deferred for safety, so this SOW keeps the ROME2D sequences in
  `board.rs` under the name `rome2d-fans`. Revisit alongside the config-driven v2 direction (SOW-0014).
- On completion, **SOW-0014** shrinks to "add the central control engine on top of this report shape"
  (decision ownership flips from modules to aiolos; same wire shape). Update its dependencies accordingly.
- The component `class` dimension added here also fills a gap in SOW-0014's original schema (which had
  only a measurement `kind`, insufficient to separate GPU from CPU).

## Progress — Bottom-Up Web Dashboard (2026-06-01)

Built the data-driven, bottom-up status dashboard the user specified (measurements are the atoms;
units group them; nothing about device types is hardcoded in the page).

Data model (so the page never invents structure):
- `nvidia` now reports a GPU as one unit with **3 components**: `temperature`, then `fan{0..n}`
  (each an `rpm` publisher + a `duty` sink). `FoundEntry.name` carries the product name; the duty
  sinks carry the commanded value, claim `state`, and `driven_by` provenance.
- Threaded a per-unit `type` through the orchestrator (`main.rs` `InstanceEntry.unit_type`,
  `module.rs` reconcile/spawn, `status_page.rs` `InstanceJson` `#[serde(rename="type")]`), so
  `/status.json` exposes each instance's `type` for grouping without the page guessing from class.
- Renamed the board-fan device struct `AsrockDevice` → `Rome2dFansDevice` (board-family name).

Dashboard (front-end, `aiolos/src/assets/*`):
- Home groups all units by `type`; each unit shows a hero = its **primary measurement's** animated
  icon (thermometer / battery) — **no device silhouettes** — plus every temperature as a live
  thermometer and every fan as a **duty-ring gauge**. Units are **stationary** (no bobbing).
- Pressure is **prominent**: a ring-gauge meter that fills + glows, the aurora reddens, and at high
  pressure the whole page enters an alarm state (rising red vignette). Verified calm vs. synthetic
  high-pressure renders in the offline preview.
- **Build-once-then-patch reconciler**: the stage rebuilds only when the unit/component signature
  changes; otherwise live values are patched into persistent nodes — SVGs are never recreated per tick.

Two fan-spin defects found in user review and fixed:
1. **Blades orbited the corner.** The blade group was rotated with an explicit centre
   `rotate(a 24 24)` while CSS `transform-origin` also applied `50% 50%`; the two stacked, moving the
   pivot to `(48,48)`. Proven with a headless pivot test (pivot traced a square). Fixed by making the
   rotation centre-less and letting the CSS origin be the only pivot.
2. **Blades hiccupped.** They were stepped on the main thread (SVG `transform` attribute via rAF),
   competing with the poll/reconcile and wind loops. Replaced with a **Web Animations API** rotation
   (compositor-driven, one 0→360° turn, speed changed only via `playbackRate`). Preview confirmed
   12/12 fans animate with duty-driven `playbackRate` over a wide range; centre re-proven by pivot test.

Deployed to `/opt/aiolos` after each step; service stayed active with zero restarts, all 7 instances
`ok`, board fans 8/8 claimed, GPUs held at 400 W.

Still open before this SOW can complete:
- **Names (#4/#5):** short unique name + long description, consistent across home, time-series (Flux
  labels still show UUIDs), and logs.
- **Board split + merge (#2):** split `ipmi-temps` into CPU/DIMM/LAN/Board components; merge the two
  ROME2D16-2T units (provenance via labels).
- External reviewer pass and on-screen visual acceptance (the project review gate) remain pending.

## Regression Log

None yet.
