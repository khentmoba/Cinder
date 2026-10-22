// Cinder UI: aggregates the per-day rows from the Rust collector.
const invokeCmd = (cmd, args) => (window.__TAURI__ ? window.__TAURI__.core.invoke(cmd, args) : Promise.reject('not in Tauri'));
const invoke = () => invokeCmd('get_stats');

let DATA = null;
let LOADING = false;         // a scan is in flight
let metric = 'tokens';       // cost | tokens | requests | limits
let lastMetric = 'tokens';   // the metric to fall back to when leaving Limits
let rangeDays = 90;          // 1 | 7 | 30 | 90 | 0 = all
let breakMode = 'model';     // model | day | agent | cache | project
let selectedAgent = null;    // null = all agents
let selectedModel = null;    // null = all models
let selectedProject = null;  // null = all projects, '' = the runs without one
let hiddenAgents = new Set();// series switched off in the chart legend
let breakAll = false;        // breakdown lists every row, not just the top ones
let chart = null;            // last drawn chart geometry, for the hover tooltip

const AGENT_COLORS = {
  'Codex': '#e8e8ea', 'Pi': '#4da3ff', 'OpenCode': '#3fb950', 'Claude Code': '#d29922',
  'Antigravity': '#a371f7', 'T3': '#f0883e', 'DSH': '#f778ba', 'LM Studio': '#6e56cf', 'Cursor': '#9aa0a6'
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
  [/^(openai|openai-codex)\//i, 'icons/openai.svg'],
  [/^anthropic\//i, 'icons/anthropic.svg'],
  [/^(google|gemini)\//i, 'icons/googlegemini.svg'],
  [/^antigravity\//i, 'icons/antigravity.png'],
  [/^meta\//i, 'icons/meta.svg'],
  [/^(deepseek)\//i, 'icons/deepseek.svg'],
  [/^(xiaomi|mimo)\//i, 'icons/xiaomi.svg'],
  [/^(moonshot|kimi)\//i, 'icons/kimi.svg'],
  [/^(qwen|alibaba|dashscope)\//i, 'icons/alibabacloud.svg'],
  [/^(z-ai|zai|zhipu|zhipuai|bigmodel|chatglm|glm)\//i, 'icons/glm.svg'],
  [/^(mistral)\//i, 'icons/mistralai.svg'],
  [/^(minimax)\//i, 'icons/minimax.svg'],
  [/^(opencode|opencode-go|opencode-zen|opencode-go-responses|opencode-free-responses)\//i, 'icons/opencode.svg'],
];
// Model family mark, from keywords in the model name.
const NAME_LOGO = [
  [/muse|llama/i, 'icons/meta.svg'],
  [/gemini/i, 'icons/googlegemini.svg'],
  [/antigravity/i, 'icons/antigravity.png'],
  [/deepseek/i, 'icons/deepseek.svg'],
  [/gpt|codex|o3|o4/i, 'icons/openai.svg'],
  [/claude/i, 'icons/anthropic.svg'],
  [/mimo|xiaomi/i, 'icons/xiaomi.svg'],
  [/kimi|moonshot/i, 'icons/kimi.svg'],
  [/qwen/i, 'icons/alibabacloud.svg'],
  [/glm|chatglm|zhipu/i, 'icons/glm.svg'],
  [/mistral/i, 'icons/mistralai.svg'],
  [/minimax/i, 'icons/minimax.svg'],
  [/space-bunny/i, 'icons/opencode.svg'],
];
function providerLogo(model) {
  for (const [re, icon] of NAME_LOGO) if (re.test(model)) return icon;
  for (const [re, icon] of PROVIDER_LOGO) if (re.test(model)) return icon;
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
const fmtUsd = (n, known, partial = false) => known
  ? (partial ? '≥ ' : '') + '$' + n.toLocaleString('en-US', n >= 100
      ? { maximumFractionDigits: 0 } : { minimumFractionDigits: 2, maximumFractionDigits: 2 })
  : '—';
// fmtUsd drops the cents above a hundred, which is right for a headline
// figure but wrong for a plan price or a budget, where the exact amount is
// the whole point of the number.
const fmtUsdCents = n => '$' + Number(n || 0).toLocaleString('en-US',
  { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

// How the money behind a figure is actually paid for. Every dollar number on
// screen is an equivalent value priced at public list prices, never an invoice,
// so the badge says which reading applies.
const BILLING_MODES = ['subscription', 'api', 'local', 'unknown'];
const BILLING_BADGE = { subscription: 'Plan', api: 'API', local: 'Local' };
const BILLING_BADGE_CLASS = { subscription: 'badge-plan', api: 'badge-api', local: 'badge-local' };
const BILLING_BADGE_TITLE = {
  subscription: 'Covered by a flat-rate plan — the card is not charged per token',
  api: 'Billed per token through a provider API key',
  local: 'Local inference — no provider bill at all',
};
// What the hero says the big number means.
const BILLING_READING = {
  subscription: 'API-equivalent value — covered by your plan',
  api: 'Direct API spend',
  local: 'Local inference — zero API cost',
};
const billingMode = v => {
  const m = String(v || '').toLowerCase();
  return BILLING_MODES.includes(m) ? m : 'unknown';
};
// The badge that says how a figure is paid for. Unknown rows get none.
const billingBadge = mode => {
  const m = billingMode(mode);
  const label = BILLING_BADGE[m];
  if (!label) return '';
  return `<span class="badge ${BILLING_BADGE_CLASS[m]}" title="${esc(BILLING_BADGE_TITLE[m])}">${label}</span>`;
};
// The project name a run belongs to; runs without a working directory are
// grouped under one honest name instead of an empty row.
const projectLabel = project => project || 'No project';

// The day a date falls on, in this machine's own time.
//
// The collector files a row under the reader's own day, so the day the
// dashboard calls "today" has to be the same day, and not the UTC one.
const localDayStr = d => new Date(d.getTime() - d.getTimezoneOffset() * 60000).toISOString().slice(0, 10);

function todayStr(offsetDays = 0) {
  const d = new Date();
  d.setDate(d.getDate() + offsetDays);
  return localDayStr(d);
}

// ---------- aggregation ----------
const modelKey = r => r.model || '(tools only)';

// Aggregation over an arbitrary window, so the chart can compare the selected
// range against the one before it.
function rowsInRange(fromDay, toDay) {
  const rows = applyModelFilter(DATA.rows);
  return rows.filter(r => (!fromDay || r.day >= fromDay) && (!toDay || r.day <= toDay));
}

function filtered() {
  const cutoff = rangeDays === 0 ? null : todayStr(-(rangeDays - 1));
  return rowsInRange(cutoff, null);
}
// One place where the model and project breadcrumbs narrow every panel at once.
function applyModelFilter(rows) {
  let out = rows;
  if (selectedProject !== null) out = out.filter(r => (r.project || '') === selectedProject);
  if (selectedModel) out = out.filter(r => modelKey(r) === selectedModel);
  return out;
}
function sessionCount() {
  if (rangeDays === 0) return DATA.sessions;
  const cutoff = todayStr(-(rangeDays - 1));
  return DATA.sessions.filter(s => s.day >= cutoff);
}

function blank() {
  return { requests: 0, input: 0, cache_read: 0, cache_write: 0, output: 0, reasoning: 0, tools: 0,
    cost: 0, cost_known: false, cost_partial: false, savings: 0,
    project: '', billing: 'unknown', subagent: false, subagent_requests: 0,
    cloud_savings: 0, local_savings: 0 };
}
// One reading for a whole total: the billing mode that covers the most money
// wins, where the money is the equivalent list value a row contributes to the
// big number, so the reading always describes how that number is made up. A
// local run costs nobody anything, so it weighs nothing here and says so on
// the "saved vs cloud" line instead; when every mode prices at zero, the mode
// with the most requests breaks the tie.
function dominantBilling(a) {
  const cost = (a && a.billingCost) || {};
  const req = (a && a.billingRequests) || {};
  let best = 'unknown';
  let bestCost = -1;
  let bestReq = -1;
  for (const mode of BILLING_MODES) {
    const c = cost[mode] || 0;
    const r = req[mode] || 0;
    if (c > bestCost || (c === bestCost && r > bestReq)) {
      best = mode; bestCost = c; bestReq = r;
    }
  }
  if (bestCost <= 0 && bestReq <= 0) return 'unknown';
  return best;
}
function acc(r, a) {
  a.requests += r.requests; a.input += r.input; a.cache_read += r.cache_read; a.cache_write += r.cache_write;
  a.output += r.output; a.reasoning += r.reasoning; a.tools += r.tools; a.cost += r.cost;
  a.cost_known = a.cost_known || r.cost_known;
  a.cost_partial ||= !!r.cost_partial || (!r.cost_known && tok(r) > 0);
  a.savings += r.savings;
  a.cloud_savings += r.cloud_savings || 0;
  a.subagent = a.subagent || !!r.subagent;
  if (!a.billingCost) {
    a.billingCost = {};
    a.billingRequests = {};
    for (const mode of BILLING_MODES) { a.billingCost[mode] = 0; a.billingRequests[mode] = 0; }
  }
  if (r.billingCost) {
    // the source is an aggregate of its own: carry its split and counts over
    a.subagent_requests += r.subagent_requests || 0;
    a.local_savings += r.local_savings || 0;
    for (const mode of BILLING_MODES) {
      a.billingCost[mode] += r.billingCost[mode] || 0;
      a.billingRequests[mode] += r.billingRequests[mode] || 0;
    }
  } else {
    a.subagent_requests += r.subagent ? (r.requests || 0) : 0;
    const mode = billingMode(r.billing);
    // the weight is the equivalent value the row contributes to the big number,
    // so the dominant mode always describes how that number is composed
    a.billingCost[mode] += r.cost || 0;
    a.billingRequests[mode] += r.requests || 0;
    if (mode === 'local') a.local_savings += r.cloud_savings || 0;
  }
  a.billing = dominantBilling(a);
  return a;
}
const tok = a => a.input + a.cache_read + a.cache_write + a.output;

// One accessor per metric, so every panel (hero, agent list, chart, breakdown,
// tooltip) reads the same number. Older Antigravity IDE transcripts carry no
// token or cost field, so a tokens/cost-only UI renders those rows as zeros and
// a flat line pinned to the baseline: switch to the requests metric to see
// them. Newer CLI transcripts do report tokens and are priced like the rest.
const METRICS = {
  cost:     { label: 'cost',     get: a => a.cost,     fmt: (n, known, partial) => fmtUsd(n, known, partial) },
  tokens:   { label: 'tokens',   get: tok,             fmt: n => fmtTok(n) },
  requests: { label: 'requests', get: a => a.requests, fmt: n => n.toLocaleString() },
};
const mval = a => METRICS[metric].get(a);
const mfmt = (n, known, partial) => METRICS[metric].fmt(n, known, partial);

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
  list.sort((x, y) => mval(y) - mval(x));
  const grand = list.reduce((t, a) => { acc(a, t); t.sessions += a.sessions; return t; }, { ...blank(), sessions: 0 });
  return { list, grand };
}

function byModel() {
  const m = new Map();
  for (const r of filtered()) {
    if (selectedAgent && r.agent !== selectedAgent) continue;
    const key = r.model || '(tools only)';
    if (!m.has(key)) m.set(key, blank());
    acc(r, m.get(key));
  }
  const list = [...m.entries()].map(([model, a]) => ({ model, ...a }));
  const totalTok = list.reduce((t, a) => t + tok(a), 0);
  list.sort((x, y) => mval(y) - mval(x));
  return { list, totalTok };
}

// Grouped by the repository the run happened in. Runs without a working
// directory land in one group of their own.
function byProject() {
  const m = new Map();
  const agents = new Map();
  for (const r of filtered()) {
    if (selectedAgent && r.agent !== selectedAgent) continue;
    const key = r.project || '';
    if (!m.has(key)) m.set(key, blank());
    acc(r, m.get(key));
    if (!agents.has(key)) agents.set(key, new Set());
    agents.get(key).add(r.agent);
  }
  const list = [...m.entries()].map(([project, a]) => ({ ...a, project, agentCount: agents.get(project).size }));
  list.sort((x, y) => mval(y) - mval(x));
  return { list };
}

function byDay(rows = filtered()) {
  const m = new Map();
  for (const r of rows) {
    if (selectedAgent && r.agent !== selectedAgent) continue;
    if (!m.has(r.day)) m.set(r.day, { ...blank(), sessions: 0 });
    acc(r, m.get(r.day));
  }
  for (const s of sessionCount()) if (m.has(s.day)) m.get(s.day).sessions += s.sessions;
  return [...m.entries()].map(([day, a]) => ({ day, ...a })).sort((a, b) => a.day.localeCompare(b.day));
}

// the same range, shifted back one window — only for the fixed presets
function previousDays() {
  if (rangeDays === 0) return [];
  return byDay(rowsInRange(todayStr(-(rangeDays * 2 - 1)), todayStr(-rangeDays)));
}

// ---------- rendering ----------
// Startup scan: fill the real panels with shimmering stand-ins that mirror the
// final layout, so a cold start reads as loading instead of an empty window.
// Same element ids, so render() simply overwrites them once the scan lands.
function renderSkeleton() {
  const sk = c => `<i class="sk ${c}"></i>`;
  document.querySelector('.hero').innerHTML =
    `<div class="big-row"><div id="bigTotal" class="big sk h1"></div><span id="bigBadge" class="badge" hidden></span></div>` +
    `<div id="bigSub" class="sub sk w3"></div><div id="bigDelta" class="delta"></div><div id="bigSaved" class="saved" hidden></div>`;
  document.getElementById('agentList').innerHTML = [0, 1, 2, 3].map(() =>
    `<div class="agent-row">${sk('dot')}${sk('sq')}${sk('w1')}${sk('w2')}</div>`).join('');
  document.getElementById('totals').innerHTML = [0, 1, 2, 3, 4].map(() =>
    `<div class="t">${sk('l w3')}${sk('v w4')}</div>`).join('');
  document.getElementById('breakdown').innerHTML =
    `<div class="bd-head"><span>#</span><span></span><span>Model</span><span>Cost</span><span>Tokens</span></div>` +
    [0, 1, 2, 3, 4, 5, 6].map(i =>
      `<div class="bd-row"><span class="rank">${i + 1}</span><span class="cell-logo">${sk('sq')}</span><span class="bd-name">${sk('w5')}</span><span class="bd-cost">${sk('w3')}</span><span class="bd-num">${sk('w4')}</span></div>`).join('');
}

// Change against the window right before this one.
function renderDelta() {
  const el = document.getElementById('bigDelta');
  if (!el) return;
  el.textContent = '';
  if (rangeDays === 0) return;              // all time: nothing to compare against
  const cur = byDay();
  const prev = previousDays();
  if (!prev.length) return;
  if (metric === 'cost' && (prev.some(d => d.cost_partial) || !prev.every(d => d.cost_known))) return;
  const curTotal = cur.reduce((t, d) => t + mval(d), 0);
  const prevTotal = prev.reduce((t, d) => t + mval(d), 0);
  if (!prevTotal || !curTotal) return;
  const pct = (curTotal - prevTotal) / prevTotal * 100;
  const money = metric === 'cost' ? ` (${fmtUsd(prevTotal, true)})` : '';
  el.innerHTML = `<span class="${pct >= 0 ? 'up' : 'down'}">${pct >= 0 ? '▲' : '▼'} ${Math.abs(pct) >= 100 ? Math.abs(pct).toFixed(0) : Math.abs(pct).toFixed(1)}%</span>` +
    ` vs previous ${rangeDays === 1 ? 'day' : rangeDays + ' days'}${money}`;
}

function render() {
  if (!DATA) return;          // still scanning: nothing to draw yet
  renderLimits();
  renderAlertBadge();
  renderSources();
  renderCrumb();
  const dash = document.getElementById('dash');
  const limitsView = document.getElementById('limitsView');
  const showLimits = metric === 'limits';
  dash.hidden = showLimits;
  limitsView.hidden = !showLimits;
  // Limits is a view, not a metric: the metric and range controls have no
  // meaning there, so they leave the header.
  document.getElementById('metric').hidden = showLimits;
  document.getElementById('range').hidden = showLimits;
  document.getElementById('view').querySelectorAll('button').forEach(b =>
    b.classList.toggle('on', (b.dataset.v === 'limits') === showLimits));
  if (showLimits) {
    // the countdown ages while the view sits open; tick it, but only here
    if (window.setInterval && !limitsTimer) limitsTimer = window.setInterval(tickLimits, 30000);
    return;
  }
  if (limitsTimer) { window.clearInterval(limitsTimer); limitsTimer = null; }

  const { list, grand } = byAgent();
  const bigVal = mfmt(mval(grand), grand.cost_known, grand.cost_partial);
  // reset the class too: the first render may land on skeleton placeholders
  const big = document.getElementById('bigTotal');
  big.className = 'big';
  big.textContent = bigVal;
  const mode = billingMode(grand.billing);
  const heroBadge = document.getElementById('bigBadge');
  if (heroBadge) {
    const label = BILLING_BADGE[mode];
    heroBadge.textContent = label || '';
    heroBadge.className = label ? 'badge ' + BILLING_BADGE_CLASS[mode] : 'badge';
    heroBadge.title = label ? BILLING_BADGE_TITLE[mode] : '';
    heroBadge.hidden = !(metric === 'cost' && label);
  }
  const sub = document.getElementById('bigSub');
  sub.className = 'sub';
  // the subtitle names the reading of the total: the cost figure is an
  // equivalent value, so it says which way it is paid for.
  const reading = metric === 'cost' ? BILLING_READING[mode] : '';
  const parts = [];
  if (reading) parts.push(reading);
  parts.push(grand.sessions + ' sessions', grand.requests.toLocaleString() + ' requests', fmtTok(grand.tools) + ' tool calls');
  sub.textContent = parts.join(' · ');
  renderDelta();
  const savedEl = document.getElementById('bigSaved');
  if (savedEl) {
    const local = grand.local_savings || 0;
    savedEl.hidden = !(local > 0);
    savedEl.innerHTML = local > 0 ? `Saved vs cloud <b>${fmtUsd(local, true)}</b>` : '';
  }

  const grandVal = mval(grand);
  const agentList = document.getElementById('agentList');
  agentList.innerHTML = list.map(a => {
    const v = mval(a);
    const share = grandVal ? (v / grandVal * 100).toFixed(1) : '0.0';
    const badge = billingBadge(a.billing);
    // Cost is redundant when it is the selected metric; the request count never
    // is, and it is the only volume signal for agents that log no tokens.
    const extra = metric === 'cost' ? ''
      : ` · ${fmtUsd(a.cost, a.cost_known, a.cost_partial)}${badge} · ${a.requests.toLocaleString()} req`;
    // a delegated run is still real usage, so it only gets counted, never hidden
    const delegated = a.subagent_requests > 0 ? ` · ${a.subagent_requests.toLocaleString()} delegated` : '';
    const saved = a.local_savings > 0 ? ` · saved ${fmtUsd(a.local_savings, true)} vs cloud` : '';
    return `<button class="agent-row${selectedAgent === a.agent ? ' sel' : ''}" data-agent="${esc(a.agent)}"
      style="box-shadow:inset 2px 0 0 ${color(a.agent)}" title="Filter to ${esc(a.agent)}">
      <span class="radio"></span>
      <img class="logo" src="${agentLogo(a.agent)}" alt="" onerror="this.style.visibility='hidden'">
      <span class="name">${esc(a.agent)} <span class="cnt">${a.sessions} sessions</span></span>
      <span class="val">${mfmt(v, a.cost_known, a.cost_partial)}${metric === 'cost' ? badge : ''}</span>
      <span class="meta">${share}% of ${METRICS[metric].label}${extra}${delegated}${saved}</span>
      <span class="bar" style="width:${grandVal ? Math.max(1.5, v / grandVal * 100) : 0}%;background:${color(a.agent)}"></span>
    </button>`;
  }).join('') || (DATA.rows.length
    ? '<div class="empty">no usage in this range</div>'
    : '<div class="empty">No agent logs found yet. Cinder is watching the paths under Settings › Data Sources.</div>');
  agentList.querySelectorAll('.agent-row[data-agent]').forEach(btn =>
    btn.addEventListener('click', () => {
      selectedAgent = selectedAgent === btn.dataset.agent ? null : btn.dataset.agent;
      render();
    }));

  // Totals carry only what the hero and the token bar do not say: repeating the
  // same token count in three panels is what made this column unreadable.
  const cachedInput = grand.cache_read;
  const freshInput = grand.input + grand.cache_write;
  const hit = cachedInput + freshInput ? cachedInput / (cachedInput + freshInput) * 100 : 0;
  const hitColor = hit > 50 ? 'var(--good)' : hit > 20 ? 'var(--accent)' : 'inherit';
  const items = [
    ['Cache hit rate', hit ? `<span style="color:${hitColor}">${hit.toFixed(1)}%</span>` : '—'],
    ['Cache savings', grand.savings > 0 ? fmtUsd(grand.savings, true) : '—'],
  ];
  if (grand.cache_read > 0) items.push(['Prompt cached', fmtTok(grand.cache_read)]);
  if (metric !== 'cost') items.push(['Cost', fmtUsd(grand.cost, grand.cost_known, grand.cost_partial) + billingBadge(grand.billing)]);
  document.getElementById('totals').innerHTML = items.map(([l, v]) =>
    `<div class="t"><div class="l">${l}</div><div class="v">${v}</div></div>`).join('');

  const total = tok(grand) || 1;
  const segs = [['Input', grand.input, '#3f3f46'], ['Cache read', grand.cache_read, '#8b5cf6'],
    ['Cache write', grand.cache_write, '#c084fc'], ['Output', grand.output, '#e8e8ea']];
  document.getElementById('stacked').innerHTML = segs.map(([l, v, c]) => {
    const pct = (v / total * 100).toFixed(1);
    return `<div style="width:${v / total * 100}%;background:${c}" title="${l}: ${fmtTok(v)} (${pct}%)"></div>`;
  }).join('');
  document.getElementById('stackLegend').innerHTML = segs.map(([l, v, c]) => {
    const pct = (v / total * 100).toFixed(1);
    return `<span title="${l}: ${fmtTok(v)} (${pct}%)"><i style="background:${c}"></i>${l} <b>${fmtTok(v)}</b> <span class="legend-pct">(${pct}%)</span></span>`;
  }).join('');

  renderBreakdown();
  renderArtifacts();
  renderChart(list);
  renderRangeLabel();
}

function renderCrumb() {
  const crumb = document.getElementById('crumb');
  if (!crumb) return;
  const parts = [`<span>Usage</span>`];
  if (selectedAgent) {
    parts.push(`<span class="sep">/</span><button class="crumb-link" data-clear="agent">All environments</button>`);
    parts.push(`<span class="sep">/</span><b>${esc(selectedAgent)}</b>`);
  } else {
    parts.push(`<span class="sep">/</span><b>All environments</b>`);
  }
  if (selectedProject !== null) {
    parts.push(`<span class="sep">/</span><b>${esc(projectLabel(selectedProject))}</b>`);
    parts.push(`<button class="crumb-link" data-clear="project" title="Clear project filter">×</button>`);
  }
  if (selectedModel) {
    parts.push(`<span class="sep">/</span><b>${esc(selectedModel)}</b>`);
    parts.push(`<button class="crumb-link" data-clear="model" title="Clear model filter">×</button>`);
  }
  if (metric !== 'limits') parts.push(`<span class="range-label" id="rangeLabel"></span>`);
  crumb.innerHTML = parts.join(' ');
}

// Rows rather than a table: the share reads as a bar behind the label, and a
// row is a filter you can click.
function breakdownData() {
  if (breakMode === 'day') {
    const days = byDay().slice().reverse();
    const total = days.reduce((t, d) => t + mval(d), 0) || 1;
    return {
      label: 'Day',
      costHeader: 'Cost',
      num: metric === 'cost' ? 'Requests' : 'Tokens',
      rows: days.map(d => ({
        key: d.day, name: d.day, logo: null, share: mval(d) / total * 100, value: mval(d),
        cost: d.cost, costKnown: d.cost_known, costPartial: d.cost_partial, priced: d.cost_known,
        num: metric === 'cost' ? d.requests.toLocaleString() : fmtTok(tok(d)),
        savings: d.savings,
        billing: d.billing,
        subagentRequests: d.subagent_requests,
      })),
    };
  }
  if (breakMode === 'project') {
    const { list } = byProject();
    const total = list.reduce((t, p) => t + mval(p), 0) || 1;
    return {
      label: 'Project',
      costHeader: 'Cost',
      num: metric === 'requests' ? 'Requests' : 'Tokens',
      rows: list.map(p => ({
        key: p.project, name: projectLabel(p.project), logo: null, share: mval(p) / total * 100, value: mval(p),
        cost: p.cost, costKnown: p.cost_known, costPartial: p.cost_partial, priced: p.cost_known,
        num: metric === 'requests' ? p.requests.toLocaleString() : fmtTok(tok(p)),
        extra: `${p.agentCount} agent${p.agentCount === 1 ? '' : 's'}`,
        savings: p.savings,
        requests: p.requests,
        billing: p.billing,
        subagentRequests: p.subagent_requests,
        project: p.project,
      })),
    };
  }
  if (breakMode === 'agent') {
    const { list, grand } = byAgent();
    const grandVal = mval(grand) || 1;
    return {
      label: 'Agent',
      costHeader: 'Cost',
      num: metric === 'requests' ? 'Requests' : 'Tokens',
      rows: list.map(a => ({
        key: a.agent, name: a.agent, logo: agentLogo(a.agent), share: mval(a) / grandVal * 100, value: mval(a),
        cost: a.cost, costKnown: a.cost_known, costPartial: a.cost_partial, priced: a.cost_known,
        num: metric === 'requests' ? a.requests.toLocaleString() : fmtTok(tok(a)),
        extra: a.sessions ? `${a.sessions} sess` : '',
        savings: a.savings,
        requests: a.requests,
        billing: a.billing,
        subagentRequests: a.subagent_requests,
      })),
    };
  }
  if (breakMode === 'cache') {
    const { list } = byModel();
    const { grand } = byAgent();
    const grandSavings = grand.savings || 1;
    const sorted = [...list].sort((a, b) => (b.savings - a.savings) || (b.cache_read - a.cache_read));
    return {
      label: 'Model (Cache)',
      costHeader: 'Saved',
      num: 'Hit % · Read',
      rows: sorted.map(a => {
        const cached = a.cache_read;
        const fresh = a.input + a.cache_write;
        const hit = (cached + fresh > 0) ? (cached / (cached + fresh) * 100) : 0;
        return {
          key: a.model, name: a.model, logo: providerLogo(a.model),
          share: grand.savings > 0 ? (a.savings / grandSavings * 100) : hit,
          value: a.savings,
          cost: a.savings,
          costKnown: true,
          costPartial: false,
          priced: true,
          num: (hit > 0 ? hit.toFixed(1) + '%' : '0%') + ' · ' + fmtTok(a.cache_read),
          rawSavings: a.savings,
          hitRate: hit,
          cacheRead: a.cache_read,
        };
      }),
    };
  }
  const { list } = byModel();
  const totalCost = list.reduce((t, a) => t + a.cost, 0) || 1;
  const totalBase = (metric === 'cost' ? totalCost
    : metric === 'tokens' ? list.reduce((t, a) => t + tok(a), 0)
    : list.reduce((t, a) => t + a.requests, 0)) || 1;
  return {
    label: 'Model',
    costHeader: 'Cost',
    num: metric === 'requests' ? 'Requests' : 'Tokens',
    rows: list.map(a => ({
      key: a.model, name: a.model, logo: providerLogo(a.model), share: mval(a) / totalBase * 100, value: mval(a),
      cost: a.cost, costKnown: a.cost_known, costPartial: a.cost_partial,
      priced: a.cost_known && (a.input + a.output + a.cache_read + a.cache_write) > 0,
      num: metric === 'requests' ? a.requests.toLocaleString() : fmtTok(tok(a)),
      savings: a.savings,
      requests: a.requests,
      cacheRead: a.cache_read,
      billing: a.billing,
      subagentRequests: a.subagent_requests,
    })),
  };
}

function renderBreakdown() {
  const el = document.getElementById('breakdown');
  const { rows, label, costHeader = 'Cost', num } = breakdownData();
  const shown = breakAll ? rows : rows.slice(0, 10);
  const head = `<div class="bd-head"><span>#</span><span></span><span>${label}</span><span>${costHeader}</span><span>${num}</span></div>`;
  const body = shown.map((r, i) => {
    const cost = breakMode === 'cache'
      ? (r.rawSavings > 0 ? fmtUsd(r.cost, true) : (r.cacheRead > 0 ? '$0.00' : '—'))
      : (r.priced || r.costKnown ? fmtUsd(r.cost, true, r.costPartial) : 'Unpriced');
    const active = (breakMode === 'model' || breakMode === 'cache') ? (selectedModel === r.key)
      : (breakMode === 'agent') ? (selectedAgent === r.key)
      : (breakMode === 'project') ? (selectedProject === r.key) : false;
    const badge = breakMode === 'cache' ? '' : billingBadge(r.billing);
    // delegated runs are part of the row, so the row says so instead of
    // quietly mixing a child run into the parent session.
    const sub = r.subagentRequests > 0
      ? `<span class="badge badge-sub" title="${r.subagentRequests.toLocaleString()} delegated requests">subagent</span>` : '';
    const marker = (breakMode === 'model' || breakMode === 'cache') ? ' data-model="1"'
      : breakMode === 'agent' ? ' data-agent="1"'
      : breakMode === 'project' ? ' data-project="1"' : '';
    return `<button class="bd-row${active ? ' sel' : ''}" data-key="${esc(r.key)}"${marker}>
      <span class="share" style="width:${Math.min(100, r.share)}%"></span>
      <span class="rank">${i + 1}</span>
      <span class="cell-logo">${r.logo ? `<img class="logo sm" src="${r.logo}" alt="" onerror="this.remove()">` : ''}</span>
      <span class="bd-name">${esc(r.name)}${r.extra ? ` <span class="cnt">${esc(r.extra)}</span>` : ''}${sub}</span>
      <span class="bd-cost${(r.priced || r.costKnown || breakMode === 'cache') ? '' : ' unpriced'}">${cost}${badge}</span>
      <span class="bd-num">${r.num}</span>
    </button>`;
  }).join('');
  const more = (!breakAll && rows.length > shown.length)
    ? `<button class="bd-more" id="bdMore">Show all ${rows.length}</button>` : '';
  el.innerHTML = head + body + more;
  document.getElementById('bdMore')?.addEventListener('click', () => { breakAll = true; renderBreakdown(); });
  el.querySelectorAll('.bd-row').forEach(row => row.addEventListener('click', () => {
    selectBreakdownRow(row.dataset.key);
  }));
}

// A breakdown row is a filter: clicking it narrows the dashboard to that
// model, agent or project, clicking it again lets go.
function selectBreakdownRow(key) {
  if (breakMode === 'model' || breakMode === 'cache') {
    selectedModel = selectedModel === key ? null : key;
  } else if (breakMode === 'agent') {
    selectedAgent = selectedAgent === key ? null : key;
  } else if (breakMode === 'project') {
    selectedProject = selectedProject === key ? null : key;
  } else {
    return;
  }
  render();
}

// Data Sources reads the last scan, so "not installed" is truthful instead of
// a README copy.
function renderSources() {
  const el = document.getElementById('sourcesList');
  if (!el || !DATA) return;
  el.innerHTML = (DATA.sources || []).map(s => {
    // A source that could not be read says so: the numbers below it are the
    // other agents' numbers, and the user should know which one is missing.
    const failed = s.error ? ' failed' : '';
    const files = s.found ? (s.files ? s.files.toLocaleString() + ' files' : 'found') : 'not installed';
    // how long the scan of this source took, when the collector measured it
    const scan = s.scan_ms ? ' · ' + Math.round(s.scan_ms).toLocaleString() + ' ms' : '';
    const error = s.error ? `<div class="source-error">${esc(s.error)}</div>` : '';
    return `<div class="source-item${s.found ? '' : ' off'}${failed}">
    <div class="source-name"><span class="dot"></span><b>${esc(s.agent)}</b> <code>${esc(s.path)}</code></div>
    <div class="source-metrics">${files}${scan}</div>
    ${error}
  </div>`;
  }).join('') || '<div class="source-metrics">No sources reported by the last scan.</div>';
}

// Artifacts the agents published: a plain list of links, hidden when there
// are none so the dashboard does not carry an empty section.
function renderArtifacts() {
  const sec = document.getElementById('artifacts');
  if (!sec || !DATA) return;
  const list = DATA.artifacts || [];
  sec.hidden = !list.length;
  if (!list.length) return;
  document.getElementById('artifactList').innerHTML = list.map(a =>
    `<a class="artifact" href="${esc(a.url)}" target="_blank" rel="noopener noreferrer">
      <img class="logo sm" src="${agentLogo(a.agent)}" alt="" onerror="this.style.visibility='hidden'">
      <span class="atitle">${esc(a.title || a.url)}</span>
      <span class="ameta">${esc(a.agent)} · ${esc(a.day)}</span>
    </a>`).join('');
}

// the visible breakdown, as CSV
function breakdownCSV() {
  const { rows } = breakdownData();
  const quote = v => `"${String(v).replace(/"/g, '""')}"`;
  let headers = 'rank,name,cost_usd,tokens,share_percent,billing';
  let lines;
  if (breakMode === 'cache') {
    headers = 'rank,model,savings_usd,hit_rate_percent,cache_read_tokens';
    lines = rows.map((r, i) => [
      i + 1,
      quote(r.name),
      r.rawSavings ? r.rawSavings.toFixed(4) : '0.0000',
      r.hitRate ? r.hitRate.toFixed(2) : '0.00',
      r.cacheRead || 0
    ].join(','));
  } else if (breakMode === 'agent' || breakMode === 'project') {
    headers = breakMode === 'project'
      ? 'rank,project,cost_usd,metric_value,share_percent,cache_savings_usd,billing'
      : 'rank,agent,cost_usd,metric_value,share_percent,cache_savings_usd,billing';
    lines = rows.map((r, i) => [
      i + 1,
      quote(r.name),
      (r.priced || r.costKnown) ? r.cost.toFixed(4) : '',
      quote(r.num),
      r.share.toFixed(2),
      r.savings ? r.savings.toFixed(4) : '0.0000',
      billingMode(r.billing)
    ].join(','));
  } else {
    lines = rows.map((r, i) => [
      i + 1,
      quote(r.name),
      (r.priced || r.costKnown) ? r.cost.toFixed(4) : '',
      quote(r.num),
      r.share.toFixed(2),
      billingMode(r.billing)
    ].join(','));
  }
  return [headers].concat(lines).join('\n');
}

function exportCSV() {
  const csv = breakdownCSV();
  const url = URL.createObjectURL(new Blob([csv], { type: 'text/csv' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = `cinder-${breakMode}-${rangeDays || 'all'}-${todayStr()}.csv`;
  a.click();
  URL.revokeObjectURL(url);
}

function renderRangeLabel() {
  const days = byDay();
  if (!days.length) { document.getElementById('rangeLabel').textContent = ''; return; }
  const d1 = new Date(days[0].day + 'T00:00:00');
  const d2 = new Date(days[days.length - 1].day + 'T00:00:00');
  const diffYears = d1.getFullYear() !== d2.getFullYear();
  const fmtD = d => new Date(d + 'T00:00:00').toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    ...(diffYears ? { year: 'numeric' } : {})
  });
  const label = document.getElementById('rangeLabel');
  label.textContent = '· ' + fmtD(days[0].day) + ' to ' + fmtD(days[days.length - 1].day);
  label.title = 'Daily buckets use UTC dates';
}

// ---------- chart ----------
// A 90-day window of daily points is noise, so above six weeks the chart
// buckets into weeks.
const WEEK = 7;

function bucketDays(days, weekly) {
  if (!weekly) return days.map(d => ({ key: d.day, from: d.day, to: d.day, rows: [d] }));
  const out = [];
  for (let end = days.length; end > 0; end -= WEEK) {
    const rows = days.slice(Math.max(0, end - WEEK), end);
    out.unshift({ key: rows[0].day, from: rows[0].day, to: rows[rows.length - 1].day, rows });
  }
  return out;
}

function renderLegend(list) {
  const el = document.getElementById('chartLegend');
  if (!el) return;
  el.innerHTML = list.map(a => `<button class="lg-chip${hiddenAgents.has(a.agent) ? ' off' : ''}"
      data-agent="${esc(a.agent)}" title="Show or hide ${esc(a.agent)}"><i style="background:${color(a.agent)}"></i>${esc(a.agent)}</button>`).join('')
    + (chart?.hasPrev ? '<span class="lg-chip prev"><i></i>previous period</span>' : '');
  el.querySelectorAll('.lg-chip[data-agent]').forEach(b => b.addEventListener('click', () => {
    const agent = b.dataset.agent;
    if (hiddenAgents.has(agent)) hiddenAgents.delete(agent); else hiddenAgents.add(agent);
    renderChart(byAgent().list);
  }));
}

function renderChart(list) {
  const box = document.getElementById('chart');
  const allDays = byDay();
  const weekly = allDays.length > 42;
  const unit = metric === 'cost' ? 'cost (USD)' : metric === 'requests' ? 'requests' : 'processed tokens';
  document.getElementById('chartTitle').textContent = (weekly ? 'Weekly ' : 'Daily ') + unit;
  if (!allDays.length) { box.innerHTML = ''; chart = null; renderLegend([]); return; }

  const buckets = bucketDays(allDays, weekly);
  const prevDays = previousDays();
  const prevBuckets = prevDays.length ? bucketDays(prevDays, weekly) : [];
  const hasPrev = prevBuckets.length === buckets.length;
  const dayOf = (a, day) => a.day.get(day);

  const series = list.filter(a => !hiddenAgents.has(a.agent)).map(a => ({
    agent: a.agent,
    known: buckets.map(b => b.rows.every(d => dayOf(a, d.day)?.cost_known)),
    partial: buckets.map(b => b.rows.some(d => dayOf(a, d.day)?.cost_partial)),
    values: buckets.map(b => b.rows.reduce((t, d) => t + (dayOf(a, d.day) ? mval(dayOf(a, d.day)) : 0), 0)),
  }));
  const prevValues = hasPrev ? prevBuckets.map(b => b.rows.reduce((t, d) => t + mval(d), 0)) : [];

  const W = box.clientWidth || 800, H = box.clientHeight || 400;
  const padL = 52, padR = 10, padT = 10, padB = 24;
  const fmtDay = d => new Date(d + 'T00:00:00').toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  const max = Math.max(1e-6, ...series.flatMap(s => s.values), ...prevValues);
  const step = niceStep(max / 3);
  const top = Math.ceil(max / step) * step;
  const x = i => padL + (buckets.length === 1 ? (W - padL - padR) / 2 : i * (W - padL - padR) / (buckets.length - 1));
  const y = v => padT + (1 - v / top) * (H - padT - padB);

  let gl = '';
  for (let v = 0; v <= top + 1e-9; v += step) {
    gl += `<line class="gl" x1="${padL}" x2="${W - padR}" y1="${y(v)}" y2="${y(v)}"/>` +
      `<text class="yl" x="${padL - 8}" y="${y(v) + 4}" text-anchor="end">${fmtAxis(v)}</text>`;
  }
  const path = values => values.map((v, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join('');
  const prevPath = hasPrev ? `<path class="series prev" d="${path(prevValues)}"/>` : '';
  let paths = '';
  // smallest series first so the biggest agent stays visible on top
  for (const s of [...series].reverse()) {
    paths += `<path class="series" d="${path(s.values)}" stroke="${color(s.agent)}" opacity="0.9"/>`;
  }
  const tickCount = Math.min(6, Math.max(3, Math.floor(W / 170)));
  const ticks = [...new Set(Array.from({ length: tickCount }, (_, i) =>
    Math.round(i * (buckets.length - 1) / (tickCount - 1))))];
  const labels = ticks.map(i => `<text class="xl" x="${x(i)}" y="${H - 6}" text-anchor="${
    i === 0 ? 'start' : i === buckets.length - 1 ? 'end' : 'middle'}">${fmtDay(buckets[i].from)}</text>`).join('');

  box.innerHTML = `<svg viewBox="0 0 ${W} ${H}">${gl}${prevPath}${paths}${labels}
    <g class="hv" opacity="0"><line class="cross" y1="${padT}" y2="${H - padB}"/><g class="dots">${
      series.map(s => `<circle r="2.5" fill="${color(s.agent)}" stroke="#0a0a0c" stroke-width="1"/>`).join('')}</g></g>
    <rect x="${padL}" y="${padT}" width="${Math.max(0, W - padL - padR)}" height="${Math.max(0, H - padT - padB)}" fill="transparent"/></svg>
    <div class="tt" hidden></div>`;
  chart = {
    box, days: buckets, series, hasPrev, W, padL, padR, y, x,
    hv: box.querySelector('.hv'), cross: box.querySelector('.cross'),
    dots: [...box.querySelectorAll('.dots circle')], tt: box.querySelector('.tt'),
  };
  renderLegend(list);
}

// hover: crosshair on the nearest day + a day/agent/total tooltip
function chartHover(e) {
  if (!chart) return;
  const { box, days, series, x, y, W, padL, padR } = chart;
  const r = box.getBoundingClientRect();
  if (!r.width) return;
  const span = (W - padL - padR) / Math.max(1, days.length - 1);
  const i = Math.max(0, Math.min(days.length - 1,
    Math.round(((e.clientX - r.left) * (W / r.width) - padL) / span)));
  const px = x(i);
  chart.hv.setAttribute('opacity', '1');
  chart.cross.setAttribute('x1', px);
  chart.cross.setAttribute('x2', px);
  series.forEach((s, k) => {
    const dot = chart.dots[k], v = s.values[i];
    dot.setAttribute('cx', px);
    dot.setAttribute('cy', y(v));
    dot.setAttribute('opacity', v > 0 ? 1 : 0);
  });
  const tt = chart.tt;
  tt.hidden = false;
  tt.innerHTML = tipHTML(days, series, i);
  let left = e.clientX - r.left + 16;
  if (left + tt.offsetWidth > box.clientWidth - 4) left = e.clientX - r.left - tt.offsetWidth - 16;
  tt.style.left = Math.max(4, left) + 'px';
  tt.style.top = Math.max(4, Math.min(e.clientY - r.top - 8, box.clientHeight - tt.offsetHeight - 4)) + 'px';
}

function chartLeave() {
  if (!chart) return;
  chart.hv.setAttribute('opacity', '0');
  chart.tt.hidden = true;
}

function tipHTML(days, series, i) {
  const b = days[i];
  const from = b.from || b.day;
  const to = b.to || b.day;
  const fmt = d => new Date(d + 'T00:00:00').toLocaleDateString(undefined,
    { month: 'short', day: 'numeric', ...(from.slice(0, 4) !== to.slice(0, 4) ? { year: '2-digit' } : {}) });
  const head = from === to ? fmt(from) : `${fmt(from)} – ${fmt(to)}`;
  const rows = series.map(s => ({
    agent: s.agent, v: s.values[i],
    known: s.known[i], partial: s.partial[i],
    txt: mfmt(s.values[i], s.known[i], s.partial[i]),
  })).sort((a, b) => b.v - a.v);
  const total = rows.reduce((t, r) => t + r.v, 0);
  const totalTxt = mfmt(total, rows.some(r => r.known), rows.some(r => r.partial));
  return `<div class="d">${head}</div>` + rows.map(r =>
    `<div class="r"><img class="logo sm" src="${agentLogo(r.agent)}" alt="" onerror="this.style.visibility='hidden'">` +
    `<span class="n">${esc(r.agent)}</span><b>${r.txt}</b></div>`).join('') +
    `<div class="r tot"><span class="n">Total</span><b>${totalTxt}</b></div>`;
}

function niceStep(raw) {
  const mag = Math.pow(10, Math.floor(Math.log10(Math.max(raw, 1e-9))));
  const n = raw / mag;
  return (n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10) * mag;
}

// "43200 min window, resets 9/9/2026" is a puzzle. This is the same fact in the
// units a person plans around.
function windowLabel(minutes) {
  const days = minutes / 1440;
  if (days >= 1) return (Number.isInteger(days) ? days : days.toFixed(1)) + '-day window';
  const hours = minutes / 60;
  if (hours >= 1) return (Number.isInteger(hours) ? hours : hours.toFixed(1)) + '-hour window';
  return minutes + '-minute window';
}

// One ticking timer, alive only while the Limits view is on screen.
let limitsTimer = null;

function tickLimits() {
  if (metric !== 'limits' || !DATA) {
    window.clearInterval(limitsTimer);
    limitsTimer = null;
    return;
  }
  renderLimits();
}

function untilReset(seconds) {
  const left = seconds - Date.now() / 1000;
  if (left <= 0) return 'due';
  const hours = left / 3600;
  if (hours < 1) return 'in ' + Math.max(1, Math.round(left / 60)) + 'm';
  if (hours < 48) return 'in ' + (hours < 10 ? hours.toFixed(1) : Math.round(hours)) + 'h';
  return 'in ' + Math.round(hours / 24) + 'd';
}

// The same vocabulary the reset countdown uses, pointing back instead of
// forward: how old the live quota snapshot is.
function sinceAgo(iso) {
  const then = Date.parse(iso || '');
  if (!Number.isFinite(then)) return '';
  const secs = (Date.now() - then) / 1000;
  if (secs < 60) return 'just now';
  const mins = secs / 60;
  if (mins < 60) return Math.round(mins) + 'm ago';
  const hours = mins / 60;
  if (hours < 48) return (hours < 10 ? hours.toFixed(1) : Math.round(hours)) + 'h ago';
  return Math.round(hours / 24) + 'd ago';
}

// The things worth interrupting the user about, worst first. Nothing is drawn
// when there is nothing wrong: an empty section would only add noise.
function renderAlerts() {
  const alerts = (DATA && DATA.alerts) || [];
  if (!alerts.length) return '';
  return `<div class="alerts">` + alerts.map(a => {
    const critical = a.severity === 'critical';
    const colour = critical ? 'var(--bad)' : 'var(--warn)';
    return `<div class="alert-row ${critical ? 'crit' : 'warn'}">
      <div class="alert-title" style="color:${colour}">${esc(a.title || '')}</div>
      <div class="alert-detail">${esc(a.detail || '')}</div>
    </div>`;
  }).join('') + '</div>';
}

// The count of active alerts rides on the Limits button, so a quota problem
// is visible without opening the view.
function renderAlertBadge() {
  const n = ((DATA && DATA.alerts) || []).length;
  const btn = document.querySelector('#view button[data-v="limits"]');
  if (btn) {
    btn.innerHTML = n ? `Limits <span id="limitsAlertBadge" class="alert-badge">${n}</span>` : 'Limits';
    btn.title = n ? `${n} active alert${n === 1 ? '' : 's'}` : '';
  }
  const badge = document.getElementById('limitsAlertBadge');
  if (badge) {
    badge.textContent = n ? String(n) : '';
    badge.hidden = !n;
  }
}

// What the declared plans look like against this month's usage. A budget is
// a limit on spending and burns down, so it gets a bar. A plan is a flat
// rate the user pays: the work is worth more than the plan costs, and it
// must never be drawn as something that is being used up.
function renderPlanStatus() {
  const plans = (DATA && DATA.plans) || [];
  if (!plans.length) return '';
  const rows = plans.map(p => {
    const logo = `<img class="logo sm" src="${agentLogo(p.agent)}" alt="" onerror="this.style.visibility='hidden'">`;
    if (p.kind === 'budget') {
      const pct = Math.min(100, Math.max(0, p.percent || 0));
      const colour = pct > 80 ? 'var(--bad)' : pct > 50 ? 'var(--warn)' : 'var(--accent)';
      return `<div class="lrow plan-row budget">
        <div class="lh">
          <div class="lh-left">${logo}<span>${esc(p.agent)}</span>
            <span class="badge badge-budget" title="A limit on spending: it burns down as the month goes on">Budget</span>
          </div>
          <span class="pct" style="color:${colour}">${Math.round(pct)}%</span>
        </div>
        <div class="track"><i class="bar" style="width:${pct}%;background:${colour}"></i></div>
        <div class="lm"><span>${fmtUsdCents(p.used)} of your ${fmtUsdCents(p.limit)} budget (${Math.round(pct)}%)</span></div>
      </div>`;
    }
    const multiple = p.multiple || 0;
    const label = p.label || p.agent;
    return `<div class="lrow plan-row subscription">
      <div class="lh">
        <div class="lh-left">${logo}<span>${esc(p.agent)}</span>
          <span class="badge badge-plan" title="A flat rate you pay: the provider's quota window is the limit, not the price">Plan</span>
        </div>
        <span class="pct" style="color:var(--accent)">${multiple.toFixed(1)}x</span>
      </div>
      <div class="lm"><span>${esc(label)} · ${fmtUsdCents(p.limit)} a month · the work was worth ${fmtUsdCents(p.used)} (${multiple.toFixed(1)}x the plan)</span></div>
      <div class="plan-note">The plan price is what you pay each month. ${fmtUsdCents(p.used)} is what the work was worth at list prices, not an amount you pay.</div>
    </div>`;
  }).join('');
  return `<h3 style="margin-top:0">Plans &amp; budgets</h3>${rows}`;
}

function renderLimits() {
  const box = document.getElementById('limitsView');
  const limits = [...(DATA.limits || [])].sort((a, b) => b.used_percent - a.used_percent);
  const quotaError = DATA.quota_error || '';
  const snapshot = sinceAgo(DATA.quota_checked_at);
  // a provider that could not be read is worth saying out loud: the window
  // below is then the last thing the logs knew, not the current state.
  const quotaNote = quotaError
    ? `<p class="limits-note">Live quotas could not be read: ${esc(quotaError)}</p>` : '';
  // the alerts and the declared plans come first: they are what the user
  // came to this view for.
  const head = renderAlerts() + renderPlanStatus();
  const rateTitle = `<h3${head ? '' : ' style="margin-top:0"'}>Rate limits</h3>`;
  if (!limits.length) {
    box.innerHTML = `<div class="limits">${head}${rateTitle}${quotaNote}
      <p style="color:var(--dim)">No agent reported rate-limit usage in the scanned logs.<br>
      Only Codex writes plan limits into its session files (5h / weekly windows).</p></div>`;
    return;
  }
  box.innerHTML = `<div class="limits">${head}${rateTitle}${quotaNote}` + limits.map(l => {    const pct = Math.min(100, Math.max(0, l.used_percent));
    const colour = pct > 80 ? 'var(--bad)' : pct > 50 ? 'var(--warn)' : 'var(--accent)';

    // Burn-rate projection: how much of the window is gone vs how much of the
    // allowance is gone. Only worth stating once the window is under way.
    const span = (l.window_minutes || 0) * 60000;
    const now = Date.now();
    const resetMs = (l.resets_at || 0) * 1000;
    const elapsed = span ? Math.max(0, Math.min(1, 1 - (resetMs - now) / span)) : 0;
    const projected = elapsed > 0.15 && elapsed < 1 ? l.used_percent / elapsed : null;

    // Burn velocity (% / hr)
    const elapsedHours = (span * elapsed) / 3600000;
    const burnRatePerHour = elapsedHours > 0.1 && l.used_percent > 0 ? (l.used_percent / elapsedHours) : null;

    // Health / Pacing badge
    let badge = '';
    if (pct >= 90) {
      badge = `<span class="badge-pace pace-bad">Cap Imminent</span>`;
    } else if (projected !== null) {
      if (projected >= 100) {
        badge = `<span class="badge-pace pace-bad">Exhaustion Risk</span>`;
      } else if (projected > 75) {
        badge = `<span class="badge-pace pace-warn">Elevated</span>`;
      } else {
        badge = `<span class="badge-pace pace-good">On Pace</span>`;
      }
    } else if (pct > 50) {
      badge = `<span class="badge-pace pace-warn">Moderate</span>`;
    }

    // Depletion ETA calculation if projected >= 100
    let note = '';
    if (projected !== null) {
      if (projected >= 100) {
        if (burnRatePerHour && burnRatePerHour > 0) {
          const hoursLeft = (100 - pct) / burnRatePerHour;
          const eta = hoursLeft < 1
            ? `~${Math.max(1, Math.round(hoursLeft * 60))}m`
            : `~${hoursLeft < 10 ? hoursLeft.toFixed(1) : Math.round(hoursLeft)}h`;
          note = ` · <b style="color:var(--bad)">exhaustion in ${eta} at current pace</b>`;
        } else {
          note = ` · on track to hit 100% before reset`;
        }
      } else {
        note = ` · on track for ${Math.round(projected)}%`;
      }
    }

    const reset = l.resets_at
      ? `resets ${untilReset(l.resets_at)} <span class="lm-when">(${new Date(l.resets_at * 1000).toLocaleString()})</span>`
      : 'reset time unknown';

    // Linear pace marker: expected % usage if burning evenly across the window
    const paceMarker = (elapsed > 0.05 && elapsed < 0.95)
      ? `<span class="pace-marker" style="left:${(elapsed * 100).toFixed(1)}%" title="Target pace for elapsed time (${Math.round(elapsed * 100)}%)"></span>`
      : '';

    const velText = burnRatePerHour
      ? `<span class="burn-vel" title="Observed consumption pace">⚡ ${burnRatePerHour.toFixed(1)}%/hr</span>`
      : '';

    // a live window comes from the provider, so it carries its own badge and
    // the provider's own description of the window. A stale one is the last
    // number that came back, which is worth saying out loud.
    const live = l.source === 'live' || l.source === 'stale';
    const liveBadge = l.source === 'live'
      ? `<span class="badge badge-live" title="Read from the provider's live quota API">live</span>`
      : l.source === 'stale'
        ? `<span class="badge badge-stale" title="The newest refresh did not come back. These are the last numbers that did.">stale</span>`
        : '';
    const windowText = live ? (l.detail || `${l.window} window`) : `${l.window} window`;
    const taken = live && snapshot ? ` · updated ${snapshot}` : '';

    // a provider that counts its allowance in money says so in money, not in
    // percent: the percentage alone would hide how much is left.
    const amount = l.limit_amount > 0
      ? `<span class="lm-amount">${fmtUsdCents(l.used_amount)} of ${fmtUsdCents(l.limit_amount)}</span>` : '';

    return `<div class="lrow">
      <div class="lh">
        <div class="lh-left">
          <img class="logo sm" src="${agentLogo(l.agent)}" alt="" onerror="this.style.visibility='hidden'">
          <span>${esc(l.agent)} · ${esc(windowText)}${l.plan ? ' · ' + esc(l.plan) : ''}</span>
          ${liveBadge}
          ${badge}
        </div>
        <div class="lh-right">
          <span class="pct" style="color:${colour}">${pct.toFixed(0)}%</span>
          ${amount}
        </div>
      </div>
      <div class="track">
        <i class="bar" style="width:${pct}%;background:${colour}"></i>
        ${paceMarker}
      </div>
      <div class="lm">
        <span>${windowLabel(l.window_minutes)} · ${reset}${note}${taken}</span>
        ${velText}
      </div>
    </div>`;
  }).join('') + `</div>`;
}

// ---------- wiring ----------
const statusLine = () => document.getElementById('status');

async function load() {
  if (LOADING) return;        // a scan is already walking the logs
  LOADING = true;
  document.body.classList.add('loading');
  if (!DATA) renderSkeleton();  // first scan: nothing real to keep on screen
  statusLine().textContent = 'scanning local agent logs…';
  try {
    DATA = await invoke();
    render();
    statusLine().textContent = 'updated ' + new Date().toLocaleTimeString();
    renderMeta(false);
  } catch (e) {
    statusLine().textContent = 'error: ' + e;
  } finally {
    LOADING = false;
    document.body.classList.remove('loading');
  }
}

// footer metadata line, shared by manual loads and live watcher pushes
function renderMeta(live) {
  const p = DATA.pricing;
  const notes = [];
  if ((DATA.plan_routes || []).length) notes.push('plan routes (no per-token billing): ' + DATA.plan_routes.join(', '));
  if ((DATA.unpriced || []).length) notes.push('unpriced: ' + DATA.unpriced.slice(0, 3).join(', '));
  const text =
    `prices: ${p.local} local + ${p.embedded} ${p.source} · ` +
    DATA.sources.map(s => `${s.agent}: ${s.found ? s.files : 'not installed'}`).join(' · ') +
    (notes.length ? ' · ' + notes.join(' · ') : '') +
    (live ? ' · live' : '');
  const line = document.getElementById('priceinfo');
  line.textContent = text;
  line.title = text;      // the line is clipped on narrow windows; the title keeps it readable
}

// the background log watcher pushes fresh stats when agents write new logs,
// so the window never goes stale while it sits open — no refresh click needed
window.__TAURI__?.event?.listen('stats-updated', e => {
  DATA = e.payload;
  render();
  renderMeta(true);
  if (!LOADING) statusLine().textContent = 'updated ' + new Date().toLocaleTimeString() + ' · live';
});
window.__TAURI__?.event?.listen('stats-stage', e => {
  if (LOADING) statusLine().textContent = 'scanning ' + e.payload + '…';
});

for (const [id, set] of [['metric', v => { metric = v; lastMetric = v; }], ['range', v => { rangeDays = v === '0' ? 0 : +v; }],
  ['breakMode', v => { breakMode = v; breakAll = false; }]]) {
  document.getElementById(id).addEventListener('click', e => {
    const b = e.target.closest('button');
    if (!b) return;
    [...e.currentTarget.querySelectorAll('button')].forEach(x => x.classList.toggle('on', x === b));
    set(b.dataset.v);
    render();
  });
}
document.getElementById('view').addEventListener('click', e => {
  const b = e.target.closest('button');
  if (!b) return;
  metric = b.dataset.v === 'limits' ? 'limits' : lastMetric;
  render();
});
const chartBox = document.getElementById('chart');
chartBox.addEventListener('pointermove', chartHover);
chartBox.addEventListener('pointerleave', chartLeave);
document.getElementById('refresh').addEventListener('click', load);
document.getElementById('crumb').addEventListener('click', e => {
  const b = e.target.closest?.('button[data-clear]');
  if (!b) return;
  if (b.dataset.clear === 'agent') selectedAgent = null;
  else if (b.dataset.clear === 'project') selectedProject = null;
  else selectedModel = null;
  render();
});
document.getElementById('exportBtn')?.addEventListener('click', exportCSV);
window.addEventListener('resize', () => DATA && metric !== 'limits' && renderChart(byAgent().list));
load();

// ---------- update system ----------
let UPDATE_INFO = null;
let UPDATE_DOWNLOADING = false;

function fmtBytes(bytes) {
  if (!bytes || bytes <= 0) return '0 B';
  const k = 1024;
  if (bytes >= k * k * k) return (bytes / (k * k * k)).toFixed(2) + ' GB';
  if (bytes >= k * k) return (bytes / (k * k)).toFixed(1) + ' MB';
  if (bytes >= k) return (bytes / k).toFixed(1) + ' KB';
  return bytes + ' B';
}

function switchSettingsTab(tabName) {
  const tabBtn = document.getElementById('tab' + tabName.charAt(0).toUpperCase() + tabName.slice(1));
  const panel = document.getElementById('panel' + tabName.charAt(0).toUpperCase() + tabName.slice(1));
  const allTabs = document.querySelectorAll('.settings-tab');
  const allPanels = document.querySelectorAll('.tab-panel');

  if (allTabs && allTabs.length) {
    allTabs.forEach(t => t.classList?.remove('on'));
  }
  if (allPanels && allPanels.length) {
    allPanels.forEach(p => {
      p.hidden = true;
      p.classList?.remove('on');
    });
  }

  if (tabBtn) tabBtn.classList?.add('on');
  if (panel) {
    panel.hidden = false;
    panel.classList?.add('on');
  }

  // the plans tab shows what the user declared, so it reads the declarations
  // every time the tab is opened
  if (tabName === 'plans') loadPlans();

  const modalTitle = document.getElementById('modalTitle');
  if (modalTitle) {
    if (tabName === 'updates' && UPDATE_INFO && UPDATE_INFO.available) {
      modalTitle.textContent = 'Cinder Update';
    } else {
      modalTitle.textContent = 'Settings';
    }
  }

  const laterBtn = document.getElementById('modalLaterBtn');
  const actionBtn = document.getElementById('modalActionBtn');
  if (tabName !== 'updates') {
    if (laterBtn) laterBtn.hidden = true;
    if (actionBtn) {
      actionBtn.textContent = 'OK';
      actionBtn.disabled = false;
      actionBtn.onclick = hideUpdateModal;
    }
  } else if (UPDATE_INFO && UPDATE_INFO.available) {
    if (laterBtn) laterBtn.hidden = false;
    if (actionBtn) {
      actionBtn.textContent = 'Update Now';
      actionBtn.disabled = false;
      actionBtn.onclick = startUpdateDownload;
    }
  } else {
    if (laterBtn) laterBtn.hidden = true;
    if (actionBtn) {
      actionBtn.textContent = 'OK';
      actionBtn.disabled = false;
      actionBtn.onclick = hideUpdateModal;
    }
  }
}

function openSettings(tabName = 'updates') {
  switchSettingsTab(tabName);
  const modal = document.getElementById('updateModal');
  if (modal) modal.hidden = false;
}

// ---------- plans and budgets ----------
// What the user declared: a plan is a flat rate they pay, a budget is a
// limit on what they may spend. The declarations live on disk, so they are
// read and written through commands instead of being kept in the page.
let PLANS = [];

// the agents Cinder knows about: the ones it has a colour for, plus the ones
// the scanned logs mention.
function knownAgents() {
  const seen = new Set(Object.keys(AGENT_COLORS));
  for (const r of ((DATA && DATA.rows) || [])) if (r.agent) seen.add(r.agent);
  return [...seen].sort();
}

function planRow(p, i) {
  const kind = p.kind === 'budget' ? 'budget' : 'subscription';
  return `<div class="plan-edit" data-i="${i}">
    <input class="pe-agent" data-i="${i}" data-f="agent" list="planAgents" value="${esc(p.agent || '')}" placeholder="Agent">
    <select class="pe-kind" data-i="${i}" data-f="kind">
      <option value="subscription"${kind === 'subscription' ? ' selected' : ''}>Subscription</option>
      <option value="budget"${kind === 'budget' ? ' selected' : ''}>Budget</option>
    </select>
    <input class="pe-limit" data-i="${i}" data-f="limit" type="number" min="0" step="0.01" value="${p.limit > 0 ? esc(String(p.limit)) : ''}" placeholder="0.00">
    <input class="pe-label" data-i="${i}" data-f="label" value="${esc(p.label || '')}" placeholder="Label">
    <button class="pe-del" data-i="${i}" title="Remove this declaration">&times;</button>
  </div>`;
}

function renderPlans() {
  const list = document.getElementById('plansList');
  if (list) {
    list.innerHTML = PLANS.map(planRow).join('') ||
      '<div class="plans-empty">Nothing declared yet. A plan says what an agent costs you a month; a budget says how much you may spend.</div>';
  }
  const dl = document.getElementById('planAgents');
  if (dl) dl.innerHTML = knownAgents().map(a => `<option value="${esc(a)}"></option>`).join('');
}

async function loadPlans() {
  renderPlans();
  try {
    const plans = await invokeCmd('get_plans');
    PLANS = Array.isArray(plans) ? plans : [];
  } catch (e) {
    // no backend to ask: keep the declarations already on screen
  }
  renderPlans();
}

async function savePlans() {
  const btn = document.getElementById('plansSaveBtn');
  const status = document.getElementById('plansStatus');
  // a declaration without a limit says nothing, so it is never sent
  const rows = PLANS
    .filter(p => p.agent && Number(p.limit) > 0)
    .map(p => ({
      agent: p.agent,
      kind: p.kind === 'budget' ? 'budget' : 'subscription',
      limit: Number(p.limit),
      label: p.label || '',
    }));
  if (btn) { btn.disabled = true; btn.textContent = 'Saving…'; }
  try {
    const saved = await invokeCmd('set_plans', { plans: rows });
    PLANS = Array.isArray(saved) ? saved : rows;
    renderPlans();
    if (status) { status.className = 'plans-status'; status.textContent = 'Saved.'; }
    // the limits view reads the plans out of the stats, so they need a fresh
    // scan before the new declaration shows up there.
    load();
  } catch (e) {
    if (status) { status.className = 'plans-status bad'; status.textContent = 'Could not save: ' + e; }
  } finally {
    if (btn) { btn.disabled = false; btn.textContent = 'Save'; }
  }
}

// the rows are the truth, so every field writes straight back into them
function planField(e) {
  const el = e.target;
  if (!el || !el.dataset || el.dataset.i === undefined) return;
  const p = PLANS[Number(el.dataset.i)];
  if (!p) return;
  const f = el.dataset.f;
  if (f === 'agent') p.agent = el.value;
  else if (f === 'kind') p.kind = el.value;
  else if (f === 'limit') p.limit = Number(el.value);
  else if (f === 'label') p.label = el.value;
}

document.getElementById('plansAddBtn')?.addEventListener('click', () => {
  PLANS.push({ agent: '', kind: 'subscription', limit: 0, label: '' });
  renderPlans();
});
document.getElementById('plansList')?.addEventListener('click', e => {
  const b = e.target?.closest?.('.pe-del');
  if (!b) return;
  PLANS.splice(Number(b.dataset.i), 1);
  renderPlans();
});
document.getElementById('plansList')?.addEventListener('input', planField);
document.getElementById('plansList')?.addEventListener('change', planField);
document.getElementById('plansSaveBtn')?.addEventListener('click', savePlans);

function showUpdateModal(info, isUpToDateMessage = false) {
  const modal = document.getElementById('updateModal');
  if (!modal) return;

  switchSettingsTab('updates');

  const tag = document.getElementById('modalTag');
  const curVer = document.getElementById('updateCurrentVer');
  const newVer = document.getElementById('updateNewVer');
  const dateEl = document.getElementById('updateDate');
  const notesEl = document.getElementById('updateNotes');
  const ghLink = document.getElementById('modalGithubBtn');
  const progressBox = document.getElementById('updateProgressBox');
  const actionBtn = document.getElementById('modalActionBtn');
  const laterBtn = document.getElementById('modalLaterBtn');
  const msgEl = document.getElementById('updateMsg');
  const metaBox = document.querySelector('.update-meta');
  const notesSec = document.querySelector('.notes-section');
  const settingsStatus = document.getElementById('settingsUpdateStatus');

  if (progressBox) progressBox.hidden = true;
  if (msgEl) msgEl.hidden = true;

  if (isUpToDateMessage || !info.available) {
    if (tag) tag.textContent = 'Latest';
    if (metaBox) metaBox.hidden = true;
    if (notesSec) notesSec.hidden = true;
    if (settingsStatus) settingsStatus.textContent = 'Cinder is up to date.';
    if (msgEl) {
      msgEl.hidden = false;
      msgEl.textContent = `You are on the latest version of Cinder (v${info.current_version}).`;
    }
    if (laterBtn) laterBtn.hidden = true;
    if (actionBtn) {
      actionBtn.textContent = 'OK';
      actionBtn.disabled = false;
      actionBtn.onclick = hideUpdateModal;
    }
  } else {
    if (metaBox) metaBox.hidden = false;
    if (notesSec) notesSec.hidden = false;
    if (tag) tag.textContent = 'v' + info.latest_version;
    if (curVer) curVer.textContent = 'v' + info.current_version;
    if (newVer) newVer.textContent = 'v' + info.latest_version;
    if (dateEl) {
      dateEl.textContent = info.published_at ? new Date(info.published_at).toLocaleDateString() : '—';
    }
    if (notesEl) {
      notesEl.textContent = info.release_notes || 'No release notes provided.';
    }
    if (ghLink) ghLink.href = info.release_url || 'https://github.com/khentmoba/Cinder/releases';
    if (settingsStatus) settingsStatus.textContent = `Update v${info.latest_version} available!`;
    if (laterBtn) laterBtn.hidden = false;
    if (actionBtn) {
      actionBtn.textContent = 'Update Now';
      actionBtn.disabled = false;
      actionBtn.onclick = startUpdateDownload;
    }
  }

  modal.hidden = false;
}

function hideUpdateModal() {
  const modal = document.getElementById('updateModal');
  if (modal) modal.hidden = true;
}

function updateProgressUI(downloaded, total, percent, finished) {
  const progressBox = document.getElementById('updateProgressBox');
  const bar = document.getElementById('progressBar');
  const pct = document.getElementById('progressPercent');
  const label = document.getElementById('progressLabel');
  const sub = document.getElementById('progressSub');

  if (progressBox) progressBox.hidden = false;
  const clamped = Math.min(100, Math.max(0, percent));
  if (bar) bar.style.width = clamped + '%';
  if (pct) pct.textContent = Math.round(clamped) + '%';
  if (sub) sub.textContent = `${fmtBytes(downloaded)} / ${total > 0 ? fmtBytes(total) : 'unknown'}`;

  if (finished) {
    if (label) label.textContent = 'Installing update and restarting Cinder…';
    if (pct) pct.textContent = '100%';
    if (bar) bar.style.width = '100%';
  }
}

async function startUpdateDownload() {
  if (UPDATE_DOWNLOADING || !UPDATE_INFO) return;
  if (!UPDATE_INFO.download_url) {
    if (UPDATE_INFO.release_url) window.open(UPDATE_INFO.release_url, '_blank');
    return;
  }

  UPDATE_DOWNLOADING = true;
  const actionBtn = document.getElementById('modalActionBtn');
  const progressBox = document.getElementById('updateProgressBox');
  const label = document.getElementById('progressLabel');
  const msgEl = document.getElementById('updateMsg');

  if (actionBtn) {
    actionBtn.disabled = true;
    actionBtn.textContent = 'Downloading…';
  }
  if (progressBox) progressBox.hidden = false;
  if (label) label.textContent = 'Downloading update…';
  if (msgEl) msgEl.hidden = true;

  try {
    await invokeCmd('download_and_install_update', {
      downloadUrl: UPDATE_INFO.download_url,
      assetName: UPDATE_INFO.asset_name,
    });
  } catch (err) {
    UPDATE_DOWNLOADING = false;
    if (actionBtn) {
      actionBtn.disabled = false;
      actionBtn.textContent = 'Retry Update';
    }
    if (msgEl) {
      msgEl.hidden = false;
      msgEl.textContent = 'Update failed: ' + err;
    }
  }
}

async function checkForUpdates(interactive = false) {
  try {
    const info = await invokeCmd('check_update');
    UPDATE_INFO = info;

    const badge = document.getElementById('updateBadge');
    const badgeText = document.getElementById('updateBadgeText');
    const updateLink = document.getElementById('updateStatusText');
    const settingsDot = document.getElementById('settingsUpdateDot');
    const tabDot = document.getElementById('tabUpdateDot');

    if (info.available) {
      if (badge) {
        badge.hidden = false;
        if (badgeText) badgeText.textContent = `v${info.latest_version} available`;
      }
      if (updateLink) {
        updateLink.textContent = `✦ Update v${info.latest_version}`;
        updateLink.hidden = false;
      }
      if (settingsDot) settingsDot.hidden = false;
      if (tabDot) tabDot.hidden = false;
      if (interactive) {
        showUpdateModal(info, false);
      }
    } else {
      if (badge) badge.hidden = true;
      if (updateLink) updateLink.hidden = true;
      if (settingsDot) settingsDot.hidden = true;
      if (tabDot) tabDot.hidden = true;
      if (interactive) {
        showUpdateModal(info, true);
      }
    }
  } catch (e) {
    if (interactive) {
      showUpdateModal({ current_version: '0.1.0', available: false }, false);
      const msgEl = document.getElementById('updateMsg');
      if (msgEl) {
        msgEl.hidden = false;
        msgEl.textContent = 'Check failed: ' + e;
      }
    }
  }
}

// Initial version fetch & update listeners
invokeCmd('get_app_version')
  .then(ver => {
    const verEl = document.getElementById('appVersion');
    if (verEl && ver) verEl.textContent = 'v' + ver;
    const aboutVer = document.getElementById('aboutVersion');
    if (aboutVer && ver) aboutVer.textContent = 'v' + ver;
    const modalTag = document.getElementById('modalTag');
    if (modalTag && ver && (!UPDATE_INFO || !UPDATE_INFO.available)) modalTag.textContent = 'v' + ver;
  })
  .catch(() => {});

window.__TAURI__?.event?.listen('update-download-progress', e => {
  const p = e.payload;
  if (p) updateProgressUI(p.downloaded, p.total, p.percent, p.finished);
});

document.getElementById('settingsBtn')?.addEventListener('click', () => openSettings('updates'));
document.getElementById('footerSettingsBtn')?.addEventListener('click', () => openSettings('updates'));

document.getElementById('settingsTabs')?.addEventListener('click', e => {
  const tab = e.target.closest ? e.target.closest('.settings-tab') : e.target;
  if (tab && tab.dataset && tab.dataset.tab) switchSettingsTab(tab.dataset.tab);
});

document.getElementById('settingsCheckBtn')?.addEventListener('click', async () => {
  const btn = document.getElementById('settingsCheckBtn');
  if (btn) {
    btn.disabled = true;
    btn.textContent = 'Checking…';
  }
  await checkForUpdates(true);
  if (btn) {
    btn.disabled = false;
    btn.textContent = 'Check for Updates';
  }
});

document.getElementById('settingsRescanBtn')?.addEventListener('click', async () => {
  const rescanBtn = document.getElementById('settingsRescanBtn');
  if (rescanBtn) {
    rescanBtn.disabled = true;
    rescanBtn.textContent = 'Rescanning all sources…';
  }
  document.body.classList.add('loading');
  try {
    DATA = await invokeCmd('rescan_stats');
    render();
    statusLine().textContent = 'updated ' + new Date().toLocaleTimeString();
    renderMeta(false);
  } catch (e) {
    statusLine().textContent = 'error: ' + e;
  } finally {
    document.body.classList.remove('loading');
    if (rescanBtn) {
      rescanBtn.disabled = false;
      rescanBtn.textContent = '↻ Rescan All Sources Now';
    }
  }
});

function initPreferences() {
  try {
    if (typeof localStorage === 'undefined') return;
    const savedMetric = localStorage.getItem('cinder_default_metric');
    if (savedMetric && ['cost', 'tokens', 'requests', 'limits'].includes(savedMetric)) {
      const btn = metricButton(savedMetric);
      if (btn) btn.click();
      updatePrefButtons('prefMetric', savedMetric);
    }
    const savedRange = localStorage.getItem('cinder_default_range');
    if (savedRange && ['1', '7', '30', '90'].includes(savedRange)) {
      const btn = document.querySelector(`#range button[data-v="${savedRange}"]`);
      if (btn) btn.click();
      updatePrefButtons('prefRange', savedRange);
    }
  } catch (_) {}
}

function metricButton(val) {
  return val === 'limits'
    ? document.querySelector('#view button[data-v="limits"]')
    : document.querySelector(`#metric button[data-v="${val}"]`);
}

function updatePrefButtons(containerId, activeVal) {
  const container = document.getElementById(containerId);
  if (!container) return;
  const buttons = container.querySelectorAll ? container.querySelectorAll('button') : [];
  if (buttons && buttons.forEach) {
    buttons.forEach(b => {
      if (b.dataset && b.dataset.v === activeVal) b.classList?.add('on');
      else b.classList?.remove('on');
    });
  }
}

document.getElementById('prefMetric')?.addEventListener('click', e => {
  const b = e.target.closest ? e.target.closest('button') : e.target;
  if (!b || !b.dataset) return;
  const val = b.dataset.v;
  if (typeof localStorage !== 'undefined') localStorage.setItem('cinder_default_metric', val);
  updatePrefButtons('prefMetric', val);
  const mainBtn = metricButton(val);
  if (mainBtn) mainBtn.click();
});

document.getElementById('prefRange')?.addEventListener('click', e => {
  const b = e.target.closest ? e.target.closest('button') : e.target;
  if (!b || !b.dataset) return;
  const val = b.dataset.v;
  if (typeof localStorage !== 'undefined') localStorage.setItem('cinder_default_range', val);
  updatePrefButtons('prefRange', val);
  const mainBtn = document.querySelector(`#range button[data-v="${val}"]`);
  if (mainBtn) mainBtn.click();
});

initPreferences();

document.getElementById('updateBadge')?.addEventListener('click', () => {
  if (UPDATE_INFO && UPDATE_INFO.available) {
    showUpdateModal(UPDATE_INFO, false);
  } else {
    checkForUpdates(true);
  }
});
document.getElementById('appVersion')?.addEventListener('click', () => openSettings('updates'));
document.getElementById('updateStatusText')?.addEventListener('click', () => {
  if (UPDATE_INFO && UPDATE_INFO.available) {
    showUpdateModal(UPDATE_INFO, false);
  } else {
    checkForUpdates(true);
  }
});
document.getElementById('modalCloseBtn')?.addEventListener('click', hideUpdateModal);
document.getElementById('modalLaterBtn')?.addEventListener('click', hideUpdateModal);
document.getElementById('updateModal')?.addEventListener('click', e => {
  if (e.target.id === 'updateModal') hideUpdateModal();
});
window.addEventListener('keydown', e => {
  if (e.key === 'Escape') hideUpdateModal();
  // No text inputs exist outside the modal, so bare keys are safe shortcuts.
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  const tag = (e.target?.tagName || '').toLowerCase();
  if (tag === 'input' || tag === 'textarea') return;
  const keys = { '1': 'cost', '2': 'tokens', '3': 'requests', '4': 'limits' };
  if (keys[e.key]) {
    const btn = metricButton(keys[e.key]);
    if (btn) btn.click();
    else { metric = keys[e.key]; render(); }
    e.preventDefault();
  } else if (e.key === 'r' || e.key === 'R') {
    load();
  } else if (e.key === 's' || e.key === 'S') {
    document.getElementById('footerSettingsBtn')?.click();
  }
});

// Check for updates in the background after startup
if (typeof setTimeout === 'function') {
  setTimeout(() => checkForUpdates(false), 2500);
}
