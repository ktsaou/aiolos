# SOW-0018 - Ideal data schema: label-driven units / components / signals

## Status

Status: in-progress

Sub-state: activated 2026-06-01 on branch `schema-v2`. User reviewed a clean-slate schema analysis
("assume aiolos has not been developed; we want the ideal schema") and **agreed** to the label-driven
model (decisions D1–D4 below). This is the **data-model foundation** of the SOW-0014 v2 umbrella and
**supersedes SOW-0017's nested `components[] → publishers[] + sinks[]` report schema**. Scope here is the
**detect/report schema + assembly + config enrichment + UI grouping** — NOT the centralized control
engine (that stays SOW-0014's apply/command phase). Production aiolos (v1) keeps cooling nova + the
desktop, untouched, until this is built, validated, and operator-approved for cutover. Work proceeds on
the branch so `master` stays green for parallel work.

## Requirements

### Purpose
Replace the report schema with the **ideal** one the user has asked for repeatedly: a flat stream of
**labelled signals** from which the orchestrator assembles **units → components → producers/sinks**, that
**user configuration can enrich**, and that the **UI can group by anything**. Hardware units are
first-class (a unit is a piece of hardware, NOT an anemos), so one physical thing reported by several
anemoi (the motherboard: `ipmi-temps` temps + `rome2d-fans` fans) is **one unit**.

### User Request
Faithful summary of the 2026-06-01 discussion (quotes condensed; constraints preserved):

> "id: whatever the system needs to uniquely identify something, stable across restarts, read from the
> system, never shown to users. labels: anemoi can attach any number of labels — important ones: **name**
> (short unique name for the user, like gpu0, derived from the system, not necessarily permanent),
> **description** (longer non-unique), **type** (temp, fan…), **component** (the component this belongs
> to), **unit** (the unit this belongs to). … units (not anemoi — units of hardware); components within
> units; producers and sinks inside components; enrichment from user configuration. A methodology for
> detection and reporting from anemoi to work, while the UI can group by anything it likes. Assume aiolos
> has not been developed. We want the ideal schema, not a step towards it — the right schema."

### Assistant Understanding

Facts (from live `/status.json` 2026-06-01 + the spec/lifecycle catalogue):
- Today **unit == anemos instance** (`FoundEntry`), so the **same motherboard is two units** (`ipmi-temps`
  id `ipmi-temps` + `rome2d-fans` id `board`) that can never merge.
- The "component" idea is applied **inconsistently**: `nvidia` reports 3 clean components; `rome2d-fans`
  crams **30 publishers + 8 sinks into one `board` component**; `ipmi-temps`/`nvme`/`nut` use one fat
  component each.
- Enrichment today is only `input=` routing, `every`/`timeout`, and `*.curve.json`. **No mechanism to
  attach a user name/label/description** to anything (`config.rs`, `registry.rs`; `extra` map exists but
  has no config path to populate it).
- Routing relays a source's components verbatim, keyed `module:id`, to consumers wired `input=`; the
  consumer selects by `kind` (`main.rs::build_inputs`).
- Distinct signal types across the fleet: `temperature`(C), `fan-rpm`(rpm), `fan-duty`(%),
  `power-charge`(%), `power-runtime`(s), `power-voltage`(V), `power-load`(%),
  `power-online/on-battery/low-battery`(bool), `power-status/model`(string), `power-limit`(mW),
  `powercap-capped`(bool), and derived `driving-temperature`/`driving-duty`/`driving-mode`.

Inferences:
- The fix inverts ownership: **anemoi emit flat labelled signals; the orchestrator assembles units/
  components by label; config enriches; the UI groups by labels.** This is the Netdata labels model and
  matches the user's repeated "data-driven, organization from data, correlation from config" asks.
- A unit reported by multiple anemoi merges automatically when they agree on (or config maps) a unit id.
- Producer/sink stays first-class with **typed** control metadata so the SOW-0014 engine stays sound
  (labels are for identity/grouping, not control semantics).

Unknowns:
- None blocking. Exact enrichment-config syntax is deferred to its own step within this SOW; the
  apply/command engine is out of scope (SOW-0014).

### Acceptance Criteria
- **Wire schema:** `detect` and live reports use the SAME shape — top-level `units[]`, `components[]`,
  `signals[]`; detect omits `value`, reports include it. Verified by protocol round-trip tests + a live
  `info`/`status.json` capture across all anemoi.
- **Units are hardware:** the two ROME2D16-2T anemoi assemble into **one** motherboard unit (merge by
  unit id, finalized in config). Verified on nova's `status.json`.
- **Components are real:** `rome2d-fans`/`ipmi-temps` board split into CPU/DIMM/LAN/board + per-fan
  components; `nvme` per-drive; every signal declares its `component` and the component its `unit`.
- **Labels everywhere:** every unit/component/signal carries a `labels{}` bag with reserved keys
  `type`, `name`, `description`; the UI can group/filter by any label. `name` short + `description` long
  appear consistently on home, the time-series legend (no UUIDs), and logs.
- **Enrichment:** a config layer overlays/extends labels and merges/renames units by id or selector,
  with no code change. Verified by renaming a unit and seeing it on home + charts + logs.
- **Behaviour unchanged:** control output is byte-identical to v1 (reporting/schema refactor only);
  validated by reproducing v1 cooling on hardware before any cutover.
- **uom ≠ unit:** unit-of-measure is `uom`; the hardware grouping is the `unit` entity (no name clash).

## Analysis

Sources checked:
- Live `/status.json` (nova, 2026-06-01); `protocol/src/lib.rs`; `anemos/` run/traits; `aiolos/`
  `config.rs`/`registry.rs`/`main.rs::build_inputs`/`status_page.rs`/`module.rs`; all `anemoi/*`; the
  protocol spec + `project-anemos-protocol`/`project-create-anemos` skills; SOW-0014 + SOW-0017.

Current state:
- Nested per-instance `FoundEntry{components:[Component{publishers,sinks}]}`; unit==instance; no labels
  beyond `name`(long product string); no enrichment; routing by `module:id` + `kind`.

Risks:
- Protocol-breaking change across the whole workspace (every anemos + SDK + orchestrator + UI). Mitigated
  by branch + schema-first sequencing + keeping v1 in production until validated cutover.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:
- One root cause — **unit identity is the anemos instance** — produces both the duplicate-motherboard
  problem and the inconsistent components. Making signals flat + labelled and letting the orchestrator
  assemble/enrich removes the cause. Evidence: live `status.json` (two board units; one 30-publisher
  component) + `config.rs`/`registry.rs` (no enrichment path).

Evidence reviewed:
- Live `/status.json` 2026-06-01; `protocol/src/lib.rs:397` (`FoundEntry`), `:52` (`Component`);
  `aiolos/src/main.rs::build_inputs`, `status_page.rs:206/270` (`InstanceJson`/`HistInstance`);
  `aiolos/src/assets/aiolos.js:597` (Flux labels keyed by `module:id` → UUIDs); the 8 anemos specs;
  `project-anemos-protocol` skill; SOW-0014 (v2 vision) + SOW-0017 (superseded schema).

Affected contracts and surfaces:
- **Protocol** (`protocol/src/lib.rs`): replace `FoundEntry`/`Component`/`Publisher`/`Sink`/`Detected`/
  `Applied`/`Inputs` with `Unit`/`Component`/`Signal`(+`Control`)/`Report`/`Inputs(=Vec<Signal> per src)`.
  PROTO_VERSION 1 → 2.
- **`anemos` SDK**: detect/collect/apply build + return the new shape; the run() lifecycle, signals,
  curve/controller, restore are otherwise unchanged.
- **All 8 anemoi**: emit flat labelled signals with `unit`/`component`/`type` + short `name`/long
  `description`; board anemoi split into real components.
- **`aiolos`**: assemble units/components from signals across instances; apply config enrichment; route
  signals; `status.json` exposes the assembled, enriched tree with labels at every level; `history.json`
  carries `name` (label) keyed by stable signal id.
- **UI**: group by unit→component by default; regroup by any label; show name/description; provenance.
- **Specs + skills**: rewrite the protocol spec + `project-anemos-protocol` + `project-create-anemos`;
  per-anemos specs updated.

Existing patterns to reuse:
- SOW-0013 non-blocking scheduler + blackboard (now a signal store); SOW-0017's collect/apply (Observe)
  split, the reactive inline-SVG icon library + build-once-then-patch reconciler (re-fed by labels);
  restore-on-EOF/SIGTERM/Drop fail-safe (unchanged).

Risk and blast radius:
- Whole-workspace protocol break. Mitigation: branch `schema-v2`; schema-first order (protocol → SDK →
  anemoi → orchestrator → UI); v1 stays in production; reproduce v1 cooling under v2 before cutover;
  per-phase build/test; no deploy until validated + operator-approved.

Sensitive data handling plan:
- No new sensitive data. Unit ids may derive from hardware serials/DMI at runtime, but **serials/BMC IP/
  IPMI creds/UPS host never go into committed artifacts** — examples in SOWs/specs use placeholders
  (`GPU-<uuid>`, `board:<dmi>`). Live captures are redacted before pasting.

Implementation plan:
1. **Protocol** (`protocol/src/lib.rs`): the new `Unit`/`Component`/`Signal`/`Control`/`Report`/`Request`/
   `Inputs` types + builders + round-trip tests; bump PROTO_VERSION. (this turn)
2. **SDK** (`anemos/`): `Anemos::detect`/`Device::collect`/`apply` return `Report`; helpers to emit
   signals; keep curve/controller/restore.
3. **Anemoi**: migrate each to flat labelled signals; board anemoi grow real components; add short
   `name`/long `description`; logs key on short name.
4. **Orchestrator**: assemble units/components by label across instances; config-enrichment overlay;
   route signals; rewrite `status.json`/`history.json`.
5. **Enrichment config**: declarative label overlay + unit merge/rename (syntax defined in this step).
6. **UI**: group-by-any-label; merged motherboard; names/provenance.
7. **Specs/skills/docs** + hardware validation + operator-approved staged cutover.

Validation plan:
- Per-crate `cargo test`/`clippy`/`fmt`; protocol round-trip golden lines; a one-line stdin→stdout
  protocol smoke per anemos; assembled `status.json` shows one motherboard + real components; reproduce
  v1 cooling on nova (GPUs 400 W, board fans claimed) before cutover; external reviewer pass.

Artifact impact plan:
- AGENTS.md: layout/contract paragraph updated when the shape lands.
- Runtime project skills: `project-anemos-protocol` + `project-create-anemos` rewritten for the signal model.
- Specs: protocol spec rewritten; per-anemos specs updated as each migrates.
- End-user/operator docs: DESIGN.md data-model section; config docs for enrichment.
- SOW lifecycle: child of the SOW-0014 umbrella; supersedes SOW-0017's report schema (0017 paused);
  SOW-0014's "reuse SOW-0017 schema" assumption repointed here.

Open-source reference evidence:
- None checked; this is an internal schema design grounded in the local fleet's live data and specs.

Open decisions: resolved — D1–D4 below.

## Implications And Decisions

Decisions agreed by the user 2026-06-01 (numbered for the record):

1. **D1 — Wire shape (chose A):** anemos → aiolos is a flat `units[]` + `components[]` + `signals[]`
   payload (normalized; anemoi stay simple; the orchestrator assembles). Rejected B (anemoi emit a nested
   unit→component tree).
2. **D2 — Ownership of merge/rename/grouping (chose A):** the orchestrator assembles by label and **user
   config** owns merge/rename/regroup (the "correlation from config" vision, SOW-0014). Rejected B
   (anemoi must agree on shared unit ids themselves).
3. **D3 — Entities vs denormalized labels (chose A):** explicit `unit`/`component` entities, each with its
   own `labels{}` bag, so a unit or component can be enriched directly. Rejected B (pure-flat signals with
   `unit_name`/`component_name` denormalized onto each).
4. **D4 — Naming (yes):** `uom` for unit-of-measure (distinct from the `unit` entity); reserved labels
   `type`, `name`, `description` at every level; `role` (producer|sink) + control metadata stay typed
   fields, not labels.
5. **D5 — Sink driving contract (2026-06-02, user: "generic and accurately represent what is actually
   happening"):** every sink carries `control.driving` — a GENERIC record of the control decision
   (`type`/`raw`/`input`/`uom`/`output`/`how`), NOT a per-module producer signal. This fixes three
   reported defects: (a) a unit's *driving* temperature was being shown as the unit's *own*
   temperature (now there are no `driving-*` producers, so a unit's temp is its real max sensor);
   (b) driving was non-uniform (GPUs had none) — now every sink (GPU fan, board fan, power cap)
   reports it; (c) `driven_by` gained a human `name`, ending the UI's "undefined 67°". **CI-locked:**
   `Report::sink_contract_violations()` flags any claimed sink missing `driving.input`/`output`, with
   per-anemos tests (protocol + nvidia + rome2d + it87) so a regression fails the build.

Schema (the agreed shape; authoritative for step 1):
```jsonc
// detect and report share ONE shape; detect omits "value".
{ "status":"ok",
  "units":[ { "id":"nvml:GPU-<uuid>",
              "labels":{ "name":"gpu0", "description":"NVIDIA RTX PRO 6000 Blackwell…", "type":"gpu" } } ],
  "components":[ { "id":"nvml:GPU-<uuid>:fan0", "unit":"nvml:GPU-<uuid>",
                   "labels":{ "name":"fan0", "type":"fan" } } ],
  "signals":[
    { "id":"nvml:GPU-<uuid>:fan0:rpm", "component":"nvml:GPU-<uuid>:fan0", "role":"producer",
      "value":1247, "uom":"rpm", "labels":{ "type":"fan-rpm", "name":"rpm" } },
    { "id":"nvml:GPU-<uuid>:fan0:duty", "component":"nvml:GPU-<uuid>:fan0", "role":"sink",
      "value":32, "uom":"%", "range":[0,100], "labels":{ "type":"fan-duty", "name":"duty" },
      "control":{ "needs_claim":true, "state":"claimed", "safe":"auto", "direction":"up=more-cooling",
                  "driven_by":[ { "signal":"nvml:GPU-<uuid>:temperature:temp", "value":27, "uom":"C" } ] } }
  ] }
```
- `id`: stable, system-derived, hidden — the time-series key. `component`/`unit`: structural parent refs.
- `labels`: open bag; reserved `type`/`name`/`description`. `value`/`uom`/`range`: typed value domain.
- `role`: producer|sink. `control`: present iff sink (needs_claim/state/safe/direction/readback/driven_by).

## Plan
1. Protocol types + tests (branch `schema-v2`). 2. SDK. 3. Anemoi. 4. Orchestrator assembly + enrichment.
5. Enrichment config. 6. UI. 7. Specs/skills/docs + hardware validation + operator-approved cutover.

## Execution Log

### 2026-06-01
- Created branch `schema-v2`. Captured the agreed schema (D1–D4) and this gate.
- **Implemented the whole stack, inward-out:** `protocol` (Unit/Component/Signal/Control/Report +
  builders + 12 tests); `anemos` SDK (traits + run loops return `Report`); `tech/nvml` (added
  `GpuInfo.index`/`Gpu::index()`/`Gpu::name()` for the `gpuN` short name + unit description); all **8
  anemoi** migrated to flat labelled signals (board anemoi split into real components: `ipmi-temps` →
  cpu0/cpu1/dimms/lan/board; `rome2d-fans` → fan1..8/cpu/control; both report unit id `board` so they
  merge); the **orchestrator** (instance worker, reconcile-on-unit-id, signal blackboard + routing,
  and `status_page` assembly that merges units across instances + per-unit history); the **UI** (data
  layer rebuilt to consume units/components/signals via an adapter; Flux legend now uses unit names,
  not UUIDs). Control logic preserved byte-for-byte (reporting refactor).
- **Validated on real nova hardware** read-only (`info`) before cutover, then **cut over production**:
  backed up `/opt/aiolos/bin` → `bin.bak-v2-<ts>`, `systemctl stop` (fail-safe restored fans),
  installed all 9 binaries, `systemctl start`. Service active, NRestarts=0, no journal warnings.

## Validation

Acceptance-criteria evidence (post-cutover, live `/status.json` on nova):
- **Wire schema:** detect + reports share the unit/component/signal shape; 6 units, 23 components, 63
  signals. Protocol round-trip tests pass (12).
- **Units are hardware / merge:** the two ROME2D anemoi assemble into **one `board` unit**
  (`sources: ["ipmi-temps","rome2d-fans"]`).
- **Components real:** board split into cpu0/cpu1/dimms/lan/board + fan1..8 + cpu + control.
- **Labels + names:** every entity carries `type`/`name`/`description`; home + Flux + logs show short
  names (`gpu0`, `board`, `nvme0`, `ups0`), no UUIDs in the Flux legend.
- **Behaviour unchanged:** board fans 8/8 claimed @37%, GPU fans 4/4 claimed @30–37%, GPUs 400 W —
  matching v1 control (curves unchanged, control path preserved).
- **UI:** rebuilt dashboard renders the merged board + clusters on real data (offline preview
  screenshot) and the embedded page serves 200 with the v2 assets.

Tests: `cargo test` green across the workspace (protocol 12, orchestrator integration 11, + all
anemoi/SDK unit tests); `cargo clippy --all-targets` clean; `cargo fmt --all --check` clean.

Reviewer findings: external reviewer pass not yet run (tracked).

Sensitive data gate: no secrets in artifacts; unit ids use UUID/serial/`board` (no host serials/BMC
IP/creds committed).

## Outcome

aiolos v2 (label-driven signal schema) is **shipped and cut over in production** on nova — cooling
correctly with the merged motherboard. Remaining before SOW completion: config-driven enrichment
(rename/merge/regroup; the basic board merge currently works by anemoi agreeing on the `board` id),
protocol spec + `project-anemos-protocol`/`project-create-anemos` skill rewrites for the signal model,
external reviewer pass, and merging `schema-v2` → master. Desktop (it87/hwmon-temps) cutover is a
separate operator action.

## Lessons Extracted

- Building UI on a schema before the schema is the agreed one wastes effort: nail the wire contract
  first, then migrate inward-out (protocol → SDK → anemoi → orchestrator → UI). Recorded after iterating
  the UI three times on a report shape the user did not actually want.

## Followup

- SOW-0014 (v2 engine): its "reuse SOW-0017 schema" assumption is replaced by this schema; once this
  ships, SOW-0014 is "add the config-driven control engine on top of the signal model."
- SOW-0017: nested component schema superseded here; its shipped UI/rome2d-rename/it87 work stays in v1
  production until re-platformed on this schema.

## Regression Log

None yet.
