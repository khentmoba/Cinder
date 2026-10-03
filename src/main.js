// KhentUsage UI: aggregates the per-day rows from the Rust collector.
const invoke = () => (window.__TAURI__ ? window.__TAURI__.core.invoke('get_stats') : Promise.reject('not in Tauri'));

let DATA = null;
let metric = 'tokens';       // cost | tokens | limits
let rangeDays = 90;          // 1 | 7 | 30 | 90 | 0 = all
let breakMode = 'model';     // model | day
let selectedAgent = null;    // null = all agents

const AGENT_COLORS = {
  'Codex': '#e8e8ea', 'Pi': '#4da3ff', 'OpenCode': '#3fb950', 'Claude Code': '#d29922',
  'Antigravity': '#a371f7', 'T3': '#f0883e', 'Cursor': '#9aa0a6'
};
const color = a => AGENT_COLORS[a] || '#7d8590';

// Real product marks: simple-icons (OpenAI, Anthropic, Meta, DeepSeek, ...) in src/icons,
// plus the official OpenCode mark and the Antigravity icon extracted from the installed app.
const AGENT_LOGO = {
  'Codex': 'icons/openai.svg',
  'Pi': 'icons/pi.svg',
  'OpenCode': 'icons/opencode.svg',
  'Claude Code': 'icons/anthropic.svg',
  'Antigravity': 'icons/antigravity.png',
};
const agentLogo = a => AGENT_LOGO[a] || 'icons/pi.svg';

// Provider mark for a model string, from its provider prefix.
const PROVIDER_LOGO = [
  [/^(opencode|opencode-go|opencode-zen|opencode-go-responses|opencode-free-responses)\//, 'icons/opencode.svg'],
  [/^(openai|openai-codex|custom)\//, 'icons/openai.svg'],
  [/^anthropic\//, 'icons/anthropic.svg'],
  [/^antigravity\//, 'icons/antigravity.png'],
  [/^meta\//, 'icons/meta.svg'],
  [/^(deepseek)\//, 'icons/deepseek.svg'],
  [/^(xiaomi|mimo)\//, 'icons/xiaomi.svg'],
  [/^(moonshot|kimi)\//, 'icons/kimi.svg'],
  [/^(qwen|alibaba|dashscope)\//, 'icons/alibabacloud.svg'],
  [/^(mistral)\//, 'icons/mistralai.svg'],
];
// fallback on the model name itself when a log has no provider prefix
const NAME_LOGO = [
  [/muse|llama/, 'icons/meta.svg'],
  [/gemini|antigravity/, 'icons/antigravity.png'],
  [/deepseek/, 'icons/deepseek.svg'],
  [/gpt|codex|o3|o4/, 'icons/openai.svg'],
  [/claude/, 'icons/anthropic.svg'],
  [/mimo|xiaomi/, 'icons/xiaomi.svg'],
  [/kimi|moonshot/, 'icons/kimi.svg'],
  [/qwen|glm/, 'icons/alibabacloud.svg'],
  [/mistral/, 'icons/mistralai.svg'],
  [/space-bunny/, 'icons/opencode.svg'],
];
function providerLogo(model) {
  for (const [re, icon] of PROVIDER_LOGO) if (re.test(model)) return icon;
  for (const [re, icon] of NAME_LOGO) if (re.test(model)) return icon;
  return null;
}

// ---------- formatting ----------
function fmtTok(n) {
  if (n >= 1e9) return (n / 1e9).toFixed(2) + 'B';
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e7 ? 0 : 1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'K';
  return String(n);
}
function fmtAxis(n) {
  if (n >= 1e9) return (n / 1e9).toFixed(n % 1e9 === 0 ? 0 : 1) + 'B';
  if (n >= 1e6) return (n / 1e6).toFixed(0) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(0) + 'K';
  return String(n);
}
const fmtUsd = (n, known) => known ? '$' + (n >= 100 ? n.toFixed(0) : n.toFixed(2)) : '—';
const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

function todayStr(offsetDays = 0) {
  const d = new Date();
  d.setDate(d.getDate() + offsetDays);
  return d.toISOString().slice(0, 10);
}

// ---------- aggregation ----------
function filtered() {
  if (rangeDays === 0) return DATA.rows;
  const cutoff = todayStr(-(rangeDays - 1));
  return DATA.rows.filter(r => r.day >= cutoff);
}
function sessionCount() {
  if (rangeDays === 0) return DATA.sessions;
  const cutoff = todayStr(-(rangeDays - 1));
  return DATA.sessions.filter(s => s.day >= cutoff);
}

function blank() {
  return { requests: 0, input: 0, cache_read: 0, cache_write: 0, output: 0, reasoning: 0, tools: 0, cost: 0, cost_known: false, savings: 0 };
}
function acc(r, a) {
  a.requests += r.requests; a.input += r.input; a.cache_read += r.cache_read; a.cache_write += r.cache_write;
  a.output += r.output; a.reasoning += r.reasoning; a.tools += r.tools; a.cost += r.cost;
  a.cost_known = a.cost_known || r.cost_known; a.savings += r.savings;
  return a;
}
const tok = a => a.input + a.cache_read + a.cache_write + a.output;

function byAgent() {
  const m = new Map();
  for (const r of filtered()) {
    if (selectedAgent && r.agent !== selectedAgent) continue;
    if (!m.has(r.agent)) m.set(r.agent, { ...blank(), sessions: 0, day: new Map() });
    acc(r, m.get(r.agent));
    const d = m.get(r.agent).day;
    if (!d.has(r.day)) d.set(r.day, blank());
    acc(r, d.get(r.day));
  }
  const sess = new Map();
  for (const s of sessionCount()) sess.set(s.agent, (sess.get(s.agent) || 0) + s.sessions);
  const list = [...m.entries()].map(([agent, a]) => ({ agent, ...a, sessions: sess.get(agent) || 0 }));
  list.sort((x, y) => (metric === 'cost' ? y.cost - x.cost : tok(y) - tok(x)));
  const grand = list.reduce((t, a) => { acc(a, t); t.sessions += a.sessions; return t; }, { ...blank(), sessions: 0 });
  return { list, grand };
}

function byModel() {
  const m = new Map();
  for (const r of filtered()) {
    if (selectedAgent && r.agent !== selectedAgent) continue;
    const key = r.model || '(tools only)';
    if (!m.has(key)) m.set(key, { ...blank(), agents: new Set(), pricedModels: new Set() });
    const a = m.get(key);
    acc(r, a);
    a.agents.add(r.agent);
    if (r.cost_known && (r.input + r.output + r.cache_read + r.cache_write) > 0) a.pricedModels.add(r.model);
  }
  const list = [...m.entries()].map(([model, a]) => ({ model, ...a }));
  const totalTok = list.reduce((t, a) => t + tok(a), 0);
  list.sort((x, y) => (metric === 'cost' ? y.cost - x.cost : tok(y) - tok(x)));
  return { list, totalTok };
}

function byDay() {
  const m = new Map();
  for (const r of filtered()) {
    if (selectedAgent && r.agent !== selectedAgent) continue;
    if (!m.has(r.day)) m.set(r.day, { ...blank(), sessions: 0 });
    acc(r, m.get(r.day));
  }
  for (const s of sessionCount()) if (m.has(s.day)) m.get(s.day).sessions += s.sessions;
  return [...m.entries()].map(([day, a]) => ({ day, ...a })).sort((a, b) => a.day.localeCompare(b.day));
}

function chartSeries() {
  const { list } = byAgent();
  return list.map(a => ({
    agent: a.agent,
    values: byDay().filter(d => (a.day.get(d.day))).map(d => metric === 'cost' ? d.cost : tok(d)),
  }));
}

// ---------- rendering ----------
function render() {
  renderLimits();
  const dash = document.getElementById('dash');
  const limitsView = document.getElementById('limitsView');
  const showLimits = metric === 'limits';
  dash.hidden = showLimits;
  limitsView.hidden = !showLimits;
  if (showLimits) { renderRangeLabel(); return; }

  const { list, grand } = byAgent();
  const bigVal = metric === 'cost' ? fmtUsd(grand.cost, grand.cost_known) : fmtTok(tok(grand));
  document.getElementById('bigTotal').textContent = bigVal;
  document.getElementById('bigSub').textContent = grand.sessions + ' sessions · ' + grand.requests.toLocaleString() + ' requests · ' + fmtTok(grand.tools) + ' tool calls';

  document.getElementById('agentList').innerHTML = list.map(a => {
    const t = tok(a);
    const share = tok(grand) ? (t / tok(grand) * 100).toFixed(1) : '0.0';
    const val = metric === 'cost' ? fmtUsd(a.cost, a.cost_known) : fmtTok(t);
    const costTxt = fmtUsd(a.cost, a.cost_known);
    return `<li class="${selectedAgent === a.agent ? 'sel' : ''}" data-agent="${esc(a.agent)}" style="box-shadow:inset 2px 0 0 ${color(a.agent)}">
      <span class="radio"></span>
      <img class="logo" src="${agentLogo(a.agent)}" alt="" onerror="this.style.visibility='hidden'">
      <span class="name">${esc(a.agent)} <span class="cnt">${a.sessions} sessions</span></span>
      <span class="val">${val}<span class="pct">${share}% of ${metric === 'cost' ? 'cost' : 'tokens'}${metric === 'cost' ? '' : ' • ' + costTxt}</span></span>
    </li>`;
  }).join('') || '<li class="dim">no data</li>';
  document.querySelectorAll('#agentList li[data-agent]').forEach(li =>
    li.addEventListener('click', () => { selectedAgent = selectedAgent === li.dataset.agent ? null : li.dataset.agent; render(); }));

  document.getElementById('totals').innerHTML = [
    ['Processed tokens', fmtTok(tok(grand))],
    ['Cached input', fmtTok(grand.cache_read)],
    ['Uncached input', fmtTok(grand.input)],
    ['Output', fmtTok(grand.output)],
    ['Cache savings', grand.savings > 0 ? '$' + grand.savings.toFixed(2) : '—'],
  ].map(([l, v]) => `<div class="t"><div class="l">${l}</div><div class="v">${v}</div></div>`).join('');

  const total = tok(grand) || 1;
  const segs = [['Input', grand.input, '#3f3f46'], ['Cache read', grand.cache_read, '#8b5cf6'],
    ['Cache write', grand.cache_write, '#c084fc'], ['Output', grand.output, '#e8e8ea']];
  document.getElementById('stacked').innerHTML = segs.map(([, v, c]) =>
    `<div style="width:${v / total * 100}%;background:${c}"></div>`).join('');
  document.getElementById('stackLegend').innerHTML = segs.map(([l, v, c]) =>
    `<span><i style="background:${c}"></i>${l} <b>${fmtTok(v)}</b></span>`).join('');

  renderBreakdown();
  renderChart(list);
  renderRangeLabel();
}

function renderBreakdown() {
  const el = document.getElementById('breakdown');
  if (breakMode === 'day') {
    const days = byDay();
    const total = days.reduce((t, d) => t + (metric === 'cost' ? d.cost : tok(d)), 0) || 1;
    el.innerHTML = `<tr><th>#</th><th>Day</th><th>Cost</th><th>Share</th><th>${metric === 'cost' ? 'Requests' : 'Tokens'}</th></tr>` +
      days.slice().reverse().map((d, i) => {
        const v = metric === 'cost' ? d.cost : tok(d);
        const last = metric === 'cost' ? d.requests.toLocaleString() : fmtTok(tok(d));
        const share = v / total * 100;
        const bar = share > 0.5 ? `<span class="bar" style="width:${Math.min(100, share * 4)}%"></span>` : '';
        return `<tr><td>${i + 1}</td><td>${d.day}${bar}</td><td class="${d.cost_known ? 'cost' : 'unpriced'}">${d.cost_known ? fmtUsd(d.cost, true) : 'Unpriced'}</td>` +
          `<td>${share < 0.1 ? '<0.1%' : share.toFixed(1) + '%'}</td><td>${last}</td></tr>`;
      }).join('');
    return;
  }
  const { list, totalTok } = byModel();
  const totalCost = list.reduce((t, a) => t + a.cost, 0) || 1;
  el.innerHTML = `<tr><th>#</th><th>Model</th><th>Cost</th><th>Share</th><th>Tokens</th></tr>` +
    list.slice(0, 25).map((a, i) => {
      const priced = a.cost_known && (a.input + a.output + a.cache_read + a.cache_write) > 0;
      const shareBase = metric === 'cost' ? a.cost : tok(a);
      const totalBase = metric === 'cost' ? totalCost : totalTok;
      const share = shareBase / totalBase * 100;
      const bar = share > 0.5 ? `<span class="bar" style="width:${Math.min(100, share * 4)}%"></span>` : '';
      const logo = providerLogo(a.model);
      return `<tr><td>${i + 1}</td><td class="modelcell">${logo ? `<img class="logo sm" src="${logo}" alt="" onerror="this.remove()">` : ''}${esc(a.model)}${bar}</td>` +
        `<td class="${priced ? 'cost' : 'unpriced'}">${priced ? fmtUsd(a.cost, true) : 'Unpriced'}</td>` +
        `<td>${share < 0.1 ? '<0.1%' : share.toFixed(1) + '%'}</td><td>${fmtTok(tok(a))}</td></tr>`;
    }).join('');
}

function renderRangeLabel() {
  const days = byDay();
  if (!days.length) { document.getElementById('rangeLabel').textContent = ''; return; }
  const fmtD = d => new Date(d + 'T00:00:00').toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  document.getElementById('rangeLabel').textContent = fmtD(days[0].day) + ' to ' + fmtD(days[days.length - 1].day);
}

function renderChart(list) {
  const box = document.getElementById('chart');
  const days = byDay();
  document.getElementById('chartTitle').textContent = 'Daily ' + (metric === 'cost' ? 'cost' : 'processed tokens');
  if (!days.length) { box.innerHTML = ''; return; }
  const series = list.map(a => ({ agent: a.agent, values: days.map(d => (metric === 'cost' ? d.cost : tok(d))) }));
  const W = box.clientWidth || 800, H = box.clientHeight || 400;
  const padL = 46, padR = 8, padT = 8, padB = 22;
  const max = Math.max(1e-6, ...series.flatMap(s => s.values));
  const step = niceStep(max / 3);
  const top = Math.ceil(max / step) * step;
  const x = i => padL + (days.length === 1 ? (W - padL - padR) / 2 : i * (W - padL - padR) / (days.length - 1));
  const y = v => padT + (1 - v / top) * (H - padT - padB);

  let gl = '';
  for (let v = 0; v <= top + 1e-9; v += step) {
    gl += `<line class="gl" x1="${padL}" x2="${W - padR}" y1="${y(v)}" y2="${y(v)}"/>` +
      `<text class="yl" x="${padL - 8}" y="${y(v) + 3}" text-anchor="end">${fmtAxis(v)}</text>`;
  }
  let paths = '';
  // smallest series first so the biggest agent stays visible on top
  for (const s of [...series].reverse()) {
    const pts = s.values.map((v, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join('');
    paths += `<path class="series" d="${pts}" stroke="${color(s.agent)}" opacity="0.9"/>`;
  }
  const fmtDay = d => new Date(d + 'T00:00:00').toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  const labels = [0, Math.floor(days.length / 2), days.length - 1].filter((v, i, a) => a.indexOf(v) === i)
    .map(i => `<text class="xl" x="${x(i)}" y="${H - 6}" text-anchor="${i === 0 ? 'start' : i === days.length - 1 ? 'end' : 'middle'}">${fmtDay(days[i].day)}</text>`).join('');

  box.innerHTML = `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">${gl}${paths}${labels}</svg>`;
}

function niceStep(raw) {
  const mag = Math.pow(10, Math.floor(Math.log10(Math.max(raw, 1e-9))));
  const n = raw / mag;
  return (n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10) * mag;
}

function renderLimits() {
  const box = document.getElementById('limitsView');
  const limits = DATA.limits || [];
  if (!limits.length) {
    box.innerHTML = `<div class="limits"><h3 style="margin-top:0">Limits</h3>
      <p style="color:var(--dim)">No agent reported rate-limit usage in the scanned logs.<br>
      Only Codex writes plan limits into its session files (5h / weekly windows).</p></div>`;
    return;
  }
  const when = s => s ? new Date(s * 1000).toLocaleString() : '';
  box.innerHTML = `<div class="limits"><h3 style="margin-top:0">Rate limits</h3>` + limits.map(l => {
    const pct = Math.min(100, l.used_percent);
    return `<div class="lrow">
      <div class="lh"><span>${esc(l.agent)} · ${esc(l.window)} window${l.plan ? ' · ' + esc(l.plan) : ''}</span><span>${pct.toFixed(0)}%</span></div>
      <div class="track"><i style="width:${pct}%;background:${pct > 80 ? '#f85149' : pct > 50 ? '#d29922' : 'var(--accent)'}"></i></div>
      <div class="lm">${l.window_minutes} min window · resets ${when(l.resets_at)}</div>
    </div>`;
  }).join('') + `</div>`;
}

// ---------- wiring ----------
async function load() {
  const status = document.getElementById('status');
  status.textContent = 'scanning local agent logs…';
  try {
    DATA = await invoke();
    render();
    status.textContent = 'updated ' + new Date().toLocaleTimeString();
    const p = DATA.pricing;
    const adj = DATA.adjusted_tokens || 0;
    const notes = [];
    if (adj > 0) notes.push(`cache ratio re-applied to ${fmtTok(adj)} tokens (Codex proxy hides cache hits)`);
    if ((DATA.plan_routes || []).length) notes.push('plan routes (no per-token billing): ' + DATA.plan_routes.join(', '));
    if ((DATA.unpriced || []).length) notes.push('unpriced: ' + DATA.unpriced.slice(0, 3).join(', '));
    document.getElementById('priceinfo').textContent =
      `prices: ${p.local} local + ${p.embedded} ${p.source} · ` +
      DATA.sources.map(s => `${s.agent}: ${s.found ? s.files : 'not installed'}`).join(' · ') +
      (notes.length ? ' · ' + notes.join(' · ') : '');
  } catch (e) {
    status.textContent = 'error: ' + e;
  }
}

for (const [id, set] of [['metric', v => { metric = v; }], ['range', v => { rangeDays = v === '0' ? 0 : +v; }],
  ['breakMode', v => { breakMode = v; }]]) {
  document.getElementById(id).addEventListener('click', e => {
    const b = e.target.closest('button');
    if (!b) return;
    [...e.currentTarget.querySelectorAll('button')].forEach(x => x.classList.toggle('on', x === b));
    set(b.dataset.v);
    render();
  });
}
document.getElementById('refresh').addEventListener('click', load);
window.addEventListener('resize', () => DATA && renderChart(byAgent().list));
load();
