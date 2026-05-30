# SOW-0014 - Typed module kinds + `input=` validation (producer/consumer contracts)

## Status

Status: open

Sub-state: discussed with the user 2026-05-30; model proposed, a couple of decisions open. Not
started. Foundational; SOW-0009 depends on it.

## Requirements

### Purpose
Give modules a declared **kind/role** (what they produce and what input they require), so the
orchestrator can **validate `input=` wiring at startup** and **fail loudly on invalid configurations**
(e.g. routing temperatures into a power-control module) instead of silently relaying readings the
consumer ignores.

### User Request
> 2026-05-30: "scope modules as temp, fans, power-state, power-control… each module should report
> what it is so that `input=` directives know what to do at the orchestrator. fans need temps.
> power-control needs power-state… producer-consumer relationships must be identified and invalid
> configurations (giving temperatures to a power-control module) should fail at validation without
> unpredictable results."

### Assistant Understanding
Facts:
- Routing is **untyped** today: `input=X` relays *all* of X's readings; the consumer filters by
  reading `type` (`asrock` keeps `type:"temp"`). A mismatched wiring doesn't fail — it's silently
  ignored. There is no producer/consumer contract.
- Readings already carry a `type` (`temp`, `fan`, …); the missing piece is the **module-level**
  declaration of produces/requires and the orchestrator-side validation.
- Ordering is NOT the issue — the scheduler (SOW-0013) keeps producers/consumers independent with
  cached last-results; this SOW is purely about *typing + validation*.

Proposed model (to confirm):
- Each **module declares** (source of truth = the module, reported via `hello`/`detect`, not config):
  `produces: [<reading-types>]` and `requires: <reading-type | none>`.
- New reading types as needed: `power-state` (and the existing `temp`, `fan`).
- Orchestrator validation at startup: for every `input=X` on module `M`,
  `X.produces ⊇ { M.requires }`; otherwise **refuse to start** with a clear error.
- Examples on this host: `nvme`/`ipmi-temps` → produces `[temp]`, requires none; `nvidia` → produces
  `[temp,fan]`, requires none (self-senses), controls fans; `asrock16-2t` → produces `[fan]`,
  **requires `temp`**; `nut` → produces `[power-state]`; `nvidia-powercap` → **requires `power-state`**.

### Acceptance Criteria
- A module declares its produces/requires; the orchestrator reads it and **validates every `input=`**.
- A mismatch (`input=nut` into a temp-requiring fan controller; `input=nvme` into a power-state
  controller) **fails at startup** with a precise message — no silent ignore, no unpredictable result.
- Valid wirings work unchanged; sensors (require none) accept no `input=` mismatch by construction.
- Protocol + specs document the declarations and validation.

## Analysis
Sources: `protocol` (`hello`/`detect`/`Reading.type`), `aiolos` registry/config/routing
(`registry.rs`, `config.rs`, `main.rs::build_inputs`), `anemos` (where modules would declare kinds),
the protocol skill/spec. Reuses the existing reading-`type` taxonomy.

## Pre-Implementation Gate
Status: needs-user-decision (model specifics) → then ready

Open decisions:
- **Declaration channel:** extend `hello` (module emits `produces`/`requires` at startup —
  recommended) vs `detect` per-entry vs a new `describe` one-shot. Validation then runs once the
  orchestrator has each module's declaration (before spawning `run` instances).
- **Multiplicity:** can a module `require` more than one type, or produce several? (Default: produce a
  set; require zero-or-one — keeps validation simple. Revisit if a consumer needs two input kinds.)
  **Concrete case:** SOW-0015 (`nvidia-powercap` thermal trigger) requires BOTH `power-state` AND
  `temp` — the first real two-input consumer. So `requires` must support a **set**, not zero-or-one.
- **Type registry:** the canonical set of reading/role types (`temp`, `fan`, `power-state`, …) and how
  new ones are added (open string vs a closed enum).
- **Strictness:** the user chose **fail** (not warn) on a mismatch — confirm fail-hard at startup
  (consistent with SOW-0012's "fail-to-start on bad config").
- **Back-compat:** existing modules (`nvidia`, `asrock16-2t`, `nvme`) must declare kinds; a module that
  declares nothing → treat as untyped/legacy (skip validation) or require declaration?

## Plan (sketch)
1. Protocol: add `produces`/`requires` to the module's startup declaration (`hello`), + the
   `power-state` reading type; update the protocol spec + project-anemos-protocol skill.
2. anemos SDK: let a module declare its kind in `ModuleInfo`/`Anemos`; emit it in `hello`.
3. Orchestrator: collect declarations; validate every `input=` at startup; fail with a clear error.
4. Declare kinds on the shipped modules; tests (valid + invalid wirings); docs.

## Execution Log
### 2026-05-30
- Created (open) from the 2026-05-30 design discussion. Model proposed; specifics open. No code.

## Validation
Pending.

## Outcome
Pending.

## Lessons Extracted
Pending.

## Followup
None yet.

## Regression Log
None yet.
