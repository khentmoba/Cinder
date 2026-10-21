// Cinder UI: aggregates the per-day rows from the Rust collector.
const invokeCmd = (cmd, args) => (window.__TAURI__ ? window.__TAURI__.core.invoke(cmd, args) : Promise.reject('not in Tauri'));
const invoke = () => invokeCmd('get_stats');

let DATA = null;
let LOADING = false;         // a scan is in flight
let metric = 'tokens';       // cost | tokens | requests | limits
let lastMetric = 'tokens';   // the metric to fall back to when leaving Limits
let rangeDays = 90;          // 1 | 7 | 30 | 90 | 0 = all
let breakMode = 'model';     // model | day
let selectedAgent = null;    // null = all agents
let selectedModel = null;    // null = all models
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
const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

function todayStr(offsetDays = 0) {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() + offsetDays);
  return d.toISOString().slice(0, 10);
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
// One place where the model breadcrumb narrows every panel at once.
function applyModelFilter(rows) {
  if (!selectedModel) return rows;
  return rows.filter(r => modelKey(r) === selectedModel);
}
function sessionCount() {
  if (rangeDays === 0) return DATA.sessions;
  const cutoff = todayStr(-(rangeDays - 1));
  return DATA.sessions.filter(s => s.day >= cutoff);
}

function blank() {
  return { requests: 0, input: 0, cache_read: 0, cache_write: 0, output: 0, reasoning: 0, tools: 0, cost: 0, cost_known: false, cost_partial: false, savings: 0 };
}
function acc(r, a) {
  a.requests += r.requests; a.input += r.input; a.cache_read += r.cache_read; a.cache_write += r.cache_write;
  a.output += r.output; a.reasoning += r.reasoning; a.tools += r.tools; a.cost += r.cost;
  a.cost_known = a.cost_known || r.cost_known;
  a.cost_partial ||= !!r.cost_partial || (!r.cost_known && tok(r) > 0);
  a.savings += r.savings;
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
    `<div id="bigTotal" class="big sk h1"></div><div id="bigSub" class="sub sk w3"></div><div id="bigDelta" class="delta"></div>`;
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
  const sub = document.getElementById('bigSub');
  sub.className = 'sub';
  sub.textContent = grand.sessions + ' sessions · ' + grand.requests.toLocaleString() + ' requests · ' + fmtTok(grand.tools) + ' tool calls';
  renderDelta();

  const grandVal = mval(grand);
  const agentList = document.getElementById('agentList');
  agentList.innerHTML = list.map(a => {
    const v = mval(a);
    const share = grandVal ? (v / grandVal * 100).toFixed(1) : '0.0';
    // Cost is redundant when it is the selected metric; the request count never
    // is, and it is the only volume signal for agents that log no tokens.
    const extra = metric === 'cost' ? ''
      : ` · ${fmtUsd(a.cost, a.cost_known, a.cost_partial)} · ${a.requests.toLocaleString()} req`;
    return `<button class="agent-row${selectedAgent === a.agent ? ' sel' : ''}" data-agent="${esc(a.agent)}"
      style="box-shadow:inset 2px 0 0 ${color(a.agent)}" title="Filter to ${esc(a.agent)}">
      <span class="radio"></span>
      <img class="logo" src="${agentLogo(a.agent)}" alt="" onerror="this.style.visibility='hidden'">
      <span class="name">${esc(a.agent)} <span class="cnt">${a.sessions} sessions</span></span>
      <span class="val">${mfmt(v, a.cost_known, a.cost_partial)}</span>
      <span class="meta">${share}% of ${METRICS[metric].label}${extra}</span>
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
  if (metric !== 'cost') items.push(['Cost', fmtUsd(grand.cost, grand.cost_known, grand.cost_partial)]);
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
      : (breakMode === 'agent') ? (selectedAgent === r.key) : false;
    return `<button class="bd-row${active ? ' sel' : ''}" data-key="${esc(r.key)}"${(breakMode === 'model' || breakMode === 'cache') ? ' data-model="1"' : breakMode === 'agent' ? ' data-agent="1"' : ''}>
      <span class="share" style="width:${Math.min(100, r.share)}%"></span>
      <span class="rank">${i + 1}</span>
      <span class="cell-logo">${r.logo ? `<img class="logo sm" src="${r.logo}" alt="" onerror="this.remove()">` : ''}</span>
      <span class="bd-name">${esc(r.name)}${r.extra ? ` <span class="cnt">${esc(r.extra)}</span>` : ''}</span>
      <span class="bd-cost${(r.priced || r.costKnown || breakMode === 'cache') ? '' : ' unpriced'}">${cost}</span>
      <span class="bd-num">${r.num}</span>
    </button>`;
  }).join('');
  const more = (!breakAll && rows.length > shown.length)
    ? `<button class="bd-more" id="bdMore">Show all ${rows.length}</button>` : '';
  el.innerHTML = head + body + more;
  document.getElementById('bdMore')?.addEventListener('click', () => { breakAll = true; renderBreakdown(); });
  el.querySelectorAll('.bd-row').forEach(row => row.addEventListener('click', () => {
    if (breakMode === 'model' || breakMode === 'cache') {
      selectedModel = selectedModel === row.dataset.key ? null : row.dataset.key;
      render();
    } else if (breakMode === 'agent') {
      selectedAgent = selectedAgent === row.dataset.key ? null : row.dataset.key;
      render();
    }
  }));
}

// Data Sources reads the last scan, so "not installed" is truthful instead of
// a README copy.
function renderSources() {
  const el = document.getElementById('sourcesList');
  if (!el || !DATA) return;
  el.innerHTML = (DATA.sources || []).map(s => `<div class="source-item${s.found ? '' : ' off'}">
    <div class="source-name"><span class="dot"></span><b>${esc(s.agent)}</b> <code>${esc(s.path)}</code></div>
    <div class="source-metrics">${s.found ? (s.files ? s.files.toLocaleString() + ' files' : 'found') : 'not installed'}</div>
  </div>`).join('') || '<div class="source-metrics">No sources reported by the last scan.</div>';
}

// the visible breakdown, as CSV
function exportCSV() {
  const { rows } = breakdownData();
  let headers = 'rank,name,cost_usd,tokens,share_percent';
  let lines;
  if (breakMode === 'cache') {
    headers = 'rank,model,savings_usd,hit_rate_percent,cache_read_tokens';
    lines = rows.map((r, i) => [
      i + 1,
      `"${String(r.name).replace(/"/g, '""')}"`,
      r.rawSavings ? r.rawSavings.toFixed(4) : '0.0000',
      r.hitRate ? r.hitRate.toFixed(2) : '0.00',
      r.cacheRead || 0
    ].join(','));
  } else if (breakMode === 'agent') {
    headers = 'rank,agent,cost_usd,metric_value,share_percent,cache_savings_usd';
    lines = rows.map((r, i) => [
      i + 1,
      `"${String(r.name).replace(/"/g, '""')}"`,
      (r.priced || r.costKnown) ? r.cost.toFixed(4) : '',
      `"${String(r.num).replace(/"/g, '""')}"`,
      r.share.toFixed(2),
      r.savings ? r.savings.toFixed(4) : '0.0000'
    ].join(','));
  } else {
    lines = rows.map((r, i) => [
      i + 1,
      `"${String(r.name).replace(/"/g, '""')}"`,
      (r.priced || r.costKnown) ? r.cost.toFixed(4) : '',
      `"${String(r.num).replace(/"/g, '""')}"`,
      r.share.toFixed(2)
    ].join(','));
  }
  const csv = [headers].concat(lines).join('\n');
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

function renderLimits() {
  const box = document.getElementById('limitsView');
  const limits = [...(DATA.limits || [])].sort((a, b) => b.used_percent - a.used_percent);
  if (!limits.length) {
    box.innerHTML = `<div class="limits"><h3 style="margin-top:0">Rate limits</h3>
      <p style="color:var(--dim)">No agent reported rate-limit usage in the scanned logs.<br>
      Only Codex writes plan limits into its session files (5h / weekly windows).</p></div>`;
    return;
  }
  box.innerHTML = `<div class="limits"><h3 style="margin-top:0">Rate limits</h3>` + limits.map(l => {
    const pct = Math.min(100, Math.max(0, l.used_percent));
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

    return `<div class="lrow">
      <div class="lh">
        <div class="lh-left">
          <img class="logo sm" src="${agentLogo(l.agent)}" alt="" onerror="this.style.visibility='hidden'">
          <span>${esc(l.agent)} · ${esc(l.window)} window${l.plan ? ' · ' + esc(l.plan) : ''}</span>
          ${badge}
        </div>
        <span class="pct" style="color:${colour}">${pct.toFixed(0)}%</span>
      </div>
      <div class="track">
        <i class="bar" style="width:${pct}%;background:${colour}"></i>
        ${paceMarker}
      </div>
      <div class="lm">
        <span>${windowLabel(l.window_minutes)} · ${reset}${note}</span>
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
