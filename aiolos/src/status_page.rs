//! Read-only HTTP status page. Hand-rolled (dependency-light, no async).
//!
//! `GET /`            -> HTML dashboard (live readings, per-instance health, errors, stderr tail)
//! `GET /status.json` -> the same data as JSON
//! everything else    -> 404. The server never mutates state.

use crate::AppState;
use anyhow::Result;
use protocol::Reading;
use serde::Serialize;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tracing::info;

pub fn serve(bind: &str, state: Arc<RwLock<AppState>>) -> Result<()> {
    let listener = TcpListener::bind(bind)?;
    info!(bind=%bind, "status page listening");
    for stream in listener.incoming() {
        match stream {
            Ok(conn) => {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    let _ = handle(conn, &state);
                });
            }
            Err(e) => tracing::warn!(error=%e, "status accept error"),
        }
    }
    Ok(())
}

fn handle(mut conn: TcpStream, state: &Arc<RwLock<AppState>>) -> Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))?;

    // Read only the request line (first line); we don't need headers/body for a read-only GET.
    let mut buf = [0u8; 2048];
    let n = conn.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");

    let (status, ctype, body) = match path {
        "/" => ("200 OK", "text/html; charset=utf-8", render_html(state)),
        "/status" | "/status.json" => ("200 OK", "application/json", render_json(state)),
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            "Not Found".to_string(),
        ),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(response.as_bytes())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusJson<'a> {
    tick: u64,
    instances: Vec<InstanceJson<'a>>,
}

#[derive(Serialize)]
struct InstanceJson<'a> {
    module: &'a str,
    id: &'a str,
    name: &'a str,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    restart_count: u32,
    last_seen_tick: u64,
    ticks_since_seen: u64,
    readings: &'a [Reading],
    stderr_tail: Vec<String>,
}

fn render_json(state: &Arc<RwLock<AppState>>) -> String {
    let s = match state.read() {
        Ok(s) => s,
        Err(_) => return r#"{"error":"state lock poisoned"}"#.to_string(),
    };
    let tick = s.tick_count;
    let mut instances: Vec<InstanceJson> = s
        .instances
        .values()
        .map(|i| InstanceJson {
            module: &i.module_name,
            id: &i.id,
            name: &i.name,
            status: &i.last_status,
            error: i.last_error.as_deref(),
            restart_count: i.restart_count,
            last_seen_tick: i.last_seen_tick,
            ticks_since_seen: tick.saturating_sub(i.last_seen_tick),
            readings: &i.last_readings,
            stderr_tail: tail_lines(i, 10),
        })
        .collect();
    instances.sort_by(|a, b| (a.module, a.id).cmp(&(b.module, b.id)));

    serde_json::to_string_pretty(&StatusJson { tick, instances })
        .unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

fn render_html(state: &Arc<RwLock<AppState>>) -> String {
    let s = match state.read() {
        Ok(s) => s,
        Err(_) => return "<h1>aiolos</h1><p>state lock poisoned</p>".to_string(),
    };

    let mut rows = String::new();
    let mut instances: Vec<_> = s.instances.values().collect();
    instances.sort_by(|a, b| (&a.module_name, &a.id).cmp(&(&b.module_name, &b.id)));

    for i in &instances {
        let readings = i
            .last_readings
            .iter()
            .map(format_reading)
            .collect::<Vec<_>>()
            .join("<br>");
        let stderr = tail_lines(i, 10)
            .iter()
            .map(|l| esc(l))
            .collect::<Vec<_>>()
            .join("<br>");
        let age = s.tick_count.saturating_sub(i.last_seen_tick);
        rows.push_str(&format!(
            "<tr class=\"{cls}\"><td>{module}</td><td class=\"mono\">{id}</td><td>{name}</td>\
             <td>{status}</td><td>{err}</td><td>{rc}</td><td>{age}</td>\
             <td>{readings}</td><td class=\"mono small\">{stderr}</td></tr>",
            cls = status_class(&i.last_status),
            module = esc(&i.module_name),
            id = esc(&i.id),
            name = esc(&i.name),
            status = esc(&i.last_status),
            err = esc(i.last_error.as_deref().unwrap_or("")),
            rc = i.restart_count,
            age = age,
        ));
    }

    if instances.is_empty() {
        rows.push_str("<tr><td colspan=\"9\"><em>no instances (detecting…)</em></td></tr>");
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta http-equiv="refresh" content="3">
<title>aiolos status</title>
<style>
 body{{font-family:system-ui,sans-serif;margin:1.2rem;color:#1a1a1a}}
 h1{{font-size:1.2rem}} .meta{{color:#666;font-size:.85rem;margin-bottom:.8rem}}
 table{{border-collapse:collapse;width:100%;font-size:.85rem}}
 th,td{{border:1px solid #ddd;padding:.3rem .5rem;text-align:left;vertical-align:top}}
 th{{background:#f4f4f4}}
 .mono{{font-family:ui-monospace,monospace}} .small{{font-size:.75rem;color:#555}}
 tr.ok td{{background:#f6fff6}} tr.error td,tr.timeout td,tr.dead td,tr.protocol_error td{{background:#fff4f4}}
</style></head><body>
<h1>aiolos status</h1>
<div class="meta">tick {tick} · {count} instance(s) · auto-refresh 3s · <a href="/status.json">JSON</a></div>
<table>
<tr><th>module</th><th>id</th><th>name</th><th>status</th><th>error</th><th>restarts</th>
<th>age (ticks)</th><th>readings</th><th>stderr tail</th></tr>
{rows}
</table>
</body></html>"#,
        tick = s.tick_count,
        count = instances.len(),
        rows = rows,
    )
}

fn format_reading(r: &Reading) -> String {
    let fields = r
        .fields
        .iter()
        .map(|(k, v)| format!("{}={}", esc(k), esc(&v.to_string())))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<b>{}</b> <span class=\"small\">{}</span> {}",
        esc(&r.label),
        esc(&r.kind),
        fields
    )
}

fn status_class(status: &str) -> &'static str {
    match status {
        "ok" => "ok",
        "error" => "error",
        "timeout" => "timeout",
        "dead" => "dead",
        "protocol_error" => "protocol_error",
        _ => "",
    }
}

fn tail_lines(entry: &crate::InstanceEntry, n: usize) -> Vec<String> {
    entry
        .stderr_tail
        .lock()
        .map(|t| t.iter().rev().take(n).rev().cloned().collect())
        .unwrap_or_default()
}

/// Minimal HTML escaping for untrusted strings (ids, module-reported labels, stderr lines).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
