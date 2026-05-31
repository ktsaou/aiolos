# SOW-0017 - Component report model (SOW-0014 data model, no engine) + reworked web UI

## Status

Status: open

Sub-state: created 2026-05-31 from the UI / data-model discussion. **Not started.** This SOW pulls
**SOW-0014's data model forward without its centralized engine** — anemoi report a structured
`components[] → publishers[] + sinks[]` shape (what SOW-0014 needs), but **control logic stays in the
modules** for now. It is an **enablement**: once shipped, SOW-0014 becomes "add the central engine"
with no module re-churn. It also delivers the device-grouped web UI, fixes a reading-duplication
confusion, and renames the odd `asrock16-2t` module to a functional name. One open decision (the new
module name). The production aiolos (v1 shape) keeps cooling nova + the desktop until cutover.

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
- `asrock16-2t` deliberately re-publishes routed GPU/NVMe temps as its own `temp/GPU`, `temp/NVMe`
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
- The new functional name for `asrock16-2t` (and whether to normalise the other names too).
- Final confirmation of the publisher/sink field shapes and the icon-ownership model.

### Acceptance Criteria
- **Report shape:** `detect` and the per-tick report use `components[] → {publishers[], sinks[]}`; no flat
  `readings[]`. Control behaviour is **byte-for-byte unchanged** (this is a reporting refactor) —
  validated by reproducing v1 cooling on hardware.
- **Component schema:** each component carries a `class` (device kind, open tag) for grouping + icon;
  publishers are normalised scalar streams `{label, kind, value, unit}`; sinks carry
  `{label, kind, range, value, readback?, safe, needs_claim, state, driven_by?}`.
- **No duplication:** each device is published by exactly one anemos (its owner); `asrock`'s successor no
  longer publishes GPU/NVMe components — consumed inputs appear only as sink `driven_by` metadata.
- **Companion surface:** `detect` returns the component map; `info` returns it **with live values**; both
  pretty-print + exit when stdin is a tty, one-line JSON otherwise.
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
- **Rename:** `asrock16-2t` → the chosen functional name across binary, crate dir, registry config,
  `*.curve.json`, specs, skills, docs — in one coordinated, approved cutover.
- **Specs + skills** updated to the component contract; **SOW-0014** reduced to "add the central engine."

## Analysis

Sources checked / to re-check at activation:
- `protocol/src/lib.rs` (wire types: `Reading`, detect/report), `anemos/` SDK (report building, `run`
  driver, `Controller`), `aiolos/src/main.rs` + `status_page.rs` (status.json / history / metrics),
  all anemoi (`nvidia`, `asrock16-2t`, `nvme`, `ipmi-temps`, `nut`, `it87`, `hwmon-temps`),
  `aiolos/src/assets/*`, the protocol spec + `project-anemos-protocol` / `project-create-anemos`,
  packaging (`install.sh`/`update.sh`, `aiolos.conf`).

Current state:
- Flat readings; module-centric UI; control in modules; one board-make module name; routed temps echoed.

Risks (detailed in the gate):
- Wide blast radius (protocol + every module + UI + specs + skills + live config); low behavioural risk
  (control unchanged); production-cutover risk (live registry + rename) mitigated by staged build +
  approved cutover + v1 fallback.

## Pre-Implementation Gate

Status: needs-user-decision (the module name; final confirmation of the field shapes) → then ready.

Problem / root-cause model:
- The flat, module-centric `readings[]` shape cannot express devices, device-class, provenance, or
  controllable outputs, so the UI cannot group by device, duplicates routed values, and SOW-0014's
  engine has nothing structured to drive. Restructuring the report to components/publishers/sinks fixes
  all three and is the data-model half of SOW-0014.

Evidence reviewed:
- Live `status.json` (GPU temp published by both `nvidia` and `asrock16-2t`); `status_page.rs` reading
  fields (`type` + `label` only, no class/icon); the SOW-0003/0004/0006 module reports; SOW-0014's
  captured capability model.

Affected contracts and surfaces:
- **Protocol** wire types: `detect` payload + per-tick report (`readings[]` → `components[]{publishers[],
  sinks[]}`); the `info` command; tty pretty-print.
- **`anemos` SDK**: a report-builder for components/publishers/sinks; `detect`/`info`/tty plumbing.
- **Every anemos**: restructure its report into components (control logic untouched); the board-fan
  module drops foreign-device echoes (→ `driven_by` metadata).
- **`aiolos`**: `status.json`, `/history.json`, `/metrics` re-mapped to the component shape.
- **status_page + assets**: device-grouped UI, icon set, interactive multi-axis time-series, animation.
- **Packaging + live config**: the `asrock16-2t` rename (binary/registry/curve filename); `aiolos.conf`.
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
1. **Protocol + SDK**: define `components`/`publishers`/`sinks` wire types + the SDK report-builder;
   `detect`/`info`/tty behaviour. Unit-test the wire types.
2. **Migrate anemoi** to publish components (control logic unchanged): `nvidia`, `nvme`, `ipmi-temps`,
   `nut`, `it87`, `hwmon-temps`, and the board-fan module — which also **drops foreign-device echoes**
   (routed inputs → sink `driven_by`).
3. **Orchestrator**: re-map `status.json` / `/history.json` / `/metrics` to the component shape.
4. **UI rework** (co-equal deliverable): device-centric grouped home + per-device detail (publishers /
   sinks / `driven_by`); the icon set; one interactive multi-axis time-series chart (hover / toggle /
   zoom / time-range); live curve view; speed-driven backdrop; theming + polish. Visual design iterated
   with the user (mockups before build).
5. **Rename** `asrock16-2t` → chosen name (binary/crate/config/curve/specs/skills/docs).
6. **Specs + skills** rewrite; reduce SOW-0014 to "the engine."
7. **On-hardware validation** on nova (+ desktop) and an **approved cutover**.

Validation plan:
- Unit tests for the new wire types + each module's component mapping; reproduce v1 readings/behaviour
  (same temps/duties/RPM, same control); UI visual check (grouping, no duplication, interactive charts);
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
  the engine. May split into child SOWs (protocol/SDK → modules → UI) at activation.

Open decisions (recorded from the discussion; recommendations given — confirm before implementation):
- **D1 Publisher shape** — one normalised scalar per publisher `{label, kind, value, unit}`; multi-value
  devices = multiple publishers. *Recommend as written* (clean for charts + SOW-0014 reducers).
- **D2 Sink shape** — `{label, kind, range, value(current), readback?, safe, needs_claim,
  state:released|claimed|diverged, driven_by?}`; module sets `value` now, aiolos sets a `target` later
  (same shape). *Recommend as written.*
- **D3 Class + icons** — `class` (device kind, open tag) on the component drives grouping; aiolos ships
  a built-in, **data-driven inline-SVG icon library keyed by `class`** (hand-authored, consistent
  line-art); an anemos may optionally override with its own inline SVG. **DECIDED 2026-05-31.** Icons are
  parametric/live from publisher/sink values: fan blades spin at real RPM (paused at 0), the UPS battery
  fills to `charge%` (color by online/on-battery/low), GPU/CPU/board tint with temperature; SSD/NIC stay
  static (no fake motion). Inline SVG (not `<img>`) + CSS `animation-duration`/transform from JS;
  `currentColor`/CSS-vars theming; animations pause when the tab is hidden / icon is off-screen. The
  actual glyphs are iterated visually with the user (mockups before build).
- **D4 Dedup rule** — a device is published by its owner only; consumed inputs become sink `driven_by`
  metadata, never foreign components. *Recommend as written.*
- **D5 Companion surface** — `detect` component map + `info` (with values) + tty pretty-print are in
  scope. *Recommend include.*
- **D6 Migration** — clean break (flip protocol + all anemoi + aiolos + UI together; approved cutover;
  v1 fallback) vs dual-emit transition. *Recommend clean break.*
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
- **D8 Charting tech** — enhance the existing **hand-rolled SVG** charts (zero-dependency, full control,
  matches the lean single-binary ethos) vs embed a tiny library (e.g. uPlot ~40 KB) for richer
  interactivity faster. *Recommend hand-rolled* — vanilla SVG can deliver hover/zoom/multi-axis and keeps
  the no-runtime-dependency principle. The detailed visual design is a **collaborative, iterated** step
  (mockups reviewed with the user before build), not a fixed spec here.

## Implications And Decisions

Agreed in discussion (2026-05-31), to confirm at the activation gate: the component/publisher/sink report
shape (D1–D5), data-model-only scope (no engine), and the clean-break migration (D6). **D3's icon model is also decided
(2026-05-31): a built-in, data-driven inline-SVG library keyed by `class` — live fan-spin / UPS-fill /
temp-tint, hand-authored, themeable, with an optional per-anemos override.** The shape sketch:

```
components: [
  { id:"gpu-0", label:"RTX 6000 (GPU-0)", class:"gpu",
    publishers: [
      { label:"temp", kind:"temperature", value:42, unit:"C" },
      { label:"fan0", kind:"fan-rpm",     value:1623, unit:"rpm" }
    ],
    sinks: [
      { label:"fans", kind:"fan-duty", range:[0,100], unit:"%",
        value:46, readback:1623, safe:"auto", needs_claim:true, state:"claimed",
        driven_by:[ {from:"nvidia:gpu-0", value:42} ] }
    ] }
]
```

**Open (must be resolved before implementation):** **D8** (charting tech — recommend hand-rolled SVG) and
a final thumbs-up on the publisher/sink field shapes. **D7 decided 2026-05-31:** rename to `rome2d-fans`
for now (only the outlier; other module names unchanged; OEM sequences stay in `board.rs`).

## Plan
1. User confirms D1–D6 and chooses D7 (the module name).
2. Complete the activation gate; optionally split into child SOWs (protocol/SDK → modules → UI).
3. Build the component-shape report alongside v1; reproduce v1 readings/behaviour; unit-test.
4. Rebuild the UI on the new shape (grouping, icons, interactive multi-axis charts, animation).
5. Rename the board-fan module; update specs/skills/docs/packaging.
6. Operator-approved on-hardware cutover on nova (+ desktop), v1 as fallback.
7. Reduce SOW-0014 to "add the central engine."

## Execution Log

### 2026-05-31
- Created (open) from the UI / data-model discussion. Scope: pull SOW-0014's data model forward
  (components/publishers/sinks) **without** the centralized engine, as an enablement; deliver the
  device-grouped UI + interactive time-series + speed-driven animation; fix the routed-temp duplication;
  rename `asrock16-2t` to a functional name. No code. One open decision (the module name).

## Validation

Pending (design-only; no implementation yet).

## Outcome

Pending.

## Lessons Extracted

Pending.

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

## Regression Log

None yet.
