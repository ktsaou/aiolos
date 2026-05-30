# SOW-0007 - Prometheus `/metrics` endpoint (feed aiolos data into the observability stack)

## Status

Status: open

Sub-state: idea captured from the 2026-05-30 server inspection. Not started. Gate at activation.

## Requirements

### Purpose
Expose aiolos's live state (per-device temps, fan duties, fan RPMs, driving temps, per-instance
health) as Prometheus metrics, so it flows into this host's existing monitoring (Netdata, Prometheus,
Grafana) for trends, dashboards, and alerting — instead of being siloed in aiolos's status page.

### User Request
> Idea #2 (highest observability leverage): this box already runs Netdata (19999), Prometheus
> (9090), Grafana, DCGM-exporter (9410) — pipe aiolos data in.

### Assistant Understanding
Facts:
- aiolos serves an HTML status page + `/status.json` (read-only) on `status_bind` (default
  `0.0.0.0:9876`); it holds structured `readings` per instance (type/label + numeric fields).
- The host runs a full Prometheus/Netdata/Grafana stack; nothing currently scrapes aiolos.
- The status server is hand-rolled and dependency-light (DESIGN §8) — keep it that way.

### Acceptance Criteria
- A `/metrics` endpoint returns valid Prometheus text-format exposition (labels: `module`, `id`,
  `label`, `type`/field) for temps, duties, RPMs, driving temps, and per-instance status/restarts.
- Scrapes cleanly (promtool/`curl`); appears in the stack; no behavior change to control.
- No new heavy dependencies (consistent with the lean ethos).

## Analysis
Sources: `aiolos/src/status_page.rs`, `aiolos/src/main.rs` (AppState/blackboard), the host
observability stack (`~/CLAUDE.md`). The readings are already structured; this is a rendering +
endpoint addition.

## Pre-Implementation Gate
Status: passed 2026-05-30 (decisions settled below; implemented in `aiolos/src/status_page.rs` only).

Problem / root-cause: aiolos's live readings (temps, fan duty, RPM, driving temp, instance health)
are siloed in the status page; the host's Prometheus/Netdata/Grafana stack cannot scrape them.

Evidence reviewed: `protocol/src/lib.rs` (`Reading{kind,label,fields}`); reading shapes from the
anemoi — `temp`→`{temp}`, `fan`→`{pwm,rpm?}`, `driving`→`{temp,raw,pct}`
(`anemoi/nvidia/src/main.rs:94-103`, `anemoi/asrock16-2t/src/main.rs:102-135`); `AppState`
(`aiolos/src/main.rs:434-462`) exposes `instances` (status, restart_count, last_seen_tick,
last_readings) and `tick_count`.

Affected contracts: adds a NEW read-only route `/metrics`; no change to the protocol, control
path, or existing routes.

Decisions:
1. **Metric schema** (final):
   - `aiolos_temp_celsius{module,id,instance_name,label}` — every `temp` reading's `temp` field.
   - `aiolos_fan_rpm{module,id,instance_name,label}` — `fan` reading `rpm` (only when present).
   - `aiolos_fan_duty_percent{module,id,instance_name,label}` — `fan` reading `pwm`.
   - `aiolos_driving_celsius{module,id,instance_name,label}` — `driving` reading `temp` (smoothed);
     `aiolos_driving_raw_celsius{...}` for `raw`; `aiolos_driving_duty_percent{...}` for `pct`.
   - `aiolos_instance_up{module,id,instance_name}` — 1 if last_status==ok else 0.
   - `aiolos_instance_restarts_total{module,id,instance_name}` (counter).
   - `aiolos_instance_ticks_since_seen{module,id,instance_name}` (staleness, gauge).
   - `aiolos_module_detect_up{module}` — 1 if detect_status==ok else 0.
   - `aiolos_tick` (gauge, the orchestrator's heartbeat counter).
2. **Pull**, at `/metrics` on the existing `status_bind` (no new bind, no remote_write).
3. **Cardinality / duplicate labels:** when the SAME (module,id,label) appears more than once in an
   instance's readings (e.g. two CPU sockets both labelled "CPU"), append `_2`, `_3`… to the `label`
   value so each series is unique. Label values are sanitized (escape `\`, `"`, newline) and the
   `instance_name` carries the human name.
4. **No new dependencies** — hand-rolled text writer, same lean ethos as the status page.

Validation plan: build + clippy + fmt clean; unit test the renderer against a synthetic `AppState`
(duplicate-label disambiguation, all reading kinds, escaping); user scrapes with promtool/curl on
the live host.

Sensitive-data plan: metrics expose only device labels and numeric readings — no secrets. The
`/metrics` route inherits the status page's existing exposure (operator controls `status_bind`).

## Plan
1. Add a Prometheus text renderer over the existing readings/state.
2. Serve it at `/metrics` on the status server.
3. Doc the metric schema; validate with promtool + a scrape from the local Prometheus.

## Execution Log
### 2026-05-30
- Created (open) from the server-inspection idea list. No code.
- Gate passed; implemented entirely in `aiolos/src/status_page.rs` (no protocol/AppState/main.rs
  change). Added `GET /metrics` (content-type `text/plain; version=0.0.4`).
- `render_metrics()` walks the read-locked `AppState`, grouping samples by family via a `MetricBuf`
  so each `# HELP`/`# TYPE` header is emitted exactly once (Prometheus grouping rule). Emits:
  `aiolos_tick`, `aiolos_module_detect_up`, `aiolos_instance_up`, `aiolos_instance_restarts_total`
  (counter), `aiolos_instance_ticks_since_seen`, `aiolos_temp_celsius`, `aiolos_fan_duty_percent`,
  `aiolos_fan_rpm`, `aiolos_driving_celsius`, `aiolos_driving_raw_celsius`,
  `aiolos_driving_duty_percent`.
- Labels `module,id,instance_name,label`; values JSON-escaped via `json_str` (escapes `\` `"`
  control chars) AND ANSI-stripped. Duplicate (kind,label) within an instance → `_2`,`_3`… suffix.
- Numeric formatting via `fmt_num` (whole floats render without `.0`).

## Validation
- `cargo build --release` — clean. `cargo clippy --all-targets` — clean (no warnings).
  `cargo fmt --all --check` — clean. `cargo test --workspace --no-run` — all test targets compile.
- Unit tests added (in `status_page.rs`): `metrics_render_all_reading_kinds` (every kind + single
  family headers), `metrics_disambiguate_duplicate_labels` (two "CPU" → `CPU`,`CPU_2`),
  `metrics_down_when_not_ok` (up=0 / detect=0), `metrics_escape_label_values`,
  `fmt_num_drops_trailing_zero`.
- NOT run here (production safety: no binaries executed in this worktree). User to run `cargo test`
  + scrape the live endpoint with `promtool check metrics` / `curl` and confirm it appears in the
  Prometheus/Netdata stack.
- Acceptance criteria: `/metrics` returns valid Prometheus exposition with the agreed schema ✓
  (renderer + tests); no control-path change ✓ (read-only, read lock only); no new deps ✓.

## Outcome
Implemented, build/clippy/fmt clean, tests compile. Awaiting the user's live scrape validation.

## Lessons Extracted
- The `driving` reading already carries `{temp,raw,pct}`, giving temp + commanded duty + raw temp
  per fan controller — three clean gauges with no extra plumbing.
- Grouping samples by family before writing is required: interleaving families breaks Prometheus
  parsing. A small `MetricBuf` keeps the renderer single-pass and correct.

## Followup
None yet.

## Regression Log
None yet.
