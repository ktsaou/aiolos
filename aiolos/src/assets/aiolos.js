/* Aiolos dashboard — vanilla JS, no frameworks, no external network.
   Polls /status.json + /history.json; builds tabs dynamically from the live module/instance set;
   renders home/overview, per-module, curve, time-series and health views; drives an animated wind
   backdrop whose intensity reflects live "system pressure" (max normalised temp/duty). */
'use strict';

const SVGNS = 'http://www.w3.org/2000/svg';
const POLL_MS = 2500;
const SPARK_POINTS = 40;

const state = {
  status: null,      // latest /status.json
  history: null,     // latest /history.json
  tab: 'home',       // active tab id
  pressure: 0,       // 0..1 fleet pressure
  curveCache: {},    // module -> curve json
  failures: 0,
};

/* ---------- tiny DOM helpers ---------- */
function el(tag, attrs, children) {
  const e = document.createElement(tag);
  if (attrs) for (const k in attrs) {
    if (k === 'class') e.className = attrs[k];
    else if (k === 'html') e.innerHTML = attrs[k];
    else if (k === 'text') e.textContent = attrs[k];
    else e.setAttribute(k, attrs[k]);
  }
  if (children) for (const c of [].concat(children)) if (c != null) e.append(c.nodeType ? c : document.createTextNode(c));
  return e;
}
function svg(tag, attrs) {
  const e = document.createElementNS(SVGNS, tag);
  if (attrs) for (const k in attrs) e.setAttribute(k, attrs[k]);
  return e;
}
const $ = (id) => document.getElementById(id);
const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
const fmt = (v, d = 0) => (v == null || isNaN(v)) ? '–' : Number(v).toFixed(d);

/* ---------- reading aggregation (mirrors the server's headline series) ---------- */
function aggregate(readings) {
  let maxTemp = null, drivingTemp = null, drivingPct = null, maxPwm = null, maxRpm = null;
  for (const r of readings || []) {
    if (r.type === 'temp' && r.temp != null) maxTemp = maxTemp == null ? r.temp : Math.max(maxTemp, r.temp);
    else if (r.type === 'driving') { if (r.temp != null) drivingTemp = r.temp; if (r.pct != null) drivingPct = r.pct; }
    else if (r.type === 'fan') {
      if (r.pwm != null) maxPwm = maxPwm == null ? r.pwm : Math.max(maxPwm, r.pwm);
      if (r.rpm != null) maxRpm = maxRpm == null ? r.rpm : Math.max(maxRpm, r.rpm);
    }
  }
  return { temp: drivingTemp != null ? drivingTemp : maxTemp, duty: drivingPct != null ? drivingPct : maxPwm, rpm: maxRpm };
}

// Normalised "pressure" of one instance: blend of temp (30..90C) and duty (0..100%).
function instPressure(agg) {
  const t = agg.temp == null ? 0 : clamp((agg.temp - 30) / 60, 0, 1);
  const d = agg.duty == null ? 0 : clamp(agg.duty / 100, 0, 1);
  return Math.max(t, d);
}

function modulesMap(status) {
  // module -> { detect, instances:[] }
  const m = {};
  for (const md of (status.modules || [])) m[md.module] = { detect: md.detect_status, detect_error: md.detect_error, instances: [] };
  for (const i of (status.instances || [])) {
    if (!m[i.module]) m[i.module] = { detect: '?', instances: [] };
    m[i.module].instances.push(i);
  }
  return m;
}

/* ---------- data fetch loop ---------- */
async function poll() {
  try {
    const [s, h] = await Promise.all([
      fetch('/status.json', { cache: 'no-store' }).then(r => r.json()),
      fetch('/history.json', { cache: 'no-store' }).then(r => r.json()).catch(() => ({ snaps: [] })),
    ]);
    state.status = s; state.history = h; state.failures = 0;
    setConn('ok', 'live');
    onData();
  } catch (e) {
    state.failures++;
    setConn(state.failures > 2 ? 'down' : 'stale', state.failures > 2 ? 'offline' : 'reconnecting');
  } finally {
    setTimeout(poll, POLL_MS);
  }
}
function setConn(cls, txt) { const c = $('conn'); if (c) { c.className = 'conn ' + cls; c.textContent = txt; } }

function onData() {
  const s = state.status;
  // fleet pressure = max instance pressure
  let p = 0, count = 0;
  for (const i of (s.instances || [])) { p = Math.max(p, instPressure(aggregate(i.readings))); count++; }
  state.pressure = p;
  updateHeader(count);
  buildTabs();
  render();
}

/* ---------- header / pressure rose ---------- */
function updateHeader(count) {
  $('tickbox').textContent = 'tick ' + (state.status.tick ?? '–');
  const mods = new Set((state.status.instances || []).map(i => i.module));
  $('fleet-sub').textContent = count
    ? `${count} instance${count > 1 ? 's' : ''} across ${mods.size} module${mods.size > 1 ? 's' : ''}`
    : 'no instances yet';
  // pressure rose
  const pct = Math.round(state.pressure * 100);
  $('pressure-num').textContent = pct;
  const col = pressureColor(state.pressure);
  $('pressure-num').style.fill = col;
  const rays = $('rose-rays'); rays.textContent = '';
  const N = 16;
  for (let k = 0; k < N; k++) {
    const a = (k / N) * Math.PI * 2 - Math.PI / 2;
    const lit = (k / N) <= state.pressure;
    const r0 = 10, r1 = 12 + (lit ? 22 * state.pressure : 6);
    const ray = svg('line', {
      x1: 40 + Math.cos(a) * r0, y1: 40 + Math.sin(a) * r0,
      x2: 40 + Math.cos(a) * r1, y2: 40 + Math.sin(a) * r1,
      class: 'rose-ray', stroke: lit ? col : 'var(--line-strong)',
      'stroke-width': lit ? 2.4 : 1.4, 'stroke-opacity': lit ? 0.95 : 0.4,
    });
    rays.append(ray);
  }
}
function pressureColor(p) {
  if (p >= 0.8) return 'var(--bad)';
  if (p >= 0.55) return 'var(--warn)';
  if (p >= 0.3) return 'var(--accent)';
  return 'var(--aether)';
}

/* ---------- tabs ---------- */
function buildTabs() {
  const nav = $('tabs');
  const mm = modulesMap(state.status);
  const tabs = [{ id: 'home', label: 'Home' }];
  for (const name of Object.keys(mm).sort()) {
    const insts = mm[name].instances;
    const bad = insts.some(i => i.status !== 'ok') || mm[name].detect !== 'ok';
    tabs.push({ id: 'mod:' + name, label: name, dot: insts.length ? (bad ? 'bad' : 'ok') : '' });
  }
  tabs.push({ id: 'curve', label: 'Curves' });
  tabs.push({ id: 'series', label: 'Time-series' });
  tabs.push({ id: 'health', label: 'Health' });

  // valid active tab?
  if (!tabs.some(t => t.id === state.tab)) state.tab = 'home';

  nav.textContent = '';
  for (const t of tabs) {
    const b = el('button', { class: 'tab' + (t.id === state.tab ? ' active' : ''), role: 'tab' });
    if (t.dot !== undefined) b.append(el('span', { class: 'dot ' + (t.dot || '') }));
    b.append(t.label);
    b.onclick = () => { state.tab = t.id; buildTabs(); render(); };
    nav.append(b);
  }
}

/* ---------- router ---------- */
function render() {
  const view = $('view'); view.textContent = '';
  const t = state.tab;
  if (t === 'home') view.append(viewHome());
  else if (t === 'curve') view.append(viewCurves());
  else if (t === 'series') view.append(viewSeries());
  else if (t === 'health') view.append(viewHealth());
  else if (t.startsWith('mod:')) view.append(viewModule(t.slice(4)));
}

/* ---------- HOME ---------- */
function viewHome() {
  const s = state.status, frag = document.createDocumentFragment();
  const instances = s.instances || [];

  // KPI strip
  let maxTemp = null, maxDuty = null, up = 0;
  for (const i of instances) {
    const a = aggregate(i.readings);
    if (a.temp != null) maxTemp = maxTemp == null ? a.temp : Math.max(maxTemp, a.temp);
    if (a.duty != null) maxDuty = maxDuty == null ? a.duty : Math.max(maxDuty, a.duty);
    if (i.status === 'ok') up++;
  }
  const kpiPanel = el('div', { class: 'panel' });
  kpiPanel.append(el('div', { class: 'kpi-strip' }, [
    kpi(Math.round(state.pressure * 100), '%', 'fleet pressure', pressureColor(state.pressure)),
    kpi(fmt(maxTemp), '°', 'hottest'),
    kpi(fmt(maxDuty), '%', 'peak duty'),
    kpi(up + '/' + instances.length, '', 'instances up'),
    kpi(s.tick ?? '–', '', 'tick'),
  ]));
  frag.append(el('h2', { class: 'section-title', text: 'The Fleet' }), kpiPanel);

  // per-module summary cards
  const mm = modulesMap(s);
  const cards = el('div', { class: 'grid cards' });
  for (const name of Object.keys(mm).sort()) {
    cards.append(moduleSummaryCard(name, mm[name]));
  }
  if (!Object.keys(mm).length) cards.append(el('div', { class: 'empty', text: 'No modules detected yet — the winds are still gathering.' }));
  frag.append(el('h2', { class: 'section-title', text: 'The Anemoi' }), cards);
  return frag;
}

function kpi(v, unit, k, color) {
  const card = el('div', { class: 'kpi' });
  const val = el('div', { class: 'v' }, [String(v), unit ? el('small', { text: unit }) : null]);
  if (color) val.style.color = color;
  card.append(val, el('div', { class: 'k', text: k }));
  return card;
}

function moduleSummaryCard(name, mod) {
  const card = el('div', { class: 'panel' });
  const insts = mod.instances;
  const bad = mod.detect !== 'ok' || insts.some(i => i.status !== 'ok');
  card.append(el('div', { class: 'card-head' }, [
    el('div', {}, [el('span', { class: 'name', text: name })]),
    el('span', { class: 'badge ' + (bad ? 'bad' : 'ok'), text: mod.detect === 'ok' ? (bad ? 'attention' : 'ok') : (mod.detect || '?') }),
  ]));
  // aggregate across instances
  let mt = null, md = null, mr = null;
  for (const i of insts) { const a = aggregate(i.readings); if (a.temp != null) mt = Math.max(mt ?? -1e9, a.temp); if (a.duty != null) md = Math.max(md ?? -1e9, a.duty); if (a.rpm != null) mr = Math.max(mr ?? -1e9, a.rpm); }
  card.append(el('div', { class: 'stats' }, [
    stat(fmt(mt), '°C', 'temp', tempClass(mt)),
    stat(fmt(md), '%', 'duty'),
    mr != null ? stat(fmt(mr), 'rpm', 'fan') : null,
    stat(String(insts.length), '', insts.length === 1 ? 'unit' : 'units'),
  ]));
  // duty bar
  if (md != null) { const bar = el('div', { class: 'bar' }); const fill = el('span'); fill.style.width = clamp(md, 0, 100) + '%'; bar.append(fill); card.append(bar); }
  card.onclick = () => { state.tab = 'mod:' + name; buildTabs(); render(); };
  card.style.cursor = 'pointer';
  return card;
}

function stat(v, unit, k, cls) {
  const s = el('div', { class: 'stat' + (cls ? ' ' + cls : '') });
  s.append(el('div', { class: 'v' }, [String(v), unit ? el('small', { text: ' ' + unit }) : null]), el('div', { class: 'k', text: k }));
  return s;
}
function tempClass(t) { if (t == null) return ''; if (t >= 75) return 'hot'; if (t >= 60) return 'warm'; return 'cool'; }

/* ---------- per-MODULE view ---------- */
function viewModule(name) {
  const frag = document.createDocumentFragment();
  const mm = modulesMap(state.status);
  const mod = mm[name];
  if (!mod) { frag.append(el('div', { class: 'empty', text: 'Module not present.' })); return frag; }
  frag.append(el('h2', { class: 'section-title', text: name }));
  if (mod.detect_error) frag.append(el('div', { class: 'panel', html: '<b>detect:</b> ' + escapeHtml(mod.detect_error) }));
  const grid = el('div', { class: 'grid cards' });
  for (const i of mod.instances) grid.append(instanceCard(i));
  if (!mod.instances.length) grid.append(el('div', { class: 'empty', text: 'No instances reported.' }));
  frag.append(grid);
  return frag;
}

function instanceCard(i) {
  const card = el('div', { class: 'panel' });
  const a = aggregate(i.readings);
  const ok = i.status === 'ok';
  card.append(el('div', { class: 'card-head' }, [
    el('div', {}, [el('span', { class: 'name', text: i.name || i.id }), el('div', { class: 'id', text: i.id })]),
    el('span', { class: 'badge ' + (ok ? 'ok' : 'bad'), text: i.status }),
  ]));
  card.append(el('div', { class: 'stats' }, [
    stat(fmt(a.temp), '°C', 'temp', tempClass(a.temp)),
    stat(fmt(a.duty), '%', 'duty'),
    a.rpm != null ? stat(fmt(a.rpm), 'rpm', 'fan') : null,
    i.restart_count ? stat(String(i.restart_count), '', 'restarts') : null,
  ]));
  if (a.duty != null) { const bar = el('div', { class: 'bar' }); const f = el('span'); f.style.width = clamp(a.duty, 0, 100) + '%'; bar.append(f); card.append(bar); }
  // sparkline of this instance's temp from history
  card.append(sparkline(i.module + ':' + i.id));
  // readings detail
  card.append(readingsList(i.readings));
  return card;
}

function readingsList(readings) {
  const box = el('div', { class: 'readings' });
  for (const r of readings || []) {
    const fields = Object.keys(r).filter(k => k !== 'type' && k !== 'label')
      .map(k => `<span class="k">${escapeHtml(k)}</span>=${escapeHtml(String(r[k]))}`).join('  ');
    box.append(el('div', { html: `<span class="lbl">${escapeHtml(r.type)}·${escapeHtml(r.label)}</span> ${fields}` }));
  }
  return box;
}

/* sparkline for a given instance key using the history ring buffer */
function sparkline(key) {
  const snaps = (state.history && state.history.snaps) || [];
  const pts = [];
  for (const s of snaps) {
    const hi = (s.instances || []).find(x => x.key === key);
    if (hi && hi.temp != null) pts.push(hi.temp);
  }
  const s = svg('svg', { class: 'spark', viewBox: '0 0 200 46', preserveAspectRatio: 'none' });
  if (pts.length < 2) { s.append(svg('line', { x1: 0, y1: 44, x2: 200, y2: 44, class: 'axis' })); return s; }
  const recent = pts.slice(-SPARK_POINTS);
  const min = Math.min(...recent), max = Math.max(...recent), span = (max - min) || 1;
  const X = (k) => (k / (recent.length - 1)) * 200;
  const Y = (v) => 42 - ((v - min) / span) * 38;
  let d = '', area = '';
  recent.forEach((v, k) => { const x = X(k), y = Y(v); d += (k ? 'L' : 'M') + x.toFixed(1) + ' ' + y.toFixed(1) + ' '; });
  area = 'M0 44 ' + d.replace('M', 'L') + `L200 44 Z`;
  s.append(svg('path', { d: area, fill: 'var(--aether)', opacity: '0.12' }));
  s.append(svg('path', { d, fill: 'none', stroke: 'var(--aether)', 'stroke-width': '1.6' }));
  return s;
}

/* ---------- CURVE view ---------- */
function viewCurves() {
  const frag = document.createDocumentFragment();
  frag.append(el('h2', { class: 'section-title', text: 'Temperature → Duty Curves' }));
  const mm = modulesMap(state.status);
  const grid = el('div', { class: 'grid cols-2' });
  let any = false;
  for (const name of Object.keys(mm).sort()) {
    // operating point: prefer a driving reading from any instance of this module
    const op = moduleOperatingPoint(mm[name].instances);
    const panel = el('div', { class: 'panel' });
    panel.append(el('h3', { text: name }));
    const holder = el('div'); panel.append(holder);
    grid.append(panel);
    any = true;
    fetchCurve(name).then(c => renderCurve(holder, name, c, op));
  }
  if (!any) grid.append(el('div', { class: 'empty', text: 'No modules to chart.' }));
  frag.append(grid);
  return frag;
}

function moduleOperatingPoint(insts) {
  // returns {temp, pct} from a driving reading, else from aggregate temp + duty
  for (const i of insts) {
    for (const r of (i.readings || [])) if (r.type === 'driving' && r.temp != null) return { temp: r.raw != null ? r.raw : r.temp, pct: r.pct };
  }
  let temp = null, pct = null;
  for (const i of insts) { const a = aggregate(i.readings); if (a.temp != null) temp = Math.max(temp ?? -1e9, a.temp); if (a.duty != null) pct = Math.max(pct ?? -1e9, a.duty); }
  return { temp, pct };
}

async function fetchCurve(name) {
  if (state.curveCache[name] && Date.now() - state.curveCache[name]._t < 15000) return state.curveCache[name];
  try {
    const c = await fetch('/curve.json?module=' + encodeURIComponent(name), { cache: 'no-store' }).then(r => r.json());
    c._t = Date.now(); state.curveCache[name] = c; return c;
  } catch (e) { return { available: false, points: [] }; }
}

function renderCurve(holder, name, c, op) {
  holder.textContent = '';
  const W = 480, H = 280, pad = 34;
  const s = svg('svg', { class: 'curve-svg', viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'xMidYMid meet' });
  const defs = svg('defs');
  const g = svg('linearGradient', { id: 'curvegrad', x1: '0', y1: '0', x2: '0', y2: '1' });
  g.append(svg('stop', { offset: '0', 'stop-color': 'var(--accent)' }));
  g.append(svg('stop', { offset: '1', 'stop-color': 'var(--accent)', 'stop-opacity': '0' }));
  defs.append(g); s.append(defs);

  const tMin = 20, tMax = 100;
  const X = (t) => pad + (clamp(t, tMin, tMax) - tMin) / (tMax - tMin) * (W - 2 * pad);
  const Y = (p) => (H - pad) - clamp(p, 0, 100) / 100 * (H - 2 * pad);

  // grid + axes
  for (let p = 0; p <= 100; p += 25) {
    s.append(svg('line', { x1: pad, y1: Y(p), x2: W - pad, y2: Y(p), class: 'axis' }));
    const ty = svg('text', { x: pad - 6, y: Y(p) + 3, class: 'axis-txt', 'text-anchor': 'end' });
    ty.textContent = p + '%';
    s.append(ty);
  }
  for (let t = tMin; t <= tMax; t += 20) {
    const tx = svg('text', { x: X(t), y: H - pad + 14, class: 'axis-txt', 'text-anchor': 'middle' }); tx.textContent = t + '°';
    s.append(tx);
  }

  if (c && c.available && c.points && c.points.length) {
    // build the interpolated/clamped curve across the whole axis
    const pts = c.points.slice().sort((a, b) => a[0] - b[0]);
    const evalCurve = (t) => {
      if (t <= pts[0][0]) return pts[0][1];
      if (t >= pts[pts.length - 1][0]) return pts[pts.length - 1][1];
      for (let k = 1; k < pts.length; k++) if (t <= pts[k][0]) {
        const [t0, p0] = pts[k - 1], [t1, p1] = pts[k];
        return p0 + (t - t0) / (t1 - t0) * (p1 - p0);
      }
      return pts[pts.length - 1][1];
    };
    let d = '';
    for (let t = tMin; t <= tMax; t += 1) { d += (t === tMin ? 'M' : 'L') + X(t).toFixed(1) + ' ' + Y(evalCurve(t)).toFixed(1) + ' '; }
    s.append(svg('path', { d: d + `L${X(tMax)} ${Y(0)} L${X(tMin)} ${Y(0)} Z`, class: 'curve-fill' }));
    s.append(svg('path', { d, class: 'curve-line' }));
    // curve control points
    for (const [t, p] of pts) s.append(svg('circle', { cx: X(t), cy: Y(p), r: 3, fill: 'var(--accent)' }));
  }

  // live operating point
  if (op && op.temp != null) {
    const px = X(op.temp), py = Y(op.pct != null ? op.pct : 0);
    if (op.pct != null) {
      s.append(svg('line', { x1: px, y1: Y(0), x2: px, y2: py, stroke: 'var(--aether)', 'stroke-width': '1', 'stroke-dasharray': '3 3', opacity: '0.5' }));
      const ring = svg('circle', { cx: px, cy: py, r: 9, class: 'curve-op-ring' });
      ring.append(svg('animate', { attributeName: 'r', values: '7;12;7', dur: '2.4s', repeatCount: 'indefinite' }));
      ring.append(svg('animate', { attributeName: 'opacity', values: '0.7;0.1;0.7', dur: '2.4s', repeatCount: 'indefinite' }));
      s.append(ring);
      s.append(svg('circle', { cx: px, cy: py, r: 5, class: 'curve-op' }));
    }
  }
  holder.append(s);

  // note + legend
  let note;
  if (!c || !c.available) note = 'Curve config not readable here; showing the live operating point only.';
  else if (op && op.temp != null && op.pct != null) note = `Operating at ${fmt(op.temp)}°C → ${fmt(op.pct)}% duty` + (c.sensitivity != null ? ` · sensitivity α=${c.sensitivity}` : '');
  else note = 'Curve loaded; awaiting a live operating point.';
  holder.append(el('div', { class: 'curve-note', text: note }));
}

/* ---------- TIME-SERIES view ---------- */
function viewSeries() {
  const frag = document.createDocumentFragment();
  frag.append(el('h2', { class: 'section-title', text: 'Time-series' }));
  const snaps = (state.history && state.history.snaps) || [];
  if (snaps.length < 2) {
    frag.append(el('div', { class: 'panel empty', text: 'Gathering history… time-series appear after a few snapshots (every 5s).' }));
    return frag;
  }
  // keys present in the latest snapshot
  const last = snaps[snaps.length - 1];
  const keys = (last.instances || []).map(x => x.key);
  const grid = el('div', { class: 'grid cols-2' });
  grid.append(seriesPanel('Temperatures', snaps, keys, 'temp', '°C', [20, 100]));
  grid.append(seriesPanel('Fan duty', snaps, keys, 'duty', '%', [0, 100]));
  grid.append(seriesPanel('Fan RPM', snaps, keys, 'rpm', 'rpm', null));
  frag.append(grid);
  return frag;
}

const PALETTE = ['#e8c466', '#5fd3e0', '#62d6a0', '#f06a6a', '#b98e2e', '#3a93c9', '#c98ad6', '#f0b341', '#7fd1a4', '#7aa5e0'];

function seriesPanel(title, snaps, keys, field, unit, fixedRange) {
  const panel = el('div', { class: 'panel' });
  panel.append(el('h3', { text: title }));
  const W = 520, H = 220, padL = 36, padR = 10, padT = 10, padB = 20;
  const s = svg('svg', { class: 'chart', viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'none' });

  // collect series
  const series = [];
  keys.forEach((key, idx) => {
    const ys = [];
    for (const sn of snaps) { const hi = (sn.instances || []).find(x => x.key === key); ys.push(hi ? (hi[field] ?? null) : null); }
    if (ys.some(v => v != null)) series.push({ key, ys, color: PALETTE[idx % PALETTE.length] });
  });
  if (!series.length) { panel.append(el('div', { class: 'empty', text: 'No data for this metric.' })); return panel; }

  // y-range
  let lo = Infinity, hi = -Infinity;
  for (const se of series) for (const v of se.ys) if (v != null) { lo = Math.min(lo, v); hi = Math.max(hi, v); }
  if (fixedRange) { lo = Math.min(lo, fixedRange[0]); hi = Math.max(hi, fixedRange[1]); }
  if (!isFinite(lo)) { lo = 0; hi = 1; }
  if (hi - lo < 1) { hi += 1; lo -= 1; }
  const t0 = snaps[0].t, t1 = snaps[snaps.length - 1].t, tspan = (t1 - t0) || 1;
  const X = (t) => padL + (t - t0) / tspan * (W - padL - padR);
  const Y = (v) => (H - padB) - (v - lo) / (hi - lo) * (H - padT - padB);

  // gridlines + y labels
  for (let g = 0; g <= 4; g++) {
    const v = lo + (hi - lo) * g / 4, y = Y(v);
    s.append(svg('line', { x1: padL, y1: y, x2: W - padR, y2: y, class: 'axis' }));
    const tx = svg('text', { x: padL - 5, y: y + 3, class: 'axis-txt', 'text-anchor': 'end' }); tx.textContent = Math.round(v);
    s.append(tx);
  }
  // x labels (start / end relative seconds)
  const ageS = Math.round((t1 - t0) / 1000);
  const txl = svg('text', { x: padL, y: H - 5, class: 'axis-txt', 'text-anchor': 'start' }); txl.textContent = '-' + ageS + 's';
  const txr = svg('text', { x: W - padR, y: H - 5, class: 'axis-txt', 'text-anchor': 'end' }); txr.textContent = 'now';
  s.append(txl, txr);

  // lines (gaps where null)
  for (const se of series) {
    let d = '', pen = false;
    se.ys.forEach((v, k) => {
      if (v == null) { pen = false; return; }
      const x = X(snaps[k].t), y = Y(v);
      d += (pen ? 'L' : 'M') + x.toFixed(1) + ' ' + y.toFixed(1) + ' '; pen = true;
    });
    s.append(svg('path', { d, fill: 'none', stroke: se.color, 'stroke-width': '1.8', 'stroke-linejoin': 'round', 'stroke-linecap': 'round' }));
  }
  panel.append(s);
  // legend
  const legend = el('div', { class: 'legend' });
  for (const se of series) legend.append(el('span', { html: `<i style="background:${se.color}"></i>${escapeHtml(se.key)}${unit && unit !== 'rpm' ? '' : ''}` }));
  panel.append(legend);
  return panel;
}

/* ---------- HEALTH view ---------- */
function viewHealth() {
  const frag = document.createDocumentFragment();
  frag.append(el('h2', { class: 'section-title', text: 'Health & Logs' }));
  const s = state.status;

  // modules table
  const mp = el('div', { class: 'panel' });
  mp.append(el('h3', { text: 'Modules (detect)' }));
  const mt = el('table', { class: 'health' });
  mt.append(el('tr', {}, [el('th', { text: 'module' }), el('th', { text: 'detect' }), el('th', { text: 'error' })]));
  for (const m of (s.modules || [])) {
    const ok = m.detect_status === 'ok';
    mt.append(el('tr', { class: ok ? 'ok' : 'bad' }, [
      el('td', { text: m.module }),
      el('td', {}, [el('span', { class: 'status-pill ' + (ok ? 'ok' : 'bad'), text: m.detect_status })]),
      el('td', { class: 'mono', text: m.detect_error || '' }),
    ]));
  }
  if (!(s.modules || []).length) mt.append(el('tr', {}, [el('td', { colspan: '3', html: '<span class="empty">—</span>' })]));
  mp.append(mt); frag.append(mp);

  // instances table with stderr tail
  const ip = el('div', { class: 'panel' });
  ip.append(el('h3', { text: 'Instances' }));
  const it = el('table', { class: 'health' });
  it.append(el('tr', {}, ['module', 'id', 'name', 'status', 'restarts', 'age', 'stderr tail'].map(h => el('th', { text: h }))));
  for (const i of (s.instances || [])) {
    const ok = i.status === 'ok';
    it.append(el('tr', { class: ok ? 'ok' : 'bad' }, [
      el('td', { text: i.module }),
      el('td', { class: 'mono', text: i.id }),
      el('td', { text: i.name }),
      el('td', {}, [el('span', { class: 'status-pill ' + (ok ? 'ok' : 'bad'), text: i.status })]),
      el('td', { text: String(i.restart_count) }),
      el('td', { text: String(i.ticks_since_seen) }),
      el('td', {}, [el('div', { class: 'tail', text: (i.stderr_tail || []).join('\n') })]),
    ]));
  }
  if (!(s.instances || []).length) it.append(el('tr', {}, [el('td', { colspan: '7', html: '<span class="empty">no instances (detecting…)</span>' })]));
  ip.append(it); frag.append(ip);
  return frag;
}

/* ---------- animated wind backdrop ---------- */
const Wind = (() => {
  let svgEl, lines = [], raf, t = 0, w = 0, h = 0;
  const N = 22;
  function init() {
    svgEl = $('wind'); resize();
    for (let k = 0; k < N; k++) {
      const p = svg('path', { class: 'wind-path' });
      svgEl.append(p);
      lines.push({ el: p, y: Math.random(), phase: Math.random() * Math.PI * 2, amp: 0.4 + Math.random() * 0.8, speed: 0.5 + Math.random() });
    }
    window.addEventListener('resize', resize);
    loop();
  }
  function resize() { w = window.innerWidth; h = window.innerHeight; svgEl.setAttribute('viewBox', `0 0 ${w} ${h}`); }
  function loop() {
    const p = state.pressure || 0;
    // pressure raises flow speed, stroke weight, amplitude and opacity
    t += 0.006 + p * 0.03;
    const baseW = 0.8 + p * 2.2;
    const op = 0.18 + p * 0.5;
    svgEl.style.opacity = (0.5 + p * 0.5).toFixed(2);
    lines.forEach((ln, k) => {
      const yy = ln.y * h;
      const amp = (10 + p * 60) * ln.amp;
      const segs = 8; let d = '';
      for (let i = 0; i <= segs; i++) {
        const x = (i / segs) * w;
        const yv = yy + Math.sin(t * ln.speed + ln.phase + i * 0.6) * amp
                      + Math.sin(t * 0.5 * ln.speed + i * 0.3) * amp * 0.4;
        d += (i ? 'L' : 'M') + x.toFixed(1) + ' ' + yv.toFixed(1) + ' ';
      }
      ln.el.setAttribute('d', d);
      ln.el.setAttribute('stroke-width', (baseW * (0.5 + ln.amp)).toFixed(2));
      ln.el.setAttribute('stroke-opacity', (op * (0.4 + ln.amp * 0.6)).toFixed(2));
      // tint hotter flows toward the accent/danger color
      ln.el.style.stroke = p > 0.6 ? 'var(--accent)' : 'var(--wind-stroke)';
    });
    raf = requestAnimationFrame(loop);
  }
  return { init };
})();

/* ---------- theme ---------- */
function initTheme() {
  const saved = localStorage.getItem('aiolos-theme') || 'dark';
  document.documentElement.setAttribute('data-theme', saved);
  setThemeIcon(saved);
  $('theme').onclick = () => {
    const cur = document.documentElement.getAttribute('data-theme');
    const next = cur === 'dark' ? 'light' : 'dark';
    document.documentElement.setAttribute('data-theme', next);
    localStorage.setItem('aiolos-theme', next);
    setThemeIcon(next);
  };
}
function setThemeIcon(t) { $('theme').textContent = t === 'dark' ? '☀' : '☾'; }

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

/* ---------- boot ---------- */
window.addEventListener('DOMContentLoaded', () => {
  initTheme();
  if (!window.matchMedia || !window.matchMedia('(prefers-reduced-motion: reduce)').matches) Wind.init();
  else { $('wind').style.display = 'none'; }
  poll();
});
