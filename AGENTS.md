# aiolos

> **Aiolos** (Αἴολος), keeper of the winds, commands the **anemoi** (ἄνεμοι — the winds).
> `aiolos` is a small, lean, domain-agnostic **orchestrator**; the **anemoi** are autonomous
> module binaries it spawns, supervises, and drives over a one-line-JSON stdio protocol.
> The flagship anemoi regulate airflow (fans) by temperature — but `aiolos` itself knows
> nothing about fans, GPUs, or IPMI.

## What this project is

- **`aiolos`** — the orchestrator (Rust, std threads, no async runtime). Spawns/supervises
  anemoi, drives the heartbeat, routes declared data flows between them, holds all state, serves
  a read-only status web page. Lean (no GC; low-MB binary, few-MB RSS), memory-safe.
- **anemoi** — autonomous module binaries (any language) implementing the protocol: `detect`
  (report the IDs they manage) and `run <ID>` (act each tick, report readings).
  - `nvidia` — per-GPU onboard fan control via NVML.
  - `asrock16-2t` — ASRockRack ROME2D16-2T board fans via IPMI (`/dev/ipmi0`), driven by GPU
    temps routed from `nvidia` plus its own CPU/board sensors.
- Process isolation is the core guarantee: each module instance is its own OS process, so a hung
  or lost device can never stall the orchestrator or sibling modules.

Authoritative design: [`DESIGN.md`](DESIGN.md). Contracts: `.agents/sow/specs/`.

## Layout

```
aiolos/                  orchestrator crate (Rust)
anemoi/nvidia/           nvidia anemos crate (Rust, nvml-wrapper)
anemoi/asrock16-2t/      asrock anemos (Rust; IPMI via /dev/ipmi0 raw, or libfreeipmi FFI)
systemd/aiolos.service
packaging/               install.sh / update.sh
```
Install target `/opt/aiolos/`: binaries in `bin/`, config in `etc/` (registry `aiolos.conf`,
per-module `*.curve.json`). Public repo: `github.com/ktsaou/aiolos`.

## Goals

Provide a lean, dependency-light, always-on orchestrator that supervises autonomous
device/signal modules with hard process isolation, and ship the first two modules (GPU fans,
board fans) to regulate this host's airflow by temperature. Success = the GPUs and CPUs stay
safely cooled by aiolos-driven fans, a hung/lost device never affects others, and anyone can add
a new module in any language without touching the orchestrator. The orchestrator stays
domain-agnostic; all device knowledge lives in the anemoi.

## SOW System

This project uses a local Statement of Work system. It is **self-contained** in this repo:
normal SOW work must not depend on `~/.agents`, global skills, templates, or scripts. Use this
`AGENTS.md`, the project-local SOW files, specs, project skills, and the active SOW.

### Roles
- **User:** purpose, scope decisions, design forks, risk acceptance, destructive approvals, final judgment.
- **Assistant:** investigation, evidence, implementation, tests/validation, reviews, docs, concise reporting.

### Required First Checks
Before non-trivial work: (1) read pending/current SOWs for overlap/decisions; (2) read relevant
specs under `.agents/sow/specs/`; (3) inspect `.agents/skills/project-*/SKILL.md` and load every
runtime skill whose trigger matches; (4) inspect code/docs as ground truth; (5) ask the user
only for irreducible product/design/risk decisions.

### Sensitive Data In Durable Artifacts
SOWs, specs, docs, skills, instructions, and code comments are commit-ready — treat them as
public.

CRITICAL: Never write raw sensitive data to durable artifacts. This includes passwords, API
keys, bearer tokens, SNMP communities, private keys, connection strings with embedded
credentials, session cookies, customer/personal identifiers, non-private customer-identifying IP
addresses, private endpoints, account IDs, and proprietary incident details.

Write only sanitized evidence: placeholders (`[REDACTED_SECRET]`, `[CUSTOMER]`,
`[PRIVATE_ENDPOINT]`); cite paths/fields/error-classes instead of values; summarize logs with
minimal redacted snippets. For aiolos specifically: the **BMC IP and IPMI credentials**, host
serials, and similar belong in operator config or `*.local.md` — never in committed artifacts.
If sensitive data is needed to proceed, stop and ask.

### Open-Source Reference Evidence
Cite external open-source repositories as `owner/repo @ commit` plus repository-relative paths —
never workstation absolute paths. Resolve `owner/repo` from the remote, record the commit.

### Git Worktrees
Assistants must not create git worktrees on their own. Create one only when the user explicitly
asks or approves.

### Pre-Implementation Gate
Implementation must not begin until the active SOW has a concrete `## Pre-Implementation Gate`
(problem/root-cause, evidence reviewed, affected contracts, patterns to reuse, risk/blast-radius,
sensitive-data plan, ordered implementation plan, validation plan, artifact-impact plan, open
decisions). `TBD`/`N/A` are invalid unless justified. Unresolved unknowns block implementation —
ask the user.

### When A SOW Is Required
Non-trivial work (features, behavioral bug fixes, refactors, migrations, process/spec/skill
changes, regressions, unclear risk) needs a SOW. Trivial work (typos, formatting, mechanical
renames) does not. When unsure, treat as non-trivial.

### SOW Locations
- `open` → `.agents/sow/pending/` · `in-progress`/`paused` → `.agents/sow/current/` ·
  `completed`/`closed` → `.agents/sow/done/`.
- Create new SOWs from `.agents/sow/SOW.template.md`. Filename `SOW-NNNN-YYYYMMDD-{slug}.md`.
- Empty SOW dirs keep a `.gitkeep`. **One SOW at a time.**

### SOW Completion And Commit
The successful terminal status is **`completed`** (never `Status: done` or `Status: complete`).
When closing: finish work+docs+specs+skills+validation+follow-up mapping; set `Status:
completed`; move the file to `.agents/sow/done/`; and commit the work + artifact updates + status
change + move **as one commit** unless the user requested a different split. Do not create a
separate commit just to mark/move the SOW.

### Validation Gate
A SOW cannot complete without: acceptance-criteria evidence; tests/equivalent validation;
real-use evidence when runnable; reviewer findings + handling; same-failure search; sensitive-data
gate; artifact-maintenance gate; spec update (or reasoned skip); skill update (or reasoned skip);
lessons; follow-up mapping. Generic "N/A" is invalid.

### Regressions
If behavior a completed SOW claimed working breaks: move the original SOW from `done/` back to
`current/`, mark it `in-progress` with a regression note, and append a dated
`## Regression - YYYY-MM-DD` section **at the end** (never prepend above the original narrative).
Do not open a new SOW for a true regression.

### Specs
Specs (`.agents/sow/specs/`) are memory of WHAT this project does/contracts. Update when shipped
work changes behavior, public contracts (the protocol!), data formats, defaults, or operational
guarantees. If specs and code disagree, record it in the active SOW and resolve/track it.

### Project Skills
Project skills (`.agents/skills/project-*/`) are memory of HOW to work here — mandatory hooks.
Before non-trivial work, inspect their descriptions and load every matching skill. Do not create
generic filler skills.

### Project Skills Index
- `.agents/skills/project-anemos-protocol/` — Trigger: **mandatory** before editing the
  orchestrator's protocol handling, any anemos's stdin/stdout, or claiming protocol conformance.
  Enforces: the one-line-JSON request/response contract, stdout=protocol / stderr=logs, the
  detect/apply/shutdown/hello messages, half-duplex + timeout, fail-safe-on-EOF.
- `.agents/skills/project-create-anemos/` — Trigger: **mandatory** when creating a new anemos
  (module) for any device/signal. Enforces: the module contract, detect/apply/shutdown, fail-safe
  (restore to firmware/auto on exit), registry wiring (`input=`), curve config, test checklist.

### Project-specific commands
```bash
cargo build --release            # build orchestrator + Rust anemoi
cargo test                       # unit/integration tests
cargo clippy --all-targets       # lints (keep clean)
cargo fmt --all                  # formatting
# protocol smoke test: echo one JSON line into a module's stdin, read one JSON line back
```
(Firm up as the workspace lands; keep current.)

### Project-specific overrides
- Orchestrator language is **Rust with std threads** (no async runtime) for leanness — do not
  introduce tokio/async or a GC language without a user-approved SOW decision.
- The C `nvfd` at `/opt/nvfd/` keeps cooling the GPUs in production until aiolos is built, tested,
  and explicitly cut over. Do not stop it without user approval.

### User Decisions
Present evidence (files/lines), numbered options with pros/cons/risks, a recommendation, and
record the decision in the active SOW before implementation.

Project SOW status: initialized
