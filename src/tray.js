// The tray popover.
//
// A small window that opens next to the tray icon and answers the questions
// the tooltip cannot: how much of each subscription is left, when it resets,
// what today cost, and what is about to run out.
//
// The window itself is placed, shown and hidden by the Rust side, so this
// script only reads numbers and asks for actions.

const invokeCmd = (cmd, args) => (window.__TAURI__ ? window.__TAURI__.core.invoke(cmd, args) : Promise.reject('not in Tauri'));

let DATA = null;
let TIMER = null;
const REFRESH_MS = 20000;

const esc = s => String(s == null ? '' : s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

const fmtUsd = (n, known, partial = false) => known
  ? (partial ? '≥ ' : '') + '$' + Number(n || 0).toLocaleString('en-US', Number(n || 0) >= 100
      ? { maximumFractionDigits: 0 } : { minimumFractionDigits: 2, maximumFractionDigits: 2 })
  : '—';

function fmtTok(n) {
  if (n >= 1e9) return (n / 1e9).toFixed(2) + 'B';
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e7 ? 0 : 1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'K';
  return String(n || 0);
}

// "5h" -> "5-hour window". The ids come from the providers, so the names are
// spelled out here rather than shown raw.
const WINDOW_NAMES = {
  '5h': '5-hour window',
  '7d': '7-day window',
  '30d': '30-day window',
  'monthly': 'Monthly credits',
  'premium': 'Premium requests',
  '7d-sonnet': 'Weekly Sonnet',
  '7d-opus': 'Weekly Opus',
  '7d-haiku': 'Weekly Haiku',
};
const windowName = id => WINDOW_NAMES[id] || id;

// The mark for a provider. A letter avatar is used when there is no picture,
// so a card never shows a broken image.
const LOGOS = {
  'Claude Code': 'icons/anthropic.svg',
  'Codex': 'icons/openai.svg',
  'OpenCode': 'icons/opencode.svg',
  'Gemini CLI': 'icons/googlegemini.svg',
  'Pi': 'icons/pi.svg',
};
const COLORS = {
  'Claude Code': '#d29922',
  'Codex': '#e8e8ea',
  'OpenCode': '#3fb950',
  'Gemini CLI': '#4da3ff',
  'GitHub Copilot': '#9aa0a6',
};
function mark(agent) {
  const name = String(agent || '').split(' · ')[0];
  const logo = LOGOS[name];
  if (logo) return `<img class="logo" src="${logo}" alt="" onerror="this.style.visibility='hidden'">`;
  const letter = (name || '?').trim().charAt(0).toUpperCase();
  const colour = COLORS[name] || '#7d8590';
  return `<span class="avatar" style="background:${colour}">${esc(letter)}</span>`;
}

// How long until a window resets.
function untilReset(seconds) {
  const left = seconds - Date.now() / 1000;
  if (left <= 0) return 'resetting';
  const hours = left / 3600;
  if (hours < 1) return 'resets in ' + Math.max(1, Math.round(left / 60)) + 'm';
  if (hours < 48) return 'resets in ' + (hours < 10 ? hours.toFixed(1) : Math.round(hours)) + 'h';
  return 'resets in ' + Math.round(hours / 24) + 'd';
}

// How old the live numbers are.
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

// The plan name, the way a person would say it.
function planName(plan) {
  const p = String(plan || '').trim();
  if (!p) return '';
  const known = { max: 'Max', pro: 'Pro', plus: 'Plus', team: 'Team', enterprise: 'Enterprise', premium: 'Premium', copilot: 'Copilot' };
  const lower = p.toLowerCase();
  if (known[lower]) return known[lower];
  return p.charAt(0).toUpperCase() + p.slice(1);
}

function windowRow(w) {
  const pct = Math.min(100, Math.max(0, w.used_percent || 0));
  const colour = pct > 80 ? 'var(--bad)' : pct > 50 ? 'var(--warn)' : 'var(--accent)';
  const reset = w.resets_at ? untilReset(w.resets_at) : '';
  const detail = w.detail && w.detail !== windowName(w.window) ? w.detail : '';
  return `<div class="wrow">
    <div class="wtop"><span class="wname">${esc(windowName(w.window))}</span><span class="wpct" style="color:${colour}">${Math.round(pct)}%</span></div>
    <div class="track"><i class="bar" style="width:${pct}%;background:${colour}"></i></div>
    <div class="wmeta"><span>${esc(detail)}</span><span>${esc(reset)}</span></div>
  </div>`;
}

function cardHTML(c) {
  const plan = planName(c.plan);
  const stale = c.stale ? '<span class="badge stale" title="These are the last numbers that came back; the newest refresh did not succeed">stale</span>' : '';
  const name = String(c.agent || '').split(' · ')[0];
  const account = String(c.agent || '').includes(' · ') ? c.agent.split(' · ').slice(1).join(' · ') : '';
  const head = `<div class="chead">
      ${mark(name)}
      <span class="cname">${esc(name)}</span>
      ${account ? `<span class="caccount">${esc(account)}</span>` : ''}
      ${plan ? `<span class="badge plan">${esc(plan)}</span>` : ''}
      ${stale}
    </div>`;
  if (c.error) {
    return `<section class="pcard failed">${head}
      <div class="err">${esc(c.error)}</div>
    </section>`;
  }
  if (!c.windows || !c.windows.length) {
    return `<section class="pcard">${head}<div class="err">No windows reported.</div></section>`;
  }
  return `<section class="pcard">${head}${c.windows.map(windowRow).join('')}</section>`;
}

function alertHTML(a) {
  const critical = a.severity === 'critical';
  return `<div class="alert ${critical ? 'crit' : 'warn'}">
    <div class="atitle">${esc(a.title || '')}</div>
    <div class="adetail">${esc(a.detail || '')}</div>
  </div>`;
}

function planHTML(p) {
  if (p.kind === 'budget') {
    const pct = Math.min(100, Math.max(0, p.percent || 0));
    const colour = pct > 80 ? 'var(--bad)' : pct > 50 ? 'var(--warn)' : 'var(--accent)';
    return `<div class="planrow">
      <span class="pname">${esc(p.agent)}<span class="badge budget">Budget</span></span>
      <span class="pval" style="color:${colour}">${Math.round(pct)}%</span>
      <div class="track"><i class="bar" style="width:${pct}%;background:${colour}"></i></div>
      <div class="pmeta">${esc(fmtUsd(p.used, true))} of your ${esc(fmtUsd(p.limit, true))} budget</div>
    </div>`;
  }
  return `<div class="planrow">
    <span class="pname">${esc(p.agent)}<span class="badge plan">Plan</span></span>
    <span class="pval">${(p.multiple || 0).toFixed(1)}x</span>
    <div class="pmeta">${esc(p.label || p.agent)} · ${esc(fmtUsd(p.limit, true))} a month · the work was worth ${esc(fmtUsd(p.used, true))}</div>
  </div>`;
}

function render() {
  const body = document.getElementById('body');
  const fresh = document.getElementById('fresh');
  if (!body) return;
  if (!DATA) {
    body.innerHTML = '<div class="empty">Reading the providers…</div>';
    return;
  }
  const s = DATA;
  const parts = [];
  if (s.alerts && s.alerts.length) {
    parts.push('<div class="section">Alerts</div>' + s.alerts.map(alertHTML).join(''));
  }
  if (s.cards && s.cards.length) {
    parts.push('<div class="section">Subscriptions</div>' + s.cards.map(cardHTML).join(''));
  } else {
    parts.push('<div class="empty">No provider is signed in on this machine yet.</div>');
  }
  if (s.plans && s.plans.length) {
    parts.push('<div class="section">Your plans</div>' + s.plans.map(planHTML).join(''));
  }
  body.innerHTML = parts.join('');

  if (fresh) {
    const age = sinceAgo(s.checked_at);
    const stale = s.restored || (s.cards || []).some(c => c.stale);
    fresh.textContent = age ? (stale ? age + ' · stale' : age) : 'not read yet';
    fresh.className = 'fresh' + (stale ? ' stale' : '');
  }
  const today = document.getElementById('today');
  if (today) {
    // A partial figure is still worth showing: it says what is known, and
    // the "≥" says there is more.
    const known = s.today_cost_known !== false || s.today_cost_partial;
    const cost = fmtUsd(s.today_cost, known, s.today_cost_partial);
    today.innerHTML = `<b>${esc(cost)}</b> today · ${esc(fmtTok(s.today_tokens))} tokens · ${esc(String(s.today_requests || 0))} requests`;
  }
}

async function load() {
  try {
    DATA = await invokeCmd('get_tray_summary');
  } catch (e) {
    DATA = DATA || { cards: [], alerts: [], plans: [] };
  }
  render();
}

function action(name) {
  invokeCmd('tray_action', { action: name }).catch(() => {});
}

function start() {
  load();
  if (TIMER) clearInterval(TIMER);
  TIMER = setInterval(load, REFRESH_MS);
}

document.getElementById('refresh')?.addEventListener('click', () => { action('refresh-quota'); load(); });
document.getElementById('openBtn')?.addEventListener('click', () => action('open'));
document.getElementById('rescanBtn')?.addEventListener('click', () => { action('rescan'); setTimeout(load, 1500); });
document.getElementById('quitBtn')?.addEventListener('click', () => action('quit'));
window.addEventListener('keydown', e => {
  if (e.key === 'Escape') action('hide');
});
window.__TAURI__?.event?.listen('stats-updated', () => load());

start();
