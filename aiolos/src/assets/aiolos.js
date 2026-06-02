/* Aiolos — "The Sky of the Winds". Vanilla JS, no frameworks, no external network.
   The home is an open atmosphere of luminous bodies: a device's glow IS its
   temperature, its fans shed visible wind, its name is human. One headline per
   device; everything else is progressive disclosure (click a body to focus).
   Build-once-then-patch: views patch live values in place, never flashing. */
'use strict';

const SVGNS = 'http://www.w3.org/2000/svg';
const POLL_MS = 2500;
const TRACE_POINTS = 60;

const state = {
  status: null, history: null,
  view: { name: 'overview', key: null },
  pressure: 0,
  curveCache: {},
  series: { rangeMs: 15 * 60 * 1000, endT: null, hidden: {} },
  failures: 0,
  dom: { sig: null, run: [] },
};
let _reg = null;
function reg(fn) { if (_reg) _reg.push(fn); }

/* ---------- DOM helpers ---------- */
function el(tag, attrs, children) {
  const e = document.createElement(tag);
  if (attrs) for (const k in attrs) {
    if (k === 'class') e.className = attrs[k];
    else if (k === 'html') e.innerHTML = attrs[k];
    else if (k === 'text') e.textContent = attrs[k];
    else if (k === 'on') for (const ev in attrs[k]) e.addEventListener(ev, attrs[k][ev]);
    else e.setAttribute(k, attrs[k]);
  }
  if (children) for (const c of [].concat(children)) if (c != null) e.append(c.nodeType ? c : document.createTextNode(c));
  return e;
}
function svg(tag, attrs, children) {
  const e = document.createElementNS(SVGNS, tag);
  if (attrs) for (const k in attrs) e.setAttribute(k, attrs[k]);
  if (children) for (const c of [].concat(children)) if (c != null) e.append(c);
  return e;
}
const $ = (id) => document.getElementById(id);
const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
const fmt = (v, d = 0) => (v == null || isNaN(v)) ? '–' : Number(v).toFixed(d);
function escapeHtml(s) { return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c])); }

/* ---------- temperature → colour ramp ---------- */
const RAMP = [[26, '#5fd3e0'], [46, '#74d6a6'], [62, '#e8c466'], [76, '#f0935a'], [88, '#f0564a']];
function mix(a, b, f) {
  const pa = [parseInt(a.slice(1, 3), 16), parseInt(a.slice(3, 5), 16), parseInt(a.slice(5, 7), 16)];
  const pb = [parseInt(b.slice(1, 3), 16), parseInt(b.slice(3, 5), 16), parseInt(b.slice(5, 7), 16)];
  return 'rgb(' + pa.map((c, i) => Math.round(c + (pb[i] - c) * f)).join(',') + ')';
}
function tempColor(t) {
  if (t == null || isNaN(t)) return 'var(--accent)';
  if (t <= RAMP[0][0]) return RAMP[0][1];
  if (t >= RAMP[RAMP.length - 1][0]) return RAMP[RAMP.length - 1][1];
  for (let i = 1; i < RAMP.length; i++) if (t <= RAMP[i][0]) { const [t0, c0] = RAMP[i - 1], [t1, c1] = RAMP[i]; return mix(c0, c1, (t - t0) / (t1 - t0)); }
  return RAMP[RAMP.length - 1][1];
}

/* ---------- data ---------- */
function pnum(p) { if (!p || p.value == null) return null; const n = Number(p.value); return Number.isFinite(n) ? n : null; }
function aggregate(components) {
  let maxTemp = null, drivingTemp = null, drivingPct = null, maxDuty = null, maxRpm = null;
  for (const c of components || []) {
    for (const p of (c.publishers || [])) {
      const v = pnum(p); if (v == null) continue;
      if (p.kind === 'temperature') maxTemp = Math.max(maxTemp ?? -1e9, v);
      else if (p.kind === 'driving-temperature') drivingTemp = v;
      else if (p.kind === 'driving-duty') drivingPct = v;
      else if (p.kind === 'fan-duty') maxDuty = Math.max(maxDuty ?? -1e9, v);
      else if (p.kind === 'fan-rpm') maxRpm = Math.max(maxRpm ?? -1e9, v);
    }
    for (const k of (c.sinks || [])) { const v = pnum(k); if (v != null && k.kind === 'fan-duty') maxDuty = Math.max(maxDuty ?? -1e9, v); }
  }
  const fix = (x) => x === -1e9 ? null : x;
  return { temp: drivingTemp != null ? drivingTemp : fix(maxTemp), maxTemp: fix(maxTemp), duty: drivingPct != null ? drivingPct : fix(maxDuty), rpm: fix(maxRpm) };
}
function powerCtx(c) {
  const g = (kind) => { const p = (c.publishers || []).find(x => x.kind === kind); return p ? p.value : null; };
  return { charge: Number(g('power-charge')), online: g('power-online') === true || g('power-online') === 'true',
    onBattery: g('power-on-battery') === true || g('power-on-battery') === 'true',
    lowBattery: g('power-low-battery') === true || g('power-low-battery') === 'true',
    runtime: Number(g('power-runtime')), load: Number(g('power-load')), voltage: Number(g('power-voltage')) };
}
function deviceCtx(c) { const a = aggregate([c]); return { ...a, power: c.class === 'power' ? powerCtx(c) : null }; }
function primaryValue(c) {
  const a = aggregate([c]);
  if (c.class === 'power') { const p = powerCtx(c); if (!isNaN(p.charge)) return { v: fmt(p.charge), u: '%', k: 'charge', col: p.lowBattery ? 'var(--bad)' : p.onBattery ? 'var(--warn)' : 'var(--good)' }; }
  if (a.temp != null) return { v: fmt(a.temp), u: '°', k: 'driving', col: tempColor(a.temp) };
  if (a.maxTemp != null) return { v: fmt(a.maxTemp), u: '°', k: 'temp', col: tempColor(a.maxTemp) };
  if (a.duty != null) return { v: fmt(a.duty), u: '%', k: 'duty', col: 'var(--accent)' };
  if (a.rpm != null) return { v: fmt(a.rpm), u: '', k: 'rpm', col: 'var(--aether)' };
  const p = (c.publishers || []).find(x => x.value != null);
  return p ? { v: String(p.value), u: p.unit || '', k: p.label || p.kind, col: 'var(--ink)' } : { v: '–', u: '', k: 'idle', col: 'var(--ink-faint)' };
}
const CLASS_NAMES = { gpu: 'Graphics', cpu: 'Processors', ssd: 'Storage', board: 'Mainboard', power: 'Power', ups: 'Power', nic: 'Network', fan: 'Fans', mock: 'Mock' };
const className = (c) => CLASS_NAMES[c] || (c ? c[0].toUpperCase() + c.slice(1) : 'Devices');
const CLUSTER_ORDER = ['gpu', 'cpu', 'board', 'ssd', 'nic', 'power', 'ups', 'fan', 'mock'];
function clusterOrder(a, b) { const ia = CLUSTER_ORDER.indexOf(a), ib = CLUSTER_ORDER.indexOf(b); return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib); }
function flattenDevices(status) {
  const out = [];
  for (const i of (status.instances || [])) for (const c of (i.components || []))
    out.push({ inst: i, component: c, key: i.module + ':' + i.id + '/' + c.id, cls: c.class || 'device' });
  return out;
}
function findDevice(key) { return flattenDevices(state.status || {}).find(d => d.key === key); }
/* ---- unit model (SOW-0018): a unit = hardware assembled from the wire's flat units/components/
   signals. We reconstruct each component's publishers (producers) + sinks so the rest of the UI
   consumes one stable shape. A unit reported by several anemoi (the motherboard) is already merged
   server-side into one `units[]` entry. ---- */
const L = (o, k, d = '') => (o && o.labels && o.labels[k] != null) ? o.labels[k] : d;
function buildUnits(status) {
  const comps = {};
  for (const c of (status.components || [])) comps[c.id] = { id: c.id, label: L(c, 'name', c.id), class: L(c, 'type', 'device'), unit: c.unit, publishers: [], sinks: [] };
  for (const s of (status.signals || [])) {
    const c = comps[s.component]; if (!c) continue;
    const base = { id: s.id, label: L(s, 'name', s.id), kind: L(s, 'type', ''), value: s.value, unit: s.uom };
    if (s.role === 'sink') {
      const sk = { ...base, state: s.control ? s.control.state : null, driven_by: s.control ? (s.control.driven_by || []) : [], safe: s.control ? s.control.safe : null, driving: s.control ? s.control.driving : null };
      if (s.labels && s.labels.fault) sk.fault = true;
      c.sinks.push(sk);
    } else c.publishers.push(base);
  }
  const units = {};
  for (const u of (status.units || [])) units[u.id] = {
    key: u.id, type: L(u, 'type', 'device'), sources: u.sources || [], components: [],
    inst: { id: u.id, name: L(u, 'name', u.id), description: L(u, 'description', ''), type: L(u, 'type', 'device'), status: u.status || 'ok' },
  };
  for (const c of Object.values(comps)) { const u = units[c.unit]; if (u) u.components.push(c); }
  for (const u of Object.values(units)) u.components.sort((a, b) => a.id < b.id ? -1 : 1);
  return Object.values(units);
}
function unitType(u) { return (u.type && CLASS_NAMES[u.type]) ? u.type : (u.type || 'device'); }
function flattenUnits(status) { return buildUnits(status); }
function findUnit(key) { return buildUnits(state.status || {}).find(u => u.key === key); }
function unitCtx(u) {
  const a = aggregate(u.components);
  const pc = u.components.find(c => (c.publishers || []).some(p => /^power-/.test(p.kind)));
  return { ...a, power: pc ? powerCtx(pc) : null };
}
function unitPrimary(u) {
  const ctx = unitCtx(u);
  if (ctx.power && !isNaN(ctx.power.charge)) return { v: fmt(ctx.power.charge), u: '%', k: 'charge', col: ctx.power.lowBattery ? 'var(--bad)' : ctx.power.onBattery ? 'var(--warn)' : 'var(--good)' };
  if (ctx.temp != null) return { v: fmt(ctx.temp), u: '°', k: 'driving', col: tempColor(ctx.temp) };
  if (ctx.maxTemp != null) return { v: fmt(ctx.maxTemp), u: '°', k: 'temp', col: tempColor(ctx.maxTemp) };
  if (ctx.duty != null) return { v: fmt(ctx.duty), u: '%', k: 'duty', col: 'var(--accent)' };
  if (ctx.rpm != null) return { v: fmt(ctx.rpm), u: '', k: 'rpm', col: 'var(--aether)' };
  return { v: '–', u: '', k: 'idle', col: 'var(--ink-faint)' };
}
// Extract, from a unit's components, one temperature per temp-bearing component + every fan (rpm+duty).
function unitView(u) {
  const temps = [], fans = [];
  for (const c of u.components) {
    let cmax = null;
    for (const p of (c.publishers || [])) if (p.kind === 'temperature') { const v = pnum(p); if (v != null) cmax = Math.max(cmax ?? -1e9, v); }
    if (cmax != null && cmax !== -1e9) temps.push({ label: c.label || c.id, value: cmax });
    const rpms = (c.publishers || []).filter(p => p.kind === 'fan-rpm');
    // Fan duty arrives as a controllable SINK (fans we drive) OR a read-only PUBLISHER (fans we only
    // report, e.g. a BIOS-driven CPU fan). Take both, so every fan shows its duty — not just the
    // ones with a sink. Sinks win on overlap (they also carry the claim state).
    const dutyPubs = (c.publishers || []).filter(p => p.kind === 'fan-duty');
    const dutySinks = (c.sinks || []).filter(s => s.kind === 'fan-duty');
    if ((rpms.length + dutyPubs.length + dutySinks.length) && rpms.length <= 1 && dutyPubs.length <= 1 && dutySinks.length <= 1) {
      const ds = dutySinks[0], dp = dutyPubs[0];
      fans.push({ label: c.label || c.id, rpm: rpms[0] ? pnum(rpms[0]) : null, duty: ds ? pnum(ds) : (dp ? pnum(dp) : null), state: ds ? ds.state : null });
    } else {
      const m = {};
      for (const p of rpms) { const n = (p.id || '').split('.')[0]; (m[n] ??= { label: p.label || n }).rpm = pnum(p); }
      for (const p of dutyPubs) { const n = (p.id || '').split('.')[0]; (m[n] ??= { label: p.label || n }).duty = pnum(p); }
      for (const s of dutySinks) { const n = (s.id || '').split('.')[0]; (m[n] ??= { label: s.label || n }).duty = pnum(s); m[n].state = s.state; if (s.label) m[n].label = s.label; }
      for (const n of Object.keys(m)) fans.push({ label: m[n].label || n, rpm: m[n].rpm ?? null, duty: m[n].duty ?? null, state: m[n].state });
    }
  }
  return { temps, fans };
}
function modulesMap(status) {
  const m = {};
  for (const md of (status.modules || [])) m[md.module] = { detect: md.detect_status, detect_error: md.detect_error, instances: [] };
  for (const i of (status.instances || [])) { (m[i.module] ||= { detect: '?', instances: [] }).instances.push(i); }
  return m;
}
function instPressure(a) { const t = a.temp == null ? 0 : clamp((a.temp - 30) / 60, 0, 1); const d = a.duty == null ? 0 : clamp(a.duty / 100, 0, 1); return Math.max(t, d); }
function friendlyName(d, idx) {
  const lbl = d.component.label || d.component.id;
  if (d.cls === 'gpu' && /^GPU-[0-9a-f]{6}/i.test(lbl)) return 'GPU ' + idx;     // UUID → human index (real name comes from the module later)
  return lbl;
}
function bodyHeat(ctx, cls) {
  if (cls === 'power') { const p = ctx.power || {}; return p.lowBattery ? 1 : p.onBattery ? 0.85 : 0.12; }
  const t = ctx.maxTemp != null ? ctx.maxTemp : ctx.temp;
  if (t == null) return ctx.duty != null ? clamp(ctx.duty / 100, 0, 1) * 0.5 : 0.1;
  return clamp((t - 30) / 55, 0.05, 1);
}
function whisperText(c, ctx) {
  if (c.class === 'power' && ctx.power && !isNaN(ctx.power.runtime)) return `${Math.round(ctx.power.runtime / 60)} min reserve · ${fmt(ctx.power.load)}% load`;
  const bits = [];
  if (ctx.duty != null) bits.push(`fans ${fmt(ctx.duty)}%`);
  if (ctx.rpm != null) bits.push(`${fmt(ctx.rpm)} rpm`);
  return bits.join('  ·  ');
}

/* ---------- reactive icon library (.update patches in place) ---------- */
function bladeGroup(cx, cy, r, n = 5) {
  const g = svg('g', { class: 'blades' });
  for (let i = 0; i < n; i++) {
    const a = (i / n) * Math.PI * 2;
    const x1 = cx + Math.cos(a) * (r * .28), y1 = cy + Math.sin(a) * (r * .28);
    const x2 = cx + Math.cos(a + .9) * r, y2 = cy + Math.sin(a + .9) * r;
    const xm = cx + Math.cos(a + .35) * r * 1.02, ym = cy + Math.sin(a + .35) * r * 1.02;
    g.append(svg('path', { class: 'fillc', d: `M${cx} ${cy} Q${xm.toFixed(1)} ${ym.toFixed(1)} ${x2.toFixed(1)} ${y2.toFixed(1)} L${x1.toFixed(1)} ${y1.toFixed(1)} Z`, opacity: .9 }));
  }
  return g;
}
function setSpin(node, rpm) { if (rpm != null && rpm > 0) { node.classList.add('spin'); node.style.setProperty('--spin', clamp(2400 / rpm, 0.12, 2.2).toFixed(2) + 's'); } else node.classList.remove('spin'); }
function makeFan(cx, cy, r, color) {
  const w = (r + 2) * 2;
  const wrap = svg('svg', { class: 'ic', x: cx - r - 2, y: cy - r - 2, width: w, height: w, viewBox: `0 0 ${w} ${w}`, style: `color:${color || 'var(--aether)'}` });
  wrap.append(svg('circle', { class: 'stroke', cx: r + 2, cy: r + 2, r, 'stroke-opacity': .4 }));
  wrap.append(bladeGroup(r + 2, r + 2, r - 1, 5));
  wrap.append(svg('circle', { class: 'fillc', cx: r + 2, cy: r + 2, r: 1.7 }));
  wrap.setRpm = (rpm) => setSpin(wrap, rpm);
  return wrap;
}
function icThermometer(ctx) {
  const s = svg('svg', { class: 'ic ic-temp', viewBox: '0 0 48 48' });
  const glow = svg('circle', { class: 'fillc glow', cx: 24, cy: 38, r: 8.5 }); glow.style.opacity = 0; s.append(glow);
  s.append(svg('rect', { class: 'stroke', x: 19.5, y: 5, width: 9, height: 27, rx: 4.5 }));
  s.append(svg('circle', { class: 'stroke', cx: 24, cy: 38, r: 7.5 }));
  s.append(svg('circle', { class: 'fillc', cx: 24, cy: 38, r: 4.6 }));
  // mercury: geometry set via CSS (style.y/height) so it transitions smoothly, never re-created.
  const merc = svg('rect', { class: 'fillc merc', x: 21.6, width: 4.8, rx: 2.4 }); s.append(merc);
  for (let k = 0; k < 4; k++) s.append(svg('line', { class: 'stroke', x1: 29, y1: 10 + k * 5.5, x2: 32, y2: 10 + k * 5.5, 'stroke-width': 1.2, 'stroke-opacity': .7 }));
  s.update = (c) => {
    const t = c.maxTemp != null ? c.maxTemp : c.temp;
    s.style.color = tempColor(t);
    const f = clamp(t == null ? 0 : (t - 20) / 70, 0.06, 1), top = 30 - f * 22;
    merc.style.y = top.toFixed(2) + 'px'; merc.style.height = (32 - top).toFixed(2) + 'px';
    glow.style.opacity = t != null && t >= 72 ? clamp((t - 72) / 18, 0, .7).toFixed(2) : 0;
  };
  s.update(ctx); return s;
}
function icFan(ctx) {
  const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48', style: 'color:var(--aether)' });
  s.append(svg('circle', { class: 'stroke', cx: 24, cy: 24, r: 19, 'stroke-opacity': .4 }));
  s.append(bladeGroup(24, 24, 15, 5));
  s.append(svg('circle', { class: 'fillc', cx: 24, cy: 24, r: 3.4 }));
  s.update = (c) => setSpin(s, c.rpm); s.update(ctx); return s;
}
function icGpu(ctx) {
  const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48' });
  s.append(svg('rect', { class: 'stroke', x: 5, y: 13, width: 36, height: 22, rx: 3 }));
  s.append(svg('path', { class: 'stroke', d: 'M9 35v6M16 35v6', 'stroke-width': 1.4 }));
  const f1 = makeFan(16, 24, 7, 'var(--aether)'), f2 = makeFan(31, 24, 7, 'var(--aether)'); s.append(f1, f2);
  s.append(svg('rect', { class: 'fillc', x: 38, y: 17, width: 3, height: 14, rx: 1.5, opacity: .7 }));
  s.update = (c) => { s.style.color = tempColor(c.maxTemp); f1.setRpm(c.rpm); f2.setRpm(c.rpm); }; s.update(ctx); return s;
}
function icBoard(ctx) {
  const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48' });
  s.append(svg('rect', { class: 'stroke', x: 6, y: 6, width: 36, height: 36, rx: 3 }));
  s.append(svg('rect', { class: 'stroke', x: 26, y: 11, width: 12, height: 12, rx: 1.5, 'stroke-width': 1.3 }));
  s.append(svg('path', { class: 'stroke', d: 'M10 12h10M10 17h7M10 22h10M10 28h6M28 30h10M30 36h8', 'stroke-width': 1.2, 'stroke-opacity': .8 }));
  const f = makeFan(16, 33, 6, 'var(--aether)'); s.append(f);
  s.update = (c) => { s.style.color = tempColor(c.maxTemp); f.setRpm(c.rpm); }; s.update(ctx); return s;
}
function icSsd(ctx) {
  const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48' });
  s.append(svg('rect', { class: 'stroke', x: 8, y: 10, width: 32, height: 28, rx: 3 }));
  s.append(svg('path', { class: 'stroke', d: 'M14 16h20M14 21h14', 'stroke-width': 1.3, 'stroke-opacity': .8 }));
  for (let k = 0; k < 5; k++) s.append(svg('rect', { class: 'fillc', x: 13 + k * 4.2, y: 31, width: 2.4, height: 3.4, rx: 1, opacity: k % 2 ? .5 : .9 }));
  s.update = (c) => { s.style.color = tempColor(c.maxTemp); }; s.update(ctx); return s;
}
function icCpu(ctx) {
  const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48' });
  s.append(svg('rect', { class: 'stroke', x: 13, y: 13, width: 22, height: 22, rx: 2 }));
  s.append(svg('rect', { class: 'fillc', x: 19, y: 19, width: 10, height: 10, rx: 1.5, opacity: .55 }));
  for (let k = 0; k < 3; k++) { const p = 17 + k * 7; s.append(svg('line', { class: 'stroke', x1: p, y1: 8, x2: p, y2: 13, 'stroke-width': 1.4 })); s.append(svg('line', { class: 'stroke', x1: p, y1: 35, x2: p, y2: 40, 'stroke-width': 1.4 })); s.append(svg('line', { class: 'stroke', x1: 8, y1: p, x2: 13, y2: p, 'stroke-width': 1.4 })); s.append(svg('line', { class: 'stroke', x1: 35, y1: p, x2: 40, y2: p, 'stroke-width': 1.4 })); }
  s.update = (c) => { s.style.color = tempColor(c.maxTemp); }; s.update(ctx); return s;
}
function icNic() { const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48', style: 'color:var(--aether)' }); s.append(svg('rect', { class: 'stroke', x: 6, y: 18, width: 36, height: 16, rx: 2 })); s.append(svg('path', { class: 'stroke', d: 'M13 18v-6h10v6M25 34v6M19 34v6M31 34v6', 'stroke-width': 1.3 })); s.append(svg('path', { class: 'stroke', d: 'M14 26h8M26 26h8', 'stroke-width': 1.4 })); s.update = () => {}; return s; }
function icPower(ctx) {
  const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48' });
  s.append(svg('rect', { class: 'stroke', x: 9, y: 12, width: 26, height: 24, rx: 3 }));
  s.append(svg('rect', { class: 'fillc', x: 35, y: 19, width: 4, height: 10, rx: 2 }));
  const fill = svg('rect', { class: 'fillc', x: 12, width: 20, rx: 1.5, opacity: .85 }); s.append(fill);
  const bolt = svg('path', { d: 'M24 15l-5 9h4l-2 8 7-11h-5z', fill: 'var(--bg0)', stroke: 'none', opacity: .9 }); bolt.style.display = 'none'; s.append(bolt);
  s.update = (c) => { const p = c.power || {}, charge = isNaN(p.charge) ? null : p.charge; s.style.color = p.lowBattery ? 'var(--bad)' : p.onBattery ? 'var(--warn)' : 'var(--good)'; const f = charge == null ? 0 : clamp(charge / 100, 0, 1), fh = 20 * f; fill.style.y = (33 - fh).toFixed(2) + 'px'; fill.style.height = fh.toFixed(2) + 'px'; bolt.style.display = (p.online && !p.onBattery) ? '' : 'none'; };
  s.update(ctx); return s;
}
function icWind() { const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48', style: 'color:var(--accent)' }); s.append(svg('path', { class: 'stroke', d: 'M6 18h22a5 5 0 1 0-5-5M6 26h30a5 5 0 1 1-5 5M6 34h16a4 4 0 1 1-4 4' })); s.update = () => {}; return s; }
function deviceIcon(cls, ctx) {
  switch (cls) { case 'gpu': return icGpu(ctx); case 'cpu': return icCpu(ctx); case 'ssd': return icSsd(ctx); case 'board': return icBoard(ctx); case 'power': return icPower(ctx); case 'nic': return icNic(); case 'fan': return icFan(ctx); default: return icWind(); }
}
function kindIcon(kind, value) {
  if (/temperature/.test(kind)) return icThermometer({ maxTemp: Number(value) });
  if (kind === 'fan-rpm') return icFan({ rpm: Number(value) });
  if (kind === 'fan-duty' || kind === 'driving-duty') { const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48', style: 'color:var(--accent)' }); s.append(svg('path', { class: 'stroke', d: 'M24 40a16 16 0 1 1 16-16' })); s.append(svg('line', { class: 'stroke', x1: 24, y1: 24, x2: 33, y2: 17, 'stroke-width': 2 })); s.update = () => {}; return s; }
  if (/^power-/.test(kind)) return icPower({ power: {} });
  const s = svg('svg', { class: 'ic', viewBox: '0 0 48 48', style: 'color:var(--ink-faint)' }); s.append(svg('circle', { class: 'fillc', cx: 24, cy: 24, r: 4 })); s.update = () => {}; return s;
}
function sizeIcon(node, px) { node.setAttribute('width', px); node.setAttribute('height', px); return node; }

// duty% → degrees/sec: a WIDE, dramatic range — idle crawl (~40°/s) to a frantic whir (~1740°/s ≈ 4.8 rev/s).
function fanSpeed(duty) { return 40 + Math.pow(clamp((duty == null ? 0 : duty) / 100, 0, 1), 1.3) * 1700; }
function dutyColor(d) { return d == null ? 'var(--aether)' : d >= 85 ? 'var(--bad)' : d >= 55 ? 'var(--accent)' : 'var(--aether)'; }
// A fan as a producer+sink glyph: a duty ring (fills ∝ duty) around blades that spin ∝ duty.
// The spin is a compositor-driven Web Animations rotation — we only change its playbackRate, so the
// speed glides with ZERO per-frame main-thread work and never restarts (no hiccup). The blade group's
// CSS transform-origin is its centre (50% 50% of the view-box), so it spins in place.
const SPIN_BASE_DPS = 360;   // one full turn per second at playbackRate 1
function fanGlyph() {
  const R = 19, C = 2 * Math.PI * R;
  const s = svg('svg', { class: 'ic fan-glyph', viewBox: '0 0 48 48' });
  s.append(svg('circle', { class: 'duty-track', cx: 24, cy: 24, r: R, fill: 'none', 'stroke-width': 3.2 }));
  const arc = svg('circle', { class: 'duty-arc', cx: 24, cy: 24, r: R, fill: 'none', 'stroke-width': 3.2, transform: 'rotate(-90 24 24)', 'stroke-linecap': 'round' });
  arc.style.strokeDasharray = `0 ${C}`; s.append(arc);
  const blades = bladeGroup(24, 24, 12.5, 5); s.append(blades);
  s.append(svg('circle', { class: 'fillc', cx: 24, cy: 24, r: 3 }));
  const spin = blades.animate
    ? blades.animate([{ transform: 'rotate(0deg)' }, { transform: 'rotate(360deg)' }], { duration: 1000, iterations: Infinity, easing: 'linear' })
    : null;
  if (spin) spin.playbackRate = 0;
  s.update = (fan) => {
    const duty = fan.duty != null ? fan.duty : (fan.rpm != null ? clamp(fan.rpm / 4500 * 100, 0, 100) : 0);
    arc.style.strokeDasharray = `${(clamp(duty / 100, 0, 1) * C).toFixed(1)} ${C.toFixed(1)}`;
    const col = dutyColor(fan.duty); s.style.color = col; arc.style.stroke = col;
    if (spin) spin.playbackRate = fanSpeed(duty) / SPIN_BASE_DPS;
  };
  return s;
}
// The unit's hero icon = its PRIMARY measurement's animated icon (thermometer / battery) — NOT a device silhouette.
function primaryGlyph(unit) {
  const ic = unitPrimary(unit).k === 'charge' ? icPower(unitCtx(unit)) : icThermometer(unitCtx(unit));
  return { el: ic, update: (u) => ic.update(unitCtx(u)) };
}

/* ---------- data loop ---------- */
async function poll() {
  try {
    const [s, h] = await Promise.all([
      fetch('/status.json', { cache: 'no-store' }).then(r => r.json()),
      fetch('/history.json', { cache: 'no-store' }).then(r => r.json()).catch(() => ({ snaps: [] })),
    ]);
    state.status = s; state.history = h; state.failures = 0; setConn('ok', 'live'); onData();
  } catch (e) { state.failures++; setConn(state.failures > 2 ? 'down' : 'stale', state.failures > 2 ? 'offline' : 'reconnecting'); }
  finally { setTimeout(poll, POLL_MS); }
}
function setConn(cls, txt) { const c = $('conn'); if (c) { c.className = 'conn ' + cls; c.textContent = txt; } }
function onData() {
  let p = 0; for (const u of buildUnits(state.status || {})) p = Math.max(p, instPressure(unitCtx(u)));
  state.pressure = p; render();
}
function pressureColor(p) { return p >= 0.8 ? 'var(--bad)' : p >= 0.55 ? 'var(--warn)' : p >= 0.3 ? 'var(--accent)' : 'var(--aether)'; }

/* ---------- render pipeline ---------- */
function render() { syncChrome(); syncStage(); }
function syncChrome() {
  const p = state.pressure, col = pressureColor(p);
  $('pnum').textContent = Math.round(p * 100);
  $('pnum').style.color = col;
  // The whole page reacts to pressure: a ring fills the meter, the aurora reddens, and at high
  // pressure the body goes into an alarm state (vignette + faster winds).
  const root = document.documentElement.style;
  root.setProperty('--pressure', p.toFixed(3));
  root.setProperty('--p-col', col);
  document.body.classList.toggle('under-pressure', p >= 0.7);
  document.body.classList.toggle('alarm', p >= 0.85);
  for (const b of document.querySelectorAll('.lens')) b.classList.toggle('active', b.dataset.view === state.view.name);
  let hot = null; for (const u of buildUnits(state.status || {})) { const a = unitCtx(u); if (a.maxTemp != null) hot = Math.max(hot ?? -1e9, a.maxTemp); }
  const aur = $('aurora'); if (aur) { aur.style.setProperty('--aura', p >= 0.5 ? col : (hot != null ? tempColor(hot) : 'transparent')); aur.style.setProperty('--aura-op', (0.16 + p * 0.62).toFixed(2)); }
}
function unitsSig() { return flattenUnits(state.status).map(u => u.key + ':' + u.components.length).join(','); }
function stageSig() {
  const v = state.view;
  if (v.name === 'overview') return 'o|' + unitsSig();
  if (v.name === 'device') { const u = findUnit(v.key); if (!u) return 'd|none'; return 'd|' + v.key + '|' + u.components.map(c => c.id).join(','); }
  return v.name;
}
function syncStage() {
  const patchable = state.view.name === 'overview' || state.view.name === 'device';
  const sig = stageSig();
  if (patchable && sig === state.dom.sig && state.dom.run.length) { for (const f of state.dom.run) f(); return; }
  const run = []; _reg = run;
  const stage = $('stage'); stage.textContent = '';
  const v = state.view;
  if (v.name === 'overview') stage.append(viewSky());
  else if (v.name === 'device') stage.append(viewFocus(v.key));
  else if (v.name === 'series') stage.append(viewFlux());
  else if (v.name === 'curve') stage.append(viewLaw());
  else if (v.name === 'health') stage.append(viewPulse());
  _reg = null;
  state.dom = { sig: patchable ? sig : ' ', run };
}
function go(name, key) {
  state.view = { name, key: key || null };
  const h = name + (key ? '/' + encodeURIComponent(key) : '');
  try { history.replaceState(null, '', location.pathname + location.search + '#' + h); } catch (e) {}
  render();
}
function applyHash() {
  const raw = location.hash.slice(1); if (!raw) { state.view = { name: 'overview', key: null }; return; }
  const i = raw.indexOf('/'); const name = i < 0 ? raw : raw.slice(0, i); const key = i < 0 ? null : decodeURIComponent(raw.slice(i + 1));
  if (name === 'device' && key) state.view = { name: 'device', key }; else if (['overview', 'series', 'curve', 'health'].includes(name)) state.view = { name, key: null };
}

/* ===================== THE SKY (overview) ===================== */
function viewSky() {
  const frag = document.createDocumentFragment(); const units = flattenUnits(state.status);
  if (!units.length) { frag.append(el('div', { class: 'empty', text: 'No devices yet — the winds are still gathering.' })); return frag; }
  const by = {}; for (const u of units) (by[u.type] ||= []).push(u);
  const wrap = el('div', { class: 'constellation' });
  for (const t of Object.keys(by).sort(clusterOrder)) {
    const cluster = el('section', { class: 'cluster' });
    cluster.append(el('div', { class: 'cluster-label', text: className(t) }));
    const bodies = el('div', { class: 'bodies' });
    by[t].forEach((u, i) => bodies.append(unitBody(u, i)));
    cluster.append(bodies); wrap.append(cluster);
  }
  frag.append(wrap); return frag;
}
function streamerSvg() {
  const s = svg('svg', { class: 'streamers', viewBox: '0 0 320 150', preserveAspectRatio: 'none' });
  const paths = [];
  for (let k = 0; k < 5; k++) { const y = 26 + k * 24; const p = svg('path', { class: 'streamer', d: `M150 ${y} C196 ${y - 9}, 250 ${y + 11}, 318 ${y - 3}`, 'stroke-width': (1 + (k % 2) * .6).toFixed(1) }); s.append(p); paths.push(p); }
  s.update = (ctx) => { const duty = ctx.duty, rpm = ctx.rpm; const on = duty != null && duty > 0; s.style.opacity = on ? clamp(0.12 + (duty / 100) * 0.7, 0, .9).toFixed(2) : 0; const dur = rpm && rpm > 0 ? clamp(45000 / rpm, 0.5, 3.5).toFixed(2) + 's' : '2.4s'; for (const p of paths) p.style.setProperty('--flow', dur); };
  return s;
}
function fanPip(fan) {
  const g = sizeIcon(fanGlyph(), 34);
  const dlabel = el('span', { class: 'fan-duty' }), rlabel = el('span', { class: 'fan-rpm' });
  const wrap = el('span', { class: 'fan-pip' }, [g, dlabel, rlabel]);
  wrap.update = (f) => {
    g.update(f);
    wrap.classList.toggle('claimed', f.state === 'claimed');
    dlabel.textContent = f.duty != null ? fmt(f.duty) + '%' : '';
    rlabel.textContent = f.rpm != null ? fmt(f.rpm) : '';
  };
  wrap.update(fan); return wrap;
}
function tempChip(t) {
  const ic = sizeIcon(icThermometer({ maxTemp: t.value }), 16);
  const lbl = el('span', { class: 'tc-l', text: t.label }), b = el('b', {});
  const wrap = el('span', { class: 'temp-chip' }, [ic, lbl, b]);
  wrap.update = (tt) => { ic.update({ maxTemp: tt.value }); lbl.textContent = tt.label; b.textContent = fmt(tt.value) + '°'; b.style.color = tempColor(tt.value); };
  wrap.update(t); return wrap;
}
// A unit: no silhouette icon. Its hero is its primary measurement's animated icon; below it, all its
// temperatures (each a live thermometer) and all its fans (each a live duty gauge). Built ONCE; the
// updater patches values in place (never re-creating an SVG) so nothing hiccups.
function unitBody(u) {
  const body = el('div', { class: 'body', on: { click: () => go('device', u.key) } });
  const halo = el('div', { class: 'halo' });
  const dot = el('span', { class: 'bdot' });
  const glyph = primaryGlyph(u); const hero = sizeIcon(glyph.el, 58); hero.classList.add('uhero');
  const bvalText = document.createTextNode(''), bvalUnit = el('small');
  const bval = el('div', { class: 'bval' }, [bvalText, bvalUnit]);
  const bname = el('div', { class: 'bname', text: u.inst.name || u.key });
  const tempRow = el('div', { class: 'temp-row' });
  const fanRow = el('div', { class: 'fan-row' });
  body.append(halo, dot, hero, bval, bname, tempRow, fanRow);
  const v0 = unitView(u);
  const tempChips = (v0.temps.length > 1 ? v0.temps : []).map(tempChip); tempRow.append(...tempChips);
  const fanPips = v0.fans.map(fanPip); fanRow.append(...fanPips);
  const key = u.key;
  const update = () => {
    const unit = findUnit(key); if (!unit) return;
    const ctx = unitCtx(unit), pv = unitPrimary(unit), heat = bodyHeat(ctx, unit.type), view = unitView(unit);
    halo.style.setProperty('--halo', pv.col); halo.style.setProperty('--heat', heat.toFixed(2)); halo.classList.toggle('hot', heat > 0.72);
    glyph.update(unit);
    bvalText.textContent = pv.v; bvalUnit.textContent = pv.u; bval.style.color = pv.col;
    dot.className = 'bdot ' + (unit.inst.status === 'ok' ? 'ok' : 'bad');
    if (tempChips.length) view.temps.forEach((t, i) => tempChips[i] && tempChips[i].update(t));
    view.fans.forEach((f, i) => fanPips[i] && fanPips[i].update(f));
  };
  update(); reg(update); return body;
}

/* ===================== DEVICE FOCUS ===================== */
function groupReadings(c) {
  const groups = new Map(); const add = (t, p) => { if (!groups.has(t)) groups.set(t, []); groups.get(t).push(p); };
  for (const p of (c.publishers || [])) {
    if (p.value == null) continue; const k = p.kind, lbl = String(p.label || p.id);
    if (/^driving/.test(k)) add('Control', p);
    else if (k === 'temperature') add(/tccd|tctl|ccd|core/i.test(lbl) ? 'CPU cores' : 'Temperatures', p);
    else if (/^fan-/.test(k)) add('Fans', p);
    else if (/^power-/.test(k)) add('Power', p);
    else add('Sensors', p);
  }
  return groups;
}
function viewFocus(key) {
  const frag = document.createDocumentFragment(); const u0 = findUnit(key);
  if (!u0) { frag.append(el('button', { class: 'back-link', text: '← back to the sky', on: { click: () => go('overview') } }), el('div', { class: 'empty', text: 'Device no longer present.' })); return frag; }
  const wrap = el('div', { class: 'focus' });
  wrap.append(el('button', { class: 'back-link', text: '← back to the sky', on: { click: () => go('overview') } }));
  const i = u0.inst;
  const hero = el('div', { class: 'focus-hero' });
  const halo = el('div', { class: 'halo' });
  const core = sizeIcon(deviceIcon(u0.type, unitCtx(u0)), 150); core.classList.add('core');
  const fvalText = document.createTextNode(''), fvalUnit = el('small');
  const fval = el('div', { class: 'fval' }, [fvalText, fvalUnit]);
  hero.append(halo, core, fval, el('div', { class: 'fname', text: i.name || u0.key }), el('div', { class: 'fsub', text: `${i.module}:${i.id} · ${className(u0.type)} · ${i.status}` }));
  hero.append(el('div', {}, [traceSvg(i.module + ':' + i.id, 'var(--aether)')]));
  wrap.append(hero);
  const readings = el('div', { class: 'readings' }); wrap.append(readings);
  frag.append(wrap);
  const update = () => {
    const unit = findUnit(key); if (!unit) return;
    const ctx = unitCtx(unit), pv = unitPrimary(unit), heat = bodyHeat(ctx, unit.type);
    halo.style.setProperty('--halo', pv.col); halo.style.setProperty('--heat', heat.toFixed(2)); halo.classList.toggle('hot', heat > 0.72);
    core.update && core.update(ctx);
    fvalText.textContent = pv.v; fvalUnit.textContent = pv.u; fval.style.color = pv.col;
    // one section per component — the grouping is the data, not a regex
    readings.replaceChildren();
    for (const c of unit.components) {
      const g = el('div', { class: 'rgroup' }, [el('h4', { text: c.label || c.id })]);
      for (const p of (c.publishers || [])) {
        if (p.value == null) continue;
        g.append(el('div', { class: 'rrow' }, [sizeIcon(kindIcon(p.kind, p.value), 18), el('div', { text: String(p.label || p.id) }), el('div', { class: 'rv', html: `${escapeHtml(String(p.value))}${p.unit ? `<span class="ru">${escapeHtml(p.unit)}</span>` : ''}` })]));
      }
      for (const s of (c.sinks || [])) {
        const by = (s.driven_by || []).map(x => `${x.name}${x.value != null ? ' ' + fmt(Number(x.value)) + (x.uom || '°') : ''}`).join('  ·  ');
        g.append(el('div', { class: 'rrow' }, [sizeIcon(kindIcon('fan-duty', s.value), 18), el('div', { text: s.label || s.id }), el('div', { class: 'rv' }, [s.value != null ? String(s.value) : '–', s.unit ? el('span', { class: 'ru', text: s.unit }) : null, el('span', { class: 'statepill ' + (s.state || 'unknown').toLowerCase(), text: s.state || 'unknown' })])]));
        const d = s.driving;
        if (d && (d.input != null || d.output != null)) {
          const inp = d.input != null ? `${fmt(Number(d.input))}${d.uom || ''}` : '?';
          const out = d.output != null ? `${fmt(Number(d.output))}${s.unit || ''}` : '?';
          g.append(el('div', { class: 'lineage' }, [el('span', { class: 'lh', text: 'driving' }), `${inp} → ${out}${d.how ? '  (' + d.how + ')' : ''}`]));
        }
        if (by) g.append(el('div', { class: 'lineage' }, [el('span', { class: 'lh', text: 'driven by' }), by]));
      }
      readings.append(g);
    }
  };
  update(); reg(update); return frag;
}
function traceSvg(key, col) {
  const snaps = (state.history && state.history.snaps) || [], pts = [];
  for (const s of snaps) { const hi = (s.instances || []).find(x => x.key === key); if (hi && hi.temp != null) pts.push(hi.temp); }
  const s = svg('svg', { class: 'trace', viewBox: '0 0 600 64', preserveAspectRatio: 'none' });
  if (pts.length < 2) { s.append(svg('line', { x1: 0, y1: 62, x2: 600, y2: 62, class: 'axis' })); return s; }
  const recent = pts.slice(-TRACE_POINTS), min = Math.min(...recent), max = Math.max(...recent), span = (max - min) || 1;
  const X = (k) => (k / (recent.length - 1)) * 600, Y = (v) => 58 - ((v - min) / span) * 52;
  let d = ''; recent.forEach((v, k) => { d += (k ? 'L' : 'M') + X(k).toFixed(1) + ' ' + Y(v).toFixed(1) + ' '; });
  s.append(svg('path', { d: 'M0 64 ' + d.replace('M', 'L') + 'L600 64 Z', fill: col, opacity: '.1' }));
  s.append(svg('path', { d, fill: 'none', stroke: col, 'stroke-width': '2' }));
  return s;
}

/* ===================== FLUX (time-series) ===================== */
const SERIES_FIELDS = [
  { field: 'temp', label: 'Temperature', unit: '°C', fixed: [20, 100] },
  { field: 'duty', label: 'Fan duty', unit: '%', fixed: [0, 100], dash: '4 3' },
  { field: 'rpm', label: 'Fan RPM', unit: 'rpm', fixed: null, dash: '1 3' },
];
const PALETTE = ['#ecc873', '#6fd9e4', '#6fdca8', '#f0706e', '#b98e2e', '#3f9fc9', '#c98ad6', '#f0bd55', '#7fd1a4', '#7aa5e0'];
function lensHead(kicker, title, sub, extra) {
  return el('div', { class: 'lens-head' }, [el('div', { class: 'lens-kicker', text: kicker }), el('h1', { class: 'lens-title', text: title }), sub ? el('div', { class: 'lens-sub', text: sub }) : null, extra || null]);
}
function viewFlux() {
  const frag = document.createDocumentFragment();
  const snaps = (state.history && state.history.snaps) || [];
  frag.append(lensHead('Pneuma · the breath of the winds over time', 'Flux'));
  const bodyEl = el('div', { class: 'lens-body' });
  if (snaps.length < 2) { bodyEl.append(el('div', { class: 'empty', text: 'Gathering history… the flux appears after a few snapshots (every 5s).' })); frag.append(bodyEl); return frag; }
  bodyEl.append(fluxControls(snaps));
  bodyEl.append(fluxChart(snaps));
  bodyEl.append(seriesLegend(collectSeries(windowedSnaps(snaps))));
  frag.append(bodyEl); return frag;
}
function fluxControls(snaps) {
  const wrap = el('div', { class: 'flux-controls' });
  for (const [label, ms] of [['1m', 60e3], ['5m', 300e3], ['15m', 900e3], ['all', null]]) {
    const active = (state.series.rangeMs == null && ms == null) || state.series.rangeMs === ms;
    wrap.append(el('button', { class: 'mini-btn' + (active ? ' active' : ''), text: label, on: { click: () => { state.series.rangeMs = ms; state.series.endT = null; render(); } } }));
  }
  wrap.append(el('span', { style: 'flex:1' }));
  for (const [label, dh] of [['taller', 8], ['shorter', -8]]) wrap.append(el('button', { class: 'mini-btn', text: label, on: { click: () => { const cur = parseInt(getComputedStyle(document.documentElement).getPropertyValue('--chart-h')) || 68; document.documentElement.style.setProperty('--chart-h', clamp(cur + dh, 36, 200) + 'vh'); } } }));
  wrap.append(el('button', { class: 'mini-btn', text: '←', on: { click: () => panSeries(snaps, -1) } }), el('button', { class: 'mini-btn', text: '→', on: { click: () => panSeries(snaps, 1) } }), el('button', { class: 'mini-btn', text: 'reset', on: { click: () => { state.series.endT = null; render(); } } }));
  return wrap;
}
function fluxChart(snaps) {
  const visible = windowedSnaps(snaps), series = collectSeries(visible);
  const flux = el('div', { class: 'flux-wrap' });
  if (!series.length) { flux.append(el('div', { class: 'empty', text: 'No numeric telemetry in range.' })); return flux; }
  const active = series.filter(se => !state.series.hidden[se.id]);
  const W = 1200, H = 560, padL = 60, padR = 120, padT = 24, padB = 40;
  const plotL = padL, plotR = W - padR, plotT = padT, plotB = H - padB, plotW = plotR - plotL, plotH = plotB - plotT;
  const tip = el('div', { class: 'chart-tip hidden' });
  const s = svg('svg', { class: 'chart', viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'none' });
  const t0 = visible[0].t, t1 = visible[visible.length - 1].t, tspan = (t1 - t0) || 1;
  const X = (t) => plotL + (t - t0) / tspan * plotW;
  const domain = axisDomains(active);
  const Y = (field, v) => { const d = domain[field] || [0, 1]; return plotB - (v - d[0]) / (d[1] - d[0]) * plotH; };
  s.append(svg('rect', { x: plotL, y: plotT, width: plotW, height: plotH, class: 'plot-bg' }));
  drawGridAndAxes(s, domain, { W, H, plotL, plotR, plotT, plotB, plotW, plotH, rangeMs: t1 - t0, liveEdge: state.series.endT == null || t1 >= snaps[snaps.length - 1].t });
  for (const se of active) { let d = '', pen = false; se.ys.forEach((v, k) => { if (v == null) { pen = false; return; } d += (pen ? 'L' : 'M') + X(visible[k].t).toFixed(1) + ' ' + Y(se.field, v).toFixed(1) + ' '; pen = true; }); if (d) s.append(svg('path', { d, class: 'series-line', fill: 'none', stroke: se.color, 'stroke-width': '1.8', 'stroke-linejoin': 'round', 'stroke-linecap': 'round', 'stroke-dasharray': se.dash || '' })); }
  const hover = svg('g', { visibility: 'hidden' }); const hoverLine = svg('line', { x1: plotL, y1: plotT, x2: plotL, y2: plotB, class: 'hover-line' }); const hoverDots = svg('g', {}); hover.append(hoverLine, hoverDots); s.append(hover);
  const overlay = svg('rect', { x: plotL, y: plotT, width: plotW, height: plotH, fill: 'transparent', 'pointer-events': 'all' });
  overlay.addEventListener('mousemove', ev => {
    const rect = s.getBoundingClientRect(); const vx = (ev.clientX - rect.left) * W / rect.width;
    const idx = clamp(Math.round((vx - plotL) / plotW * (visible.length - 1)), 0, visible.length - 1); const snap = visible[idx], x = X(snap.t);
    hover.setAttribute('visibility', 'visible'); hoverLine.setAttribute('x1', x); hoverLine.setAttribute('x2', x); hoverDots.textContent = '';
    const rows = []; for (const se of active) { const v = se.ys[idx]; if (v == null) continue; hoverDots.append(svg('circle', { cx: x, cy: Y(se.field, v), r: 3.5, fill: se.color, stroke: 'var(--bg0)', 'stroke-width': 1.5 })); rows.push(`<div><i style="background:${se.color}"></i>${escapeHtml(se.label)} <b>${fmt(v, se.field === 'rpm' ? 0 : 1)}${escapeHtml(se.unit)}</b></div>`); }
    const age = Math.round((visible[visible.length - 1].t - snap.t) / 1000);
    tip.innerHTML = `<strong>${age ? '-' + age + 's' : 'now'}</strong>${rows.join('') || '<div>No values</div>'}`;
    const prect = flux.getBoundingClientRect(); tip.style.left = Math.min(Math.max(ev.clientX - prect.left + 14, 8), Math.max(8, prect.width - 280)) + 'px'; tip.style.top = Math.max(ev.clientY - prect.top - 18, 8) + 'px'; tip.classList.remove('hidden');
  });
  overlay.addEventListener('mouseleave', () => { hover.setAttribute('visibility', 'hidden'); tip.classList.add('hidden'); });
  overlay.addEventListener('wheel', ev => { ev.preventDefault(); zoomSeries(snaps, ev.deltaY < 0 ? 0.8 : 1.25); }, { passive: false });
  s.append(overlay); flux.append(s, tip); return flux;
}
function windowedSnaps(snaps) {
  if (!snaps.length) return []; const first = snaps[0].t, latest = snaps[snaps.length - 1].t, total = Math.max(latest - first, 1);
  let range = state.series.rangeMs == null ? total : Math.min(state.series.rangeMs, total);
  let end = state.series.endT == null ? latest : clamp(state.series.endT, first + range, latest);
  if (state.series.rangeMs == null) { end = latest; range = total; }
  const out = snaps.filter(sn => sn.t >= end - range && sn.t <= end); return out.length >= 2 ? out : snaps.slice(-2);
}
function collectSeries(snaps) {
  const keys = Array.from(new Set(snaps.flatMap(sn => (sn.instances || []).map(i => i.key)))).sort();
  const bySnap = snaps.map(sn => { const m = new Map(); for (const i of (sn.instances || [])) m.set(i.key, i); return m; });
  const nameByKey = {}; for (const sn of snaps) for (const i of (sn.instances || [])) if (i.name) nameByKey[i.key] = i.name;
  const out = [];
  keys.forEach((key, keyIdx) => SERIES_FIELDS.forEach((meta, fieldIdx) => { const ys = bySnap.map(m => { const v = m.get(key)?.[meta.field]; return v == null || !Number.isFinite(Number(v)) ? null : Number(v); }); if (!ys.some(v => v != null)) return; out.push({ id: key + '|' + meta.field, key, field: meta.field, label: `${nameByKey[key] || key} · ${meta.label}`, unit: meta.unit, color: PALETTE[(keyIdx * SERIES_FIELDS.length + fieldIdx) % PALETTE.length], dash: meta.dash, ys }); }));
  return out;
}
function axisDomains(series) {
  const domains = {};
  for (const meta of SERIES_FIELDS) { let lo = Infinity, hi = -Infinity; if (meta.fixed) { lo = meta.fixed[0]; hi = meta.fixed[1]; } if (meta.field === 'rpm') lo = 0; for (const se of series) if (se.field === meta.field) for (const v of se.ys) if (v != null) { lo = Math.min(lo, v); hi = Math.max(hi, v); } if (!isFinite(lo) || !isFinite(hi)) { lo = 0; hi = 1; } if (hi - lo < 1) hi = lo + 1; if (!meta.fixed) { const pad = (hi - lo) * 0.08; lo = Math.max(meta.field === 'rpm' ? 0 : -Infinity, lo - pad); hi += pad; } domains[meta.field] = [lo, hi]; }
  return domains;
}
function drawGridAndAxes(s, domain, box) {
  const { W, H, plotL, plotR, plotT, plotB, plotH, rangeMs, liveEdge } = box;
  const yFor = (f, v) => plotB - (v - domain[f][0]) / (domain[f][1] - domain[f][0]) * plotH;
  const ticks = (f) => Array.from({ length: 5 }, (_, g) => domain[f][0] + (domain[f][1] - domain[f][0]) * g / 4);
  for (const v of ticks('temp')) { const y = yFor('temp', v); s.append(svg('line', { x1: plotL, y1: y, x2: plotR, y2: y, class: 'axis' })); const tx = svg('text', { x: plotL - 7, y: y + 3, class: 'axis-txt axis-temp', 'text-anchor': 'end' }); tx.textContent = fmt(v, 0); s.append(tx); }
  for (const [field, x, cls, unit] of [['duty', plotR + 7, 'axis-duty', '%'], ['rpm', plotR + 60, 'axis-rpm', 'rpm']]) { s.append(svg('line', { x1: x - 5, y1: plotT, x2: x - 5, y2: plotB, class: 'axis ' + cls })); for (const v of ticks(field)) { const y = yFor(field, v); const tx = svg('text', { x, y: y + 3, class: 'axis-txt ' + cls, 'text-anchor': 'start' }); tx.textContent = fmt(v, 0); s.append(tx); } const lab = svg('text', { x, y: plotT - 8, class: 'axis-txt ' + cls, 'text-anchor': 'start' }); lab.textContent = unit; s.append(lab); }
  s.append(svg('line', { x1: plotL, y1: plotB, x2: plotR, y2: plotB, class: 'axis' }));
  const xl = svg('text', { x: plotL, y: H - 10, class: 'axis-txt', 'text-anchor': 'start' }); xl.textContent = '-' + Math.round((rangeMs || 0) / 1000) + 's';
  const xr = svg('text', { x: plotR, y: H - 10, class: 'axis-txt', 'text-anchor': 'end' }); xr.textContent = liveEdge ? 'now' : 'window';
  const lg = svg('text', { x: plotL, y: plotT - 8, class: 'axis-txt axis-temp', 'text-anchor': 'start' }); lg.textContent = '°C'; s.append(xl, xr, lg);
}
function seriesLegend(series) {
  const legend = el('div', { class: 'series-legend' });
  for (const se of series) { const hidden = !!state.series.hidden[se.id]; const b = el('button', { class: 'series-toggle' + (hidden ? ' muted' : ''), on: { click: () => { if (hidden) delete state.series.hidden[se.id]; else state.series.hidden[se.id] = true; render(); } } }); b.innerHTML = `<i style="background:${se.color}"></i><span>${escapeHtml(se.label)}</span>`; legend.append(b); }
  return legend;
}
function zoomSeries(snaps, factor) { const first = snaps[0].t, latest = snaps[snaps.length - 1].t, total = Math.max(latest - first, 30e3); const current = state.series.rangeMs == null ? total : state.series.rangeMs, next = clamp(current * factor, 30e3, total); state.series.rangeMs = next >= total * 0.98 ? null : Math.round(next); state.series.endT = state.series.endT == null ? latest : clamp(state.series.endT, first + next, latest); render(); }
function panSeries(snaps, dir) { const first = snaps[0].t, latest = snaps[snaps.length - 1].t, total = Math.max(latest - first, 1); if (state.series.rangeMs == null || state.series.rangeMs >= total) return; const range = state.series.rangeMs, current = state.series.endT == null ? latest : state.series.endT; state.series.endT = clamp(current + dir * range * 0.5, first + range, latest); render(); }

/* ===================== LAW (curves — only where one exists) ===================== */
// Units this module contributes to (by the assembled `sources` list).
function moduleUnits(module) { return buildUnits(state.status || {}).filter(u => (u.sources || []).includes(module)); }
function moduleControls(module) { return moduleUnits(module).some(u => u.components.some(c => c.sinks.length || c.publishers.some(p => /^driving/.test(p.kind)))); }
function viewLaw() {
  const frag = document.createDocumentFragment();
  frag.append(lensHead('Nomos · the law that binds heat to wind', 'Law'));
  const bodyEl = el('div', { class: 'lens-body' });
  const mm = modulesMap(state.status); const grid = el('div', { class: 'law-grid' }); let any = false;
  for (const name of Object.keys(mm).sort()) {
    if (!moduleControls(name)) continue;  // skip sensor-only modules (no curve)
    const op = moduleOperatingPoint(name); const card = el('div', { class: 'law-card' }); card.append(el('h3', { text: name }));
    const holder = el('div'); card.append(holder); grid.append(card); any = true;
    fetchCurve(name).then(c => renderCurve(holder, name, c, op));
  }
  bodyEl.append(any ? grid : el('div', { class: 'empty', text: 'No controlling modules — nothing has a curve.' }));
  frag.append(bodyEl); return frag;
}
function moduleOperatingPoint(module) {
  const us = moduleUnits(module);
  // The operating point now lives on the sinks' driving record (input temp → output duty).
  for (const u of us) for (const c of u.components) for (const s of (c.sinks || [])) { const d = s.driving; if (d && d.input != null) return { temp: d.raw != null ? d.raw : d.input, pct: d.output }; }
  let temp = null, pct = null; for (const u of us) { const a = unitCtx(u); if (a.temp != null) temp = Math.max(temp ?? -1e9, a.temp); if (a.duty != null) pct = Math.max(pct ?? -1e9, a.duty); } return { temp: temp === -1e9 ? null : temp, pct: pct === -1e9 ? null : pct };
}
async function fetchCurve(name) { if (state.curveCache[name] && Date.now() - state.curveCache[name]._t < 15000) return state.curveCache[name]; try { const c = await fetch('/curve.json?module=' + encodeURIComponent(name), { cache: 'no-store' }).then(r => r.json()); c._t = Date.now(); state.curveCache[name] = c; return c; } catch (e) { return { available: false, points: [] }; } }
let CURVE_SEQ = 0;
function renderCurve(holder, name, c, op) {
  holder.textContent = ''; const W = 480, H = 280, pad = 34, gid = 'cg' + (++CURVE_SEQ);
  const s = svg('svg', { class: 'curve-svg', viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'xMidYMid meet' });
  const defs = svg('defs'); const g = svg('linearGradient', { id: gid, x1: '0', y1: '0', x2: '0', y2: '1' }); g.append(svg('stop', { offset: '0', 'stop-color': 'var(--accent)', 'stop-opacity': '.35' })); g.append(svg('stop', { offset: '1', 'stop-color': 'var(--accent)', 'stop-opacity': '0' })); defs.append(g); s.append(defs);
  const tMin = 20, tMax = 100; const X = (t) => pad + (clamp(t, tMin, tMax) - tMin) / (tMax - tMin) * (W - 2 * pad); const Y = (p) => (H - pad) - clamp(p, 0, 100) / 100 * (H - 2 * pad);
  for (let p = 0; p <= 100; p += 25) { s.append(svg('line', { x1: pad, y1: Y(p), x2: W - pad, y2: Y(p), class: 'axis' })); const ty = svg('text', { x: pad - 6, y: Y(p) + 3, class: 'axis-txt', 'text-anchor': 'end' }); ty.textContent = p + '%'; s.append(ty); }
  for (let t = tMin; t <= tMax; t += 20) { const tx = svg('text', { x: X(t), y: H - pad + 14, class: 'axis-txt', 'text-anchor': 'middle' }); tx.textContent = t + '°'; s.append(tx); }
  if (c && c.available && c.points && c.points.length) { const pts = c.points.slice().sort((a, b) => a[0] - b[0]); const ev = (t) => { if (t <= pts[0][0]) return pts[0][1]; if (t >= pts[pts.length - 1][0]) return pts[pts.length - 1][1]; for (let k = 1; k < pts.length; k++) if (t <= pts[k][0]) { const [t0, p0] = pts[k - 1], [t1, p1] = pts[k]; return t1 === t0 ? p0 : p0 + (t - t0) / (t1 - t0) * (p1 - p0); } return pts[pts.length - 1][1]; }; let d = ''; for (let t = tMin; t <= tMax; t += 1) d += (t === tMin ? 'M' : 'L') + X(t).toFixed(1) + ' ' + Y(ev(t)).toFixed(1) + ' '; s.append(svg('path', { d: d + `L${X(tMax)} ${Y(0)} L${X(tMin)} ${Y(0)} Z`, fill: `url(#${gid})` })); s.append(svg('path', { d, class: 'curve-line' })); for (const [t, p] of pts) s.append(svg('circle', { cx: X(t), cy: Y(p), r: 3, fill: 'var(--accent)' })); }
  if (op && op.temp != null && op.pct != null) { const px = X(op.temp), py = Y(op.pct); s.append(svg('line', { x1: px, y1: Y(0), x2: px, y2: py, stroke: 'var(--aether)', 'stroke-width': '1', 'stroke-dasharray': '3 3', opacity: '0.5' })); const ring = svg('circle', { cx: px, cy: py, r: 9, class: 'curve-op-ring' }); ring.append(svg('animate', { attributeName: 'r', values: '7;12;7', dur: '2.4s', repeatCount: 'indefinite' })); ring.append(svg('animate', { attributeName: 'opacity', values: '0.7;0.1;0.7', dur: '2.4s', repeatCount: 'indefinite' })); s.append(ring); s.append(svg('circle', { cx: px, cy: py, r: 5, class: 'curve-op' })); }
  holder.append(s);
  let note; if (!c || !c.available) note = 'Curve config not readable here; live operating point only.'; else if (op && op.temp != null && op.pct != null) note = `Operating at ${fmt(op.temp)}°C → ${fmt(op.pct)}% duty` + (c.sensitivity != null ? ` · α=${c.sensitivity}` : ''); else note = 'Curve loaded; awaiting a live operating point.';
  holder.append(el('div', { class: 'curve-note', text: note }));
}

/* ===================== PULSE (health + logs) ===================== */
function viewPulse() {
  const frag = document.createDocumentFragment(); const s = state.status;
  frag.append(lensHead('Hygeia · the wellbeing of the anemoi', 'Pulse'));
  const bodyEl = el('div', { class: 'lens-body' });
  const roster = el('div', { class: 'pulse-roster' });
  for (const i of (s.instances || [])) { const ok = i.status === 'ok'; roster.append(el('div', { class: 'pulse-row' }, [el('div', { class: 'pn' }, [i.name || i.id, el('small', { text: i.module })]), el('span', { class: 'status-pill ' + (ok ? 'ok' : 'bad'), text: i.status }), el('span', { class: 'meta', text: (i.restart_count ? i.restart_count + ' restarts' : 'stable') }), el('span', { class: 'meta', text: i.seconds_since_seen + 's ago' })])); }
  for (const m of (s.modules || [])) if (m.detect_status !== 'ok') roster.append(el('div', { class: 'pulse-row' }, [el('div', { class: 'pn' }, [m.module, el('small', { text: 'detect' })]), el('span', { class: 'status-pill bad', text: m.detect_status }), el('span', { class: 'meta', text: m.detect_error || '' }), el('span', {})]));
  if (!(s.instances || []).length) roster.append(el('div', { class: 'empty', text: 'No instances yet.' }));
  bodyEl.append(roster);
  const logs = el('div', { class: 'logs' }); logs.append(el('h3', { text: 'Logs' }));
  const stream = el('div', { class: 'logstream' }); let had = false;
  for (const i of (s.instances || [])) { const tail = (i.stderr_tail || []).filter(Boolean); if (!tail.length) continue; had = true; stream.append(el('div', { html: `<span class="lg-mod">${escapeHtml(i.module + ':' + i.id)}</span>` })); for (const ln of tail.slice(-12)) stream.append(el('div', { text: ln })); stream.append(el('div', { text: ' ' })); }
  if (!had) stream.append(el('div', { class: 'lg-empty', text: 'All quiet — the anemoi have nothing to report.' }));
  logs.append(stream); bodyEl.append(logs); frag.append(bodyEl); return frag;
}

/* ---------- wind backdrop (speed-driven; paused when hidden) ---------- */
const Wind = (() => {
  let svgEl, lines = [], raf = null, t = 0, w = 0, h = 0, running = false; const N = 22;
  function init() { svgEl = $('wind'); resize(); for (let k = 0; k < N; k++) { const p = svg('path', { class: 'wind-path' }); svgEl.append(p); lines.push({ el: p, y: Math.random(), phase: Math.random() * Math.PI * 2, amp: 0.4 + Math.random() * 0.8, speed: 0.5 + Math.random() }); } window.addEventListener('resize', resize); document.addEventListener('visibilitychange', () => document.hidden ? stop() : start()); start(); }
  function resize() { w = window.innerWidth; h = window.innerHeight; if (svgEl) svgEl.setAttribute('viewBox', `0 0 ${w} ${h}`); }
  function start() { if (running) return; running = true; loop(); }
  function stop() { running = false; if (raf) cancelAnimationFrame(raf); raf = null; }
  function loop() { if (!running) return; const p = state.pressure || 0; t += 0.006 + p * 0.045; svgEl.style.opacity = 0.7; lines.forEach((ln) => { const yy = ln.y * h, amp = (10 + p * 60) * ln.amp, segs = 8; let d = ''; for (let i = 0; i <= segs; i++) { const x = (i / segs) * w; const yv = yy + Math.sin(t * ln.speed + ln.phase + i * 0.6) * amp + Math.sin(t * 0.5 * ln.speed + i * 0.3) * amp * 0.4; d += (i ? 'L' : 'M') + x.toFixed(1) + ' ' + yv.toFixed(1) + ' '; } ln.el.setAttribute('d', d); ln.el.setAttribute('stroke-width', (1.2 * (0.5 + ln.amp)).toFixed(2)); ln.el.setAttribute('stroke-opacity', (0.26 * (0.4 + ln.amp * 0.6)).toFixed(2)); }); raf = requestAnimationFrame(loop); }
  return { init };
})();
function makeStars() { const host = $('stars'); if (!host) return; for (let k = 0; k < 50; k++) { const s = document.createElement('i'); s.style.left = (Math.random() * 100).toFixed(2) + '%'; s.style.top = (Math.random() * 100).toFixed(2) + '%'; s.style.animationDelay = (Math.random() * 6).toFixed(2) + 's'; host.append(s); } }

/* ---------- theme ---------- */
function initTheme() { const saved = localStorage.getItem('aiolos-theme') || 'dark'; document.documentElement.setAttribute('data-theme', saved); setThemeIcon(saved); $('theme').onclick = () => { const cur = document.documentElement.getAttribute('data-theme'), next = cur === 'dark' ? 'light' : 'dark'; document.documentElement.setAttribute('data-theme', next); localStorage.setItem('aiolos-theme', next); setThemeIcon(next); }; }
function setThemeIcon(t) { $('theme').textContent = t === 'dark' ? '☀' : '☾'; }

/* ---------- boot ---------- */
window.addEventListener('DOMContentLoaded', () => {
  initTheme(); applyHash();
  for (const b of document.querySelectorAll('.lens')) b.addEventListener('click', () => go(b.dataset.view));
  window.addEventListener('hashchange', () => { applyHash(); if (state.status) render(); });
  const reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (!reduce) { Wind.init(); makeStars(); } else { $('wind').style.display = 'none'; }
  poll();
});
