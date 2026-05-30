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
Status: blocked (open SOW; gate at activation)

Open decisions:
- **Metric naming & labels:** e.g. `aiolos_temp_celsius{module,id,label}`,
  `aiolos_fan_rpm{module,id,label}`, `aiolos_fan_duty_percent{...}`, `aiolos_driving_celsius{...}`,
  `aiolos_instance_up{...}`, `aiolos_instance_restarts_total{...}`. Settle on a clean schema.
- **Pull vs push:** a `/metrics` pull endpoint (simplest, standard) vs Prometheus `remote_write`
  push. Pull recommended.
- **Bind/exposure:** reuse the status `status_bind` port/path, or a separate metrics bind.
- **Cardinality:** duplicate CPU labels (two sockets) — disambiguate by socket to avoid label clashes.

## Plan
1. Add a Prometheus text renderer over the existing readings/state.
2. Serve it at `/metrics` on the status server.
3. Doc the metric schema; validate with promtool + a scrape from the local Prometheus.

## Execution Log
### 2026-05-30
- Created (open) from the server-inspection idea list. No code.

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
