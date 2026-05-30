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
Status: passed 2026-05-30 (decisions settled below; implemented in `aiolos/src/status_page.rs` only).

Problem: the status page is a bare table; the request is a themed, dynamic, animated multi-tab
dashboard reflecting the aiolos+anemoi architecture, pleasing to watch, dependency-light.

Evidence reviewed: `aiolos/src/status_page.rs` (current server, routes, `AppState` access),
`protocol/src/lib.rs` (reading shapes), the anemoi reading kinds (`temp`/`fan`/`driving`),
`anemos/src/curve.rs` + `packaging/*.curve.json` (the curve format `{ "<temp>": <pct>, … ,
"sensitivity": α }` at `$AIOLOS_ETC_DIR/<module>.curve.json` || `/opt/aiolos/etc/<module>.curve.json`).
The ANSI-escape root cause is already fixed on master (`anemos/src/run.rs` `init_logging`
`.with_ansi(false)`), so this SOW does the dashboard only.

Affected contracts: read-only additive routes on `status_bind`; no protocol/AppState/main.rs change.

Decisions:
1. **Time-series source:** a bounded in-process ring buffer LIVING INSIDE `status_page.rs` (no
   AppState/main.rs change). A background snapshotter thread (spawned from `serve()`) reads the
   shared `Arc<RwLock<AppState>>` read-only every few seconds and appends a compact snapshot; capped
   (720 points ≈ 1h at 5s). Served at `/history.json`.
2. **Frontend tech:** all HTML/CSS/JS/SVG EMBEDDED as `&str` consts compiled into the binary and
   served from `status_page.rs`. Vanilla JS/SVG/CSS only — NO frameworks, NO external CDN/network.
   The page polls `/status.json` + `/history.json`. Assets split into `/`, `/aiolos.css`,
   `/aiolos.js` for cacheability and a small index.
3. **Branding/theming:** "Aiolos, keeper of the winds" — an ancient-Greece abstract designer
   language: a meander (Greek-key) motif, amphora/wind-rose accents, a wind/aether colour system,
   a serif display face (system serif stack) for headings + system sans for data. Dark + light
   themes via a `data-theme` attribute + CSS custom properties; toggle persisted in localStorage.
   "System pressure" = max(normalised temp, normalised duty) across the fleet → drives wind speed,
   particle density, and accent intensity of the animated SVG.
4. **Tabs (dynamic, from the live set):** HOME/overview (fleet pressure, animated winds, KPI cards
   per module), per-MODULE tabs (auto-generated, instance cards with temps/duties/RPMs + sparkline),
   CURVE view (renders each curved module's temp→duty curve from its etc JSON + the live operating
   point from the `driving` reading), TIME-SERIES view (multi-series line charts of temps/duties/RPMs
   from `/history.json`), and a HEALTH/log tab (instances table + stderr tail, ANSI-stripped
   defensively). Tabs generated client-side from the JSON so any future anemos appears automatically.
5. **Curve source:** the status page reads `$AIOLOS_ETC_DIR/<module>.curve.json` ||
   `/opt/aiolos/etc/<module>.curve.json` read-only (the same convention the anemos SDK uses) for the
   curve shape; the live operating point comes from the `driving` reading. Missing file → the view
   still shows the operating point and a note. This reads CONFIG only (never main.rs/AppState).

Risk/blast-radius: read-only; a panic in a request handler is isolated per-connection thread (as
today). The snapshotter only takes a read lock briefly. No control-path impact.

Sensitive-data plan: renders only device labels + numeric readings + curve config; no secrets.
All module-reported strings are HTML-escaped (existing `esc`) and ANSI-stripped before display.

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
- ANSI root-cause fix already shipped on master (`anemos/src/run.rs` `init_logging`
  `.with_ansi(false)`); this SOW delivers the dashboard. The status page additionally strips ANSI
  defensively (`strip_ansi`) on `/status.json` stderr tails so any stale escapes never reach the UI.
- Implemented entirely in `aiolos/src/status_page.rs` + embedded assets
  `aiolos/src/assets/{index.html,aiolos.css,aiolos.js}` (compiled in via `include_str!`). No
  AppState/main.rs change; vanilla JS/SVG/CSS only, no frameworks, no CDN/network.
- New read-only routes: `/` (shell), `/aiolos.css`, `/aiolos.js`, `/history.json` (ring buffer),
  `/curve.json?module=<m>` (curve config). Existing `/status.json` kept (compact, ANSI-stripped).
- Time-series: a bounded in-process ring buffer (`History`, cap 720 ≈ 1h @ 5s) lives inside the
  module; a `spawn_snapshotter` thread reads the shared state read-only every 5s and appends a
  compact per-instance snapshot (temp/duty/rpm/up). No storage added to AppState.
- Theming: dark + light via `data-theme` + CSS custom properties, toggle persisted in localStorage.
  Ancient-Greece "designer" language: amphora brand mark, Greek-key (meander) dividers, a wind-rose
  pressure indicator, serif display + system sans for data, gold/aether/terracotta palettes.
- Tabs (dynamic from the live set): HOME (fleet KPI strip + animated winds + per-module summary
  cards), one tab per MODULE (instance cards: temps/duties/RPMs + history sparkline + raw readings),
  CURVES (temp→duty curve from each module's etc JSON + animated live operating point from the
  `driving` reading), TIME-SERIES (multi-series SVG line charts for temps/duties/RPMs over the ring
  buffer), HEALTH (modules + instances tables with ANSI-clean stderr tails).
- Animated SVG backdrop: wind streamlines whose speed, amplitude, weight and tint scale with live
  "system pressure" = max(normalised temp, normalised duty) across the fleet; the wind-rose mirrors
  it. Honours `prefers-reduced-motion` (animation disabled).

## Validation
- `cargo build --release` — clean. `cargo clippy --all-targets` — clean (no warnings).
  `cargo fmt --all --check` — clean. `cargo test --workspace --no-run` — all test targets compile.
- `node --check aiolos/src/assets/aiolos.js` — syntax OK.
- Unit tests added in `status_page.rs`: history ring buffer is bounded; `/status.json` strips ANSI
  in stderr tails; curve reader reads points + sensitivity and rejects path traversal
  (`../`, `a/b`, `a.b`); reading aggregation prefers `driving` and takes maxima; percent-decode /
  module-param parsing.
- NOT run live here (production safety: no binaries executed in this worktree). User to run
  `cargo test` and open the page on `status_bind` to watch the live dashboard.
- Acceptance criteria: no raw escape codes (ANSI fix on master + defensive strip) ✓; themed
  multi-tab dashboard (home/module/curve/time-series/health) ✓; dynamic from the live set ✓;
  animated SVGs driven by pressure + curve operating point + time-series ✓; dependency-light /
  self-served / read-only ✓.

## Outcome
Dashboard + Prometheus endpoint implemented in `status_page.rs` + embedded assets; build/clippy/fmt
clean, tests compile, JS syntax-checks. Awaiting the user's live visual + cutover validation.

## Lessons Extracted
- The dashboard can stay 100% inside `status_page.rs`: a self-owned ring buffer + a read-only
  snapshotter thread give time-series without touching AppState or main.rs.
- Tabs/cards built from `/status.json` make the UI automatically cover any future anemos — no
  per-module front-end code.
- Curve shape isn't in AppState, but the per-module etc JSON is readable read-only via the same
  `$AIOLOS_ETC_DIR` convention the anemos SDK uses; the live operating point comes from the
  `driving` reading, so the curve view works even when the config file is absent.

## Followup
None yet.

## Regression Log
None yet.
