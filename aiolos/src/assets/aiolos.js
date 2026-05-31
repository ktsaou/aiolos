/* Aiolos dashboard — vanilla JS, no frameworks, no external network.
   Polls /status.json + /history.json; builds tabs dynamically from the live module/instance set;
   renders device-centric home, per-module, curve, time-series and health views; drives an animated
   wind backdrop whose speed reflects live "system pressure" (max normalised temp/duty). */
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
  series: { rangeMs: 15 * 60 * 1000, endT: null, hidden: {} },
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

/* ---------- component aggregation (mirrors the server's headline series) ---------- */
function pnum(p) {
  if (!p || p.value == null) return null;
  const n = Number(p.value);
  return Number.isFinite(n) ? n : null;
}
function snum(s) {
  if (!s || s.value == null) return null;
  const n = Number(s.value);
  return Number.isFinite(n) ? n : null;
}
function aggregate(components) {
  let maxTemp = null, drivingTemp = null, drivingPct = null, maxPwm = null, maxRpm = null;
  for (const c of components || []) {
    for (const p of (c.publishers || [])) {
      const v = pnum(p);
      if (v == null) continue;
      if (p.kind === 'temperature') maxTemp = maxTemp == null ? v : Math.max(maxTemp, v);
      else if (p.kind === 'driving-temperature') drivingTemp = v;
      else if (p.kind === 'driving-duty') drivingPct = v;
      else if (p.kind === 'fan-duty') maxPwm = maxPwm == null ? v : Math.max(maxPwm, v);
      else if (p.kind === 'fan-rpm') maxRpm = maxRpm == null ? v : Math.max(maxRpm, v);
    }
    for (const k of (c.sinks || [])) {
      const v = snum(k);
      if (v != null && k.kind === 'fan-duty') maxPwm = maxPwm == null ? v : Math.max(maxPwm, v);
    }
  }
  return { temp: drivingTemp != null ? drivingTemp : maxTemp, duty: drivingPct != null ? drivingPct : maxPwm, rpm: maxRpm };
}

function primaryValue(c) {
  const agg = aggregate([c]);
  if (agg.temp != null) return { value: fmt(agg.temp), unit: '°C', cls: tempClass(agg.temp), label: 'temp' };
  if (agg.duty != null) return { value: fmt(agg.duty), unit: '%', cls: '', label: 'duty' };
  if (agg.rpm != null) return { value: fmt(agg.rpm), unit: 'rpm', cls: '', label: 'fan' };
  const p = (c.publishers || []).find(x => x.value != null);
  return p ? { value: String(p.value), unit: p.unit || '', cls: '', label: p.label || p.kind } : { value: '–', unit: '', cls: '', label: 'idle' };
}

function classTitle(cls) {
  const names = { gpu: 'GPUs', cpu: 'CPUs', ssd: 'SSDs', board: 'Boards', power: 'Power', nic: 'NICs', fan: 'Fans', mock: 'Mock' };
  return names[cls] || (cls ? cls[0].toUpperCase() + cls.slice(1) : 'Devices');
}

function classGlyph(cls) {
  const icons = { gpu: '▣', cpu: '◈', ssd: '▭', board: '✣', power: '⚡', nic: '⇄', fan: '✺', mock: '◇' };
  return icons[cls] || '◌';
}

function classIcon(cls, component) {
  const s = svg('svg', { class: 'class-icon-svg ' + (cls || 'device'), viewBox: '0 0 24 24', 'aria-hidden': 'true' });
  const add = (tag, attrs) => s.append(svg(tag, attrs));
  if (cls === 'power') {
    add('rect', { x: 4, y: 7, width: 14, height: 10, rx: 2 });
    add('path', { d: 'M18 10h2v4h-2M8 12h6' });
  } else if (cls === 'ssd') {
    add('rect', { x: 4, y: 5, width: 16, height: 14, rx: 2 });
    add('path', { d: 'M8 9h8M8 13h5M7 17h1M11 17h1M15 17h1' });
  } else if (cls === 'nic') {
    add('path', { d: 'M4 8h16v8H4zM8 8V5h8v3M8 16v3M16 16v3M9 12h6' });
  } else if (cls === 'gpu') {
    add('rect', { x: 3, y: 7, width: 16, height: 10, rx: 2 });
    add('path', { d: 'M19 10h2v4h-2M7 10h5v4H7zM8 4v3M14 4v3' });
  } else if (cls === 'cpu') {
    add('rect', { x: 7, y: 7, width: 10, height: 10, rx: 1.5 });
    add('path', { d: 'M4 8h3M4 12h3M4 16h3M17 8h3M17 12h3M17 16h3M8 4v3M12 4v3M16 4v3M8 17v3M12 17v3M16 17v3' });
  } else {
    add('circle', { cx: 12, cy: 12, r: 8 });
    add('path', { d: 'M12 6v12M6 12h12M8.5 8.5l7 7M15.5 8.5l-7 7' });
  }
  const rpm = aggregate(component ? [component] : []).rpm;
  if (rpm != null && rpm > 0) {
    s.classList.add('spinning');
    s.style.animationDuration = clamp(120 / rpm, 0.08, 1.2).toFixed(2) + 's';
  }
  return s;
}

function flattenDevices(status) {
  const out = [];
  for (const i of (status.instances || [])) {
    for (const c of (i.components || [])) out.push({ inst: i, component: c, key: i.module + ':' + i.id + '/' + c.id });
  }
  return out;
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
  for (const i of (s.instances || [])) { p = Math.max(p, instPressure(aggregate(i.components))); count++; }
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
    const a = aggregate(i.components);
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

  // device-centric home: group by component class, not by anemos name.
  const devices = flattenDevices(s);
  frag.append(el('h2', { class: 'section-title', text: 'Devices' }), deviceGroups(devices));

  // Keep module health visible below the device view.
  const mm = modulesMap(s);
  const cards = el('div', { class: 'grid cards' });
  for (const name of Object.keys(mm).sort()) cards.append(moduleSummaryCard(name, mm[name]));
  if (!Object.keys(mm).length) cards.append(el('div', { class: 'empty', text: 'No modules detected yet — the winds are still gathering.' }));
  frag.append(el('h2', { class: 'section-title', text: 'The Anemoi' }), cards);
  return frag;
}

function deviceGroups(devices) {
  if (!devices.length) return el('div', { class: 'empty', text: 'No components reported yet — waiting for the first tick.' });
  const by = {};
  for (const d of devices) (by[d.component.class || 'device'] ||= []).push(d);
  const wrap = el('div', { class: 'device-groups' });
  for (const cls of Object.keys(by).sort()) {
    const group = el('section', { class: 'device-group' });
    group.append(el('h3', { class: 'device-group-title' }, [
      classIcon(cls),
      classTitle(cls),
    ]));
    const grid = el('div', { class: 'grid cards' });
    for (const d of by[cls].sort((a, b) => a.component.label.localeCompare(b.component.label))) grid.append(deviceCard(d));
    group.append(grid);
    wrap.append(group);
  }
  return wrap;
}

function deviceCard(d) {
  const { inst: i, component: c } = d;
  const p = primaryValue(c);
  const a = aggregate([c]);
  const ok = i.status === 'ok';
  const card = el('div', { class: 'panel device-card' });
  card.append(el('div', { class: 'card-head' }, [
    el('div', {}, [
      el('span', { class: 'name device-name' }, [classIcon(c.class, c), el('span', { text: c.label || c.id })]),
      el('div', { class: 'id', text: `${i.module}:${i.id} · ${c.id}` }),
    ]),
    el('span', { class: 'badge ' + (ok ? 'ok' : 'bad'), text: i.status }),
  ]));
  card.append(el('div', { class: 'stats' }, [
    stat(p.value, p.unit, p.label, p.cls),
    a.duty != null ? stat(fmt(a.duty), '%', 'duty') : null,
    a.rpm != null ? stat(fmt(a.rpm), 'rpm', 'fan') : null,
    stat(String((c.publishers || []).length), '', 'signals'),
  ]));
  if (a.duty != null) { const bar = el('div', { class: 'bar' }); const f = el('span'); f.style.width = clamp(a.duty, 0, 100) + '%'; bar.append(f); card.append(bar); }
  card.append(componentDetail(c));
  return card;
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
  for (const i of insts) { const a = aggregate(i.components); if (a.temp != null) mt = Math.max(mt ?? -1e9, a.temp); if (a.duty != null) md = Math.max(md ?? -1e9, a.duty); if (a.rpm != null) mr = Math.max(mr ?? -1e9, a.rpm); }
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
  const a = aggregate(i.components);
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
  // component detail
  card.append(componentsList(i.components));
  return card;
}

function componentsList(components) {
  const box = el('div', { class: 'components-report' });
  for (const c of components || []) box.append(componentDetail(c));
  return box;
}

function componentDetail(c) {
  const box = el('div', { class: 'component-detail' });
  box.append(el('div', { class: 'component-title', text: `${classGlyph(c.class)} ${c.label || c.id} · ${c.class || 'device'}` }));
  for (const p of (c.publishers || [])) {
    if (p.value == null) continue;
    const unit = p.unit ? ` ${escapeHtml(p.unit)}` : '';
    box.append(el('div', { html: `<span class="lbl">${escapeHtml(p.label || p.id)}</span> <span class="k">${escapeHtml(p.kind)}</span>=${escapeHtml(String(p.value))}${unit}` }));
  }
  for (const k of (c.sinks || [])) {
    const val = k.value != null ? `${escapeHtml(String(k.value))}${k.unit ? ' ' + escapeHtml(k.unit) : ''}` : '–';
    const by = (k.driven_by || []).map(d => {
      const v = d.value != null ? ` ${escapeHtml(String(d.value))}${d.unit ? escapeHtml(d.unit) : ''}` : '';
      return `${escapeHtml(d.from)}${d.publisher ? '/' + escapeHtml(d.publisher) : ''}${v}`;
    }).join(', ');
    box.append(el('div', { html: `<span class="lbl">sink ${escapeHtml(k.label || k.id)}</span> <span class="k">${escapeHtml(k.state || 'unknown')}</span>→${val}${by ? ` · driven by ${by}` : ''}` }));
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
  // returns {temp, pct} from driving publishers, else from aggregate temp + duty
  for (const i of insts) {
    for (const c of (i.components || [])) {
      let raw = null, temp = null, pct = null;
      for (const p of (c.publishers || [])) {
        if (p.kind === 'driving-raw-temperature') raw = pnum(p);
        else if (p.kind === 'driving-temperature') temp = pnum(p);
        else if (p.kind === 'driving-duty') pct = pnum(p);
      }
      if (temp != null || raw != null) return { temp: raw != null ? raw : temp, pct };
    }
  }
  let temp = null, pct = null;
  for (const i of insts) {
    const a = aggregate(i.components);
    if (a.temp != null) temp = Math.max(temp ?? -1e9, a.temp);
    if (a.duty != null) pct = Math.max(pct ?? -1e9, a.duty);
  }
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
const SERIES_FIELDS = [
  { field: 'temp', label: 'Temperature', unit: '°C', axis: 'left', fixed: [20, 100], dash: '' },
  { field: 'duty', label: 'Fan duty', unit: '%', axis: 'right', fixed: [0, 100], dash: '4 3' },
  { field: 'rpm', label: 'Fan RPM', unit: 'rpm', axis: 'far', fixed: null, dash: '1 3' },
];

function viewSeries() {
  const frag = document.createDocumentFragment();
  frag.append(el('h2', { class: 'section-title', text: 'Time-series' }));
  const snaps = (state.history && state.history.snaps) || [];
  if (snaps.length < 2) {
    frag.append(el('div', { class: 'panel empty', text: 'Gathering history… time-series appear after a few snapshots (every 5s).' }));
    return frag;
  }
  frag.append(multiAxisSeriesPanel(snaps));
  return frag;
}

const PALETTE = ['#e8c466', '#5fd3e0', '#62d6a0', '#f06a6a', '#b98e2e', '#3a93c9', '#c98ad6', '#f0b341', '#7fd1a4', '#7aa5e0'];
const FIELD_COLOR = { temp: 'var(--aether)', duty: 'var(--accent)', rpm: 'var(--good)' };

function multiAxisSeriesPanel(snaps) {
  const panel = el('div', { class: 'panel series-panel' });
  panel.append(el('div', { class: 'chart-head' }, [
    el('div', {}, [
      el('h3', { text: 'Combined fleet telemetry' }),
      el('div', { class: 'chart-sub', text: 'One chart, three y-axes: °C (left), % (right), RPM (far right). Hover for values; wheel to zoom.' }),
    ]),
    seriesControls(snaps),
  ]));

  const visible = windowedSnaps(snaps);
  const series = collectSeries(visible);
  if (!series.length) {
    panel.append(el('div', { class: 'empty', text: 'No numeric telemetry in the selected range.' }));
    return panel;
  }

  const active = series.filter(se => !state.series.hidden[se.id]);
  const W = 900, H = 360, padL = 58, padR = 112, padT = 20, padB = 36;
  const plotL = padL, plotR = W - padR, plotT = padT, plotB = H - padB;
  const plotW = plotR - plotL, plotH = plotB - plotT;
  const chart = el('div', { class: 'chart-wrap' });
  const tip = el('div', { class: 'chart-tip hidden' });
  const s = svg('svg', { class: 'chart multi-axis-chart', viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'none' });
  const t0 = visible[0].t, t1 = visible[visible.length - 1].t, tspan = (t1 - t0) || 1;
  const X = (t) => plotL + (t - t0) / tspan * plotW;
  const domain = axisDomains(active);
  const Y = (field, v) => {
    const d = domain[field] || [0, 1];
    return plotB - (v - d[0]) / (d[1] - d[0]) * plotH;
  };

  s.append(svg('rect', { x: plotL, y: plotT, width: plotW, height: plotH, class: 'plot-bg' }));
  drawGridAndAxes(s, domain, {
    W, H, plotL, plotR, plotT, plotB, plotW, plotH,
    rangeMs: t1 - t0,
    liveEdge: state.series.endT == null || t1 >= snaps[snaps.length - 1].t,
  });

  for (const se of active) {
    let d = '', pen = false;
    se.ys.forEach((v, k) => {
      if (v == null) { pen = false; return; }
      const x = X(visible[k].t), y = Y(se.field, v);
      d += (pen ? 'L' : 'M') + x.toFixed(1) + ' ' + y.toFixed(1) + ' ';
      pen = true;
    });
    if (d) s.append(svg('path', {
      d, class: 'series-line', fill: 'none', stroke: se.color, 'stroke-width': '1.8',
      'stroke-linejoin': 'round', 'stroke-linecap': 'round', 'stroke-dasharray': se.dash,
    }));
  }

  const hover = svg('g', { class: 'chart-hover', visibility: 'hidden' });
  const hoverLine = svg('line', { x1: plotL, y1: plotT, x2: plotL, y2: plotB, class: 'hover-line' });
  const hoverDots = svg('g', {});
  hover.append(hoverLine, hoverDots);
  s.append(hover);

  const overlay = svg('rect', { x: plotL, y: plotT, width: plotW, height: plotH, fill: 'transparent', 'pointer-events': 'all' });
  overlay.addEventListener('mousemove', ev => {
    const rect = s.getBoundingClientRect();
    const vx = (ev.clientX - rect.left) * W / rect.width;
    const idx = clamp(Math.round((vx - plotL) / plotW * (visible.length - 1)), 0, visible.length - 1);
    const snap = visible[idx];
    const x = X(snap.t);
    hover.setAttribute('visibility', 'visible');
    hoverLine.setAttribute('x1', x); hoverLine.setAttribute('x2', x);
    hoverDots.textContent = '';
    const rows = [];
    for (const se of active) {
      const v = se.ys[idx];
      if (v == null) continue;
      hoverDots.append(svg('circle', { cx: x, cy: Y(se.field, v), r: 3.5, fill: se.color, stroke: 'var(--panel-solid)', 'stroke-width': 1.5 }));
      rows.push(`<div><i style="background:${se.color}"></i>${escapeHtml(se.label)} <b>${fmt(v, se.field === 'rpm' ? 0 : 1)}${escapeHtml(se.unit)}</b></div>`);
    }
    const age = Math.round((visible[visible.length - 1].t - snap.t) / 1000);
    tip.innerHTML = `<strong>${age ? '-' + age + 's' : 'now'}</strong>${rows.join('') || '<div>No values</div>'}`;
    const prect = panel.getBoundingClientRect();
    tip.style.left = Math.min(Math.max(ev.clientX - prect.left + 14, 8), Math.max(8, prect.width - 280)) + 'px';
    tip.style.top = Math.max(ev.clientY - prect.top - 18, 8) + 'px';
    tip.classList.remove('hidden');
  });
  overlay.addEventListener('mouseleave', () => { hover.setAttribute('visibility', 'hidden'); tip.classList.add('hidden'); });
  overlay.addEventListener('wheel', ev => {
    ev.preventDefault();
    zoomSeries(snaps, ev.deltaY < 0 ? 0.8 : 1.25);
  }, { passive: false });
  s.append(overlay);

  chart.append(s, tip);
  panel.append(chart);
  panel.append(seriesLegend(series));
  return panel;
}

function seriesControls(snaps) {
  const wrap = el('div', { class: 'series-controls' });
  const ranges = [
    ['1m', 60 * 1000], ['5m', 5 * 60 * 1000], ['15m', 15 * 60 * 1000], ['all', null],
  ];
  for (const [label, ms] of ranges) {
    const active = (state.series.rangeMs == null && ms == null) || state.series.rangeMs === ms;
    const b = el('button', { class: 'mini-btn' + (active ? ' active' : ''), text: label });
    b.onclick = () => { state.series.rangeMs = ms; state.series.endT = null; render(); };
    wrap.append(b);
  }
  const left = el('button', { class: 'mini-btn', text: '←' });
  left.title = 'Pan earlier'; left.onclick = () => { panSeries(snaps, -1); };
  const right = el('button', { class: 'mini-btn', text: '→' });
  right.title = 'Pan later'; right.onclick = () => { panSeries(snaps, 1); };
  const reset = el('button', { class: 'mini-btn', text: 'reset' });
  reset.onclick = () => { state.series.endT = null; render(); };
  wrap.append(left, right, reset);
  return wrap;
}

function windowedSnaps(snaps) {
  if (!snaps.length) return [];
  const first = snaps[0].t, latest = snaps[snaps.length - 1].t;
  const total = Math.max(latest - first, 1);
  let range = state.series.rangeMs == null ? total : Math.min(state.series.rangeMs, total);
  let end = state.series.endT == null ? latest : clamp(state.series.endT, first + range, latest);
  if (state.series.rangeMs == null) { end = latest; range = total; }
  const start = end - range;
  const out = snaps.filter(sn => sn.t >= start && sn.t <= end);
  return out.length >= 2 ? out : snaps.slice(-2);
}

function collectSeries(snaps) {
  const keys = Array.from(new Set(snaps.flatMap(sn => (sn.instances || []).map(i => i.key)))).sort();
  const bySnap = snaps.map(sn => {
    const m = new Map();
    for (const i of (sn.instances || [])) m.set(i.key, i);
    return m;
  });
  const out = [];
  keys.forEach((key, keyIdx) => {
    SERIES_FIELDS.forEach((meta, fieldIdx) => {
      const ys = bySnap.map(m => {
        const v = m.get(key)?.[meta.field];
        return v == null || !Number.isFinite(Number(v)) ? null : Number(v);
      });
      if (!ys.some(v => v != null)) return;
      out.push({
        id: key + '|' + meta.field,
        key,
        field: meta.field,
        label: `${key} · ${meta.label}`,
        unit: meta.unit,
        color: PALETTE[(keyIdx * SERIES_FIELDS.length + fieldIdx) % PALETTE.length],
        dash: meta.dash,
        ys,
      });
    });
  });
  return out;
}

function axisDomains(series) {
  const domains = {};
  for (const meta of SERIES_FIELDS) {
    let lo = Infinity, hi = -Infinity;
    if (meta.fixed) { lo = meta.fixed[0]; hi = meta.fixed[1]; }
    if (meta.field === 'rpm') lo = 0;
    for (const se of series) if (se.field === meta.field) {
      for (const v of se.ys) if (v != null) { lo = Math.min(lo, v); hi = Math.max(hi, v); }
    }
    if (!isFinite(lo) || !isFinite(hi)) { lo = 0; hi = 1; }
    if (hi - lo < 1) hi = lo + 1;
    if (!meta.fixed) {
      const pad = (hi - lo) * 0.08;
      lo = Math.max(meta.field === 'rpm' ? 0 : -Infinity, lo - pad);
      hi += pad;
    }
    domains[meta.field] = [lo, hi];
  }
  return domains;
}

function drawGridAndAxes(s, domain, box) {
  const { W, H, plotL, plotR, plotT, plotB, plotW, plotH, rangeMs, liveEdge } = box;
  const yFor = (field, v) => plotB - (v - domain[field][0]) / (domain[field][1] - domain[field][0]) * plotH;
  const tickVals = (field) => Array.from({ length: 5 }, (_, g) => domain[field][0] + (domain[field][1] - domain[field][0]) * g / 4);
  // Temperature grid owns the horizontal grid; the other axes share the same plot.
  for (const v of tickVals('temp')) {
    const y = yFor('temp', v);
    s.append(svg('line', { x1: plotL, y1: y, x2: plotR, y2: y, class: 'axis' }));
    const tx = svg('text', { x: plotL - 7, y: y + 3, class: 'axis-txt axis-temp', 'text-anchor': 'end' });
    tx.textContent = fmt(v, 0);
    s.append(tx);
  }
  const axes = [
    ['duty', plotR + 7, 'start', 'axis-duty', '%'],
    ['rpm', plotR + 58, 'start', 'axis-rpm', 'rpm'],
  ];
  for (const [field, x, anchor, cls, unit] of axes) {
    s.append(svg('line', { x1: x - 5, y1: plotT, x2: x - 5, y2: plotB, class: 'axis ' + cls }));
    for (const v of tickVals(field)) {
      const y = yFor(field, v);
      const tx = svg('text', { x, y: y + 3, class: 'axis-txt ' + cls, 'text-anchor': anchor });
      tx.textContent = fmt(v, field === 'rpm' ? 0 : 0);
      s.append(tx);
    }
    const lab = svg('text', { x, y: plotT - 7, class: 'axis-txt ' + cls, 'text-anchor': anchor });
    lab.textContent = unit;
    s.append(lab);
  }
  s.append(svg('line', { x1: plotL, y1: plotB, x2: plotR, y2: plotB, class: 'axis' }));
  const xl = svg('text', { x: plotL, y: H - 8, class: 'axis-txt', 'text-anchor': 'start' });
  xl.textContent = '-' + Math.round((rangeMs || 0) / 1000) + 's';
  const xr = svg('text', { x: plotR, y: H - 8, class: 'axis-txt', 'text-anchor': 'end' });
  xr.textContent = liveEdge ? 'now' : 'window';
  s.append(xl, xr);
  const legend = svg('text', { x: plotL, y: plotT - 7, class: 'axis-txt axis-temp', 'text-anchor': 'start' });
  legend.textContent = '°C';
  s.append(legend);
}

function seriesLegend(series) {
  const legend = el('div', { class: 'legend series-legend' });
  for (const se of series) {
    const hidden = !!state.series.hidden[se.id];
    const b = el('button', { class: 'series-toggle' + (hidden ? ' muted' : '') });
    b.innerHTML = `<i style="background:${se.color}"></i><span>${escapeHtml(se.label)}</span>`;
    b.onclick = () => {
      if (hidden) delete state.series.hidden[se.id];
      else state.series.hidden[se.id] = true;
      render();
    };
    legend.append(b);
  }
  return legend;
}

function zoomSeries(snaps, factor) {
  const first = snaps[0].t, latest = snaps[snaps.length - 1].t;
  const total = Math.max(latest - first, 30 * 1000);
  const current = state.series.rangeMs == null ? total : state.series.rangeMs;
  const next = clamp(current * factor, 30 * 1000, total);
  state.series.rangeMs = next >= total * 0.98 ? null : Math.round(next);
  state.series.endT = state.series.endT == null ? latest : clamp(state.series.endT, first + next, latest);
  render();
}

function panSeries(snaps, dir) {
  const first = snaps[0].t, latest = snaps[snaps.length - 1].t;
  const total = Math.max(latest - first, 1);
  if (state.series.rangeMs == null || state.series.rangeMs >= total) return;
  const range = state.series.rangeMs;
  const current = state.series.endT == null ? latest : state.series.endT;
  state.series.endT = clamp(current + dir * range * 0.5, first + range, latest);
  render();
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
      el('td', { text: i.seconds_since_seen + 's' }),
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
    // pressure raises flow speed; color stays steady so motion, not intensity, carries load.
    t += 0.006 + p * 0.04;
    const baseW = 1.2;
    const op = 0.28;
    svgEl.style.opacity = 0.72;
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
      ln.el.style.stroke = 'var(--wind-stroke)';
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
