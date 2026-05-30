# SOW-0011 - Status web dashboard redesign (themed, dynamic, animated) + ANSI-escape fix

## Status

Status: open

Sub-state: requested 2026-05-30. Not started. Gate at activation. Contains one quick bug-fix that
can be pulled out and shipped immediately (see ANSI escape, below).

## Requirements

### Purpose
Replace the basic status page with a beautiful, dynamic, themed dashboard that is "very pleasing to
watch" — reflecting the modular (aiolos + anemoi) architecture, with multiple tabs and a great home
tab.

### User Request
> "the web page needs a lot of love. The messages seems broken with `esc[2m` and other tty control
> flags in them. And I would love the dark/white themed modules dashboard, in designer style
> (impressive ancient greece abstract styling for aiolos and anemoi), with animated SVGs depending
> on system pressure, curves visualization, time-series data, graphs, etc — something very pleasing
> to watch. Given the modularity of the system, this dashboard should be dynamic, with multiple tabs
> for the different views, and a great, very appealing, home tab."

### Assistant Understanding
Facts:
- Current status server: hand-rolled HTML + `/status.json`, dependency-light (DESIGN §8), served from
  `aiolos/src/status_page.rs` on `status_bind` (default `0.0.0.0:9876`).
- **BUG (live now):** captured module `stderr` contains raw ANSI color codes (e.g. `[2m`,
  `[32m`) shown verbatim in the status page (and the journal). Root cause: the `anemos` SDK's
  tracing subscriber emits ANSI to stderr (`anemos/src/run.rs` `init_logging`, no `.with_ansi(false)`),
  and aiolos captures that stderr into the per-instance tail. **Fix = one line:** `.with_ansi(false)`
  in `init_logging` (anemos stderr is always captured, never a user TTY); optionally also strip ANSI
  in the status page defensively. This needs a rebuild + redeploy to take effect on the running aiolos.
- Time-series/graphs imply aiolos must retain a short history of readings (a per-metric ring buffer)
  OR the dashboard pulls history from Prometheus/Netdata (see SOW-0007).

Design vision (from the request): dark/light themes; ancient-Greece abstract "designer" styling for
aiolos (Aiolos, keeper of the winds) and the anemoi (the winds); animated SVGs whose motion reflects
system "pressure" (temps/duties); curve visualization (the temp→duty curve + the live operating
point); time-series graphs; multiple tabs; a striking home tab.

### Acceptance Criteria
- No raw escape codes anywhere in the UI (ANSI fix shipped).
- A themed (dark/light), multi-tab dashboard: a home/overview tab + per-module/per-instance views +
  curve view + time-series view.
- Dynamic — tabs/cards generated from the live module/instance set (works for any anemoi).
- Animated SVGs that respond to live system pressure; curve + live operating point; time-series of
  temps/duties/RPMs.
- Stays dependency-light / self-served by aiolos (no heavy backend); performant; read-only.

## Analysis
Sources: `aiolos/src/status_page.rs`, `/status.json` shape, `anemos/src/run.rs` `init_logging`
(ANSI), DESIGN §8 (status page is meant to be lean). Relationship to SOW-0007 (Prometheus) for
time-series storage.

## Pre-Implementation Gate
Status: blocked (open SOW; gate at activation)

Open decisions:
- **Time-series source:** an in-aiolos ring buffer (keeps it self-contained, bounded memory) vs.
  rely on Prometheus/Netdata (SOW-0007) and have the dashboard query those. The lean ethos favors a
  small in-process ring buffer for the live view.
- **Frontend tech vs the "lean, no-deps" ethos:** the orchestrator is deliberately dependency-light.
  A rich animated dashboard implies meaningful HTML/CSS/SVG/JS — decide how much ships embedded in
  the binary (single self-served asset bundle, no external CDNs) vs a separate build step. No heavy
  server-side deps.
- **Branding/theming:** the ancient-Greece abstract visual language, palette, typography, the
  Aiolos/anemoi motifs, and what "system pressure" maps to visually.
- **Scope/phasing:** ship the ANSI fix + a themed overview first, then add curve view, time-series,
  and the animated home tab iteratively.

## Plan (sketch — phased)
0. **Quick win:** `.with_ansi(false)` in `anemos` `init_logging` (+ defensive ANSI strip in the
   status page); rebuild + redeploy. (Can ship independently of the redesign.)
1. Status server: serve a bundled static dashboard + the live JSON; add a bounded readings ring
   buffer for time-series.
2. Frontend: themed multi-tab app (home/overview, modules, curve, time-series); animated SVGs driven
   by live pressure; curve + operating-point viz.
3. Iterate on the home tab + animations; theming.

## Execution Log
### 2026-05-30
- Created (open). Captured the request verbatim + the ANSI root cause (one-line fix). No code.

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
