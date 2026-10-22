const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/main.js'), 'utf8');

function dashboard() {
  const elements = new Map();
  const handlers = {};
  const element = () => ({
    innerHTML: '', textContent: '', hidden: false, className: '',
    clientWidth: 800, clientHeight: 400,
    classList: { add() {}, remove() {}, toggle() {} },
    addEventListener() {}, setAttribute() {},
    querySelector: () => element(), querySelectorAll: () => [],
  });
  const get = id => {
    if (!elements.has(id)) elements.set(id, element());
    return elements.get(id);
  };
  const context = vm.createContext({
    window: {
      // Keep startup suspended; all tests supply synthetic data explicitly.
      __TAURI__: { core: { invoke: () => new Promise(() => {}) } },
      addEventListener: (name, callback) => { handlers[name] = callback; },
    },
    document: { body: element(), getElementById: get, querySelector: get, querySelectorAll: () => [] },
  });
  vm.runInContext(source, context);
  const run = code => vm.runInContext(code, context);
  run("rangeDays = 0; DATA = { rows: [], sessions: [], limits: [] };");
  return { run, get, handlers };
}

function mixedUsage(run) {
  run(`metric = 'cost'; DATA.rows = [
    {...blank(), day: '2026-01-01', agent: 'Pi', model: 'm', input: 100, requests: 1, cost: 1, cost_known: true},
    {...blank(), day: '2026-01-01', agent: 'Pi', model: 'm', input: 100, requests: 1},
    {...blank(), day: '2026-01-01', agent: 'Pi', model: 'm', tools: 1}
  ];`);
}

test('partial costs remain partial across agent, model, day, and grand totals', () => {
  const { run } = dashboard();
  mixedUsage(run);
  for (const expression of ['byAgent().grand', 'byAgent().list[0]', 'byModel().list[0]', 'byDay()[0]']) {
    assert.equal(run(`(${expression}).cost_partial`), true);
    assert.equal(run(`fmtUsd((${expression}).cost, (${expression}).cost_known, (${expression}).cost_partial)`), '≥ $1.00');
  }
  run('DATA.rows.reverse()');
  assert.equal(run('byAgent().grand.cost_partial'), true);
  assert.equal(run('byAgent().grand.input'), 200);
});

test('all cost displays mark incomplete totals, including the tooltip', () => {
  const { run, get } = dashboard();
  mixedUsage(run);
  run('render()');
  assert.equal(get('bigTotal').textContent, '≥ $1.00');
  assert.match(get('agentList').innerHTML, /≥ \$1\.00/);
  assert.match(get('breakdown').innerHTML, /≥ \$1\.00/);
  run("breakMode = 'day'; renderBreakdown()");
  assert.match(get('breakdown').innerHTML, /≥ \$1\.00/);
  const tip = run('tipHTML(chart.days, chart.series, 0)');
  assert.equal((tip.match(/≥ \$1\.00/g) || []).length, 2);
  run("metric = 'tokens'; render()");
  assert.match(get('agentList').innerHTML, /≥ \$1\.00/);
});

test('already-partial backend rows stay partial without a separate unknown row', () => {
  const { run } = dashboard();
  run(`DATA.rows = [{...blank(), day: '2026-01-01', agent: 'Pi', model: 'm', input: 200,
    cost: 1, cost_known: true, cost_partial: true}];`);
  assert.equal(run('byAgent().grand.cost_partial'), true);
});

test('free usage is known, tools-only rows do not make it partial, unknown usage stays unpriced', () => {
  const { run } = dashboard();
  run(`metric = 'cost'; DATA.rows = [
    {...blank(), day: '2026-01-01', agent: 'Pi', model: 'm', input: 100, cost_known: true},
    {...blank(), day: '2026-01-01', agent: 'Pi', model: 'm', tools: 1}
  ]; render()`);
  assert.equal(run('byAgent().grand.cost_partial'), false);
  assert.match(run('tipHTML(chart.days, chart.series, 0)'), /Total<\/span><b>\$0\.00/);
  run('DATA.rows[0].cost_known = false; render()');
  assert.match(run('tipHTML(chart.days, chart.series, 0)'), /Total<\/span><b>—/);
});

test('Limits view can be resized with populated data and return to the dashboard', () => {
  const { run, handlers, get } = dashboard();
  mixedUsage(run);
  run("metric = 'limits'; render()");
  assert.equal(get('dash').hidden, true);
  assert.doesNotThrow(() => handlers.resize());
  run("metric = 'tokens'; render()");
  assert.equal(get('dash').hidden, false);
  assert.doesNotThrow(() => handlers.resize());
});

test('Today uses UTC calendar boundaries, including through a local daylight-saving change', () => {
  const { run } = dashboard();
  run(`const RealDate = Date;
    Date = class extends RealDate {
      constructor(...args) { super(...(args.length ? args : ['2026-03-09T00:30:00Z'])); }
    };
    rangeDays = 1;
    DATA.rows = [{day: '2026-03-08'}, {day: '2026-03-09'}];
    DATA.sessions = [{day: '2026-03-08'}, {day: '2026-03-09'}];`);
  assert.equal(run('todayStr()'), '2026-03-09');
  assert.equal(run('todayStr(-1)'), '2026-03-08');
  assert.equal(run('filtered().length'), 1);
  assert.equal(run('filtered()[0].day'), '2026-03-09');
  assert.equal(run('sessionCount().length'), 1);
  run('rangeDays = 7');
  assert.equal(run('filtered().length'), 2);
  for (const file of ['../src/index.html', '../mock/index.html']) {
    const html = fs.readFileSync(path.join(__dirname, file), 'utf8');
    assert.match(html, /data-v="1" title="UTC calendar day">Today<\/button>/);
    assert.doesNotMatch(html, /Past 24h/);
  }
});

test('model aggregation no longer carries unused sets', () => {
  const { run } = dashboard();
  mixedUsage(run);
  assert.equal(run("'agents' in byModel().list[0]"), false);
  assert.equal(run("'pricedModels' in byModel().list[0]"), false);
});

test('an empty dashboard renders and resizes without throwing', () => {
  const { run, handlers } = dashboard();
  assert.doesNotThrow(() => run('render()'));
  assert.doesNotThrow(() => handlers.resize());
});

test('All time range button and filter aggregates without date cutoff', () => {
  const { run, get } = dashboard();
  run(`DATA.rows = [
    {...blank(), day: '2025-01-01', agent: 'Pi', model: 'm', input: 100, cost: 1, cost_known: true},
    {...blank(), day: '2026-10-18', agent: 'Pi', model: 'm', input: 200, cost: 2, cost_known: true}
  ];
  DATA.sessions = [{day: '2025-01-01', agent: 'Pi', sessions: 1}, {day: '2026-10-18', agent: 'Pi', sessions: 1}];
  rangeDays = 0;
  render();`);
  assert.equal(run('filtered().length'), 2);
  assert.equal(run('byAgent().grand.input'), 300);
  assert.match(get('rangeLabel').textContent, /2025.*to.*2026/);
  for (const file of ['../src/index.html', '../mock/index.html']) {
    const html = fs.readFileSync(path.join(__dirname, file), 'utf8');
    assert.match(html, /data-v="0">All time<\/button>/);
  }
});

test('limit cards speak human time, not raw minute counts', () => {
  const { run } = dashboard();
  assert.equal(run('windowLabel(43200)'), '30-day window');
  assert.equal(run('windowLabel(300)'), '5-hour window');
  assert.equal(run('windowLabel(30)'), '30-minute window');
  assert.equal(run('untilReset(0)'), 'due');
  assert.match(run(`untilReset(${Math.floor(Date.now() / 1000) + 7200})`), /^in (2\.0h|1\.9h|119m)$/);
  assert.match(run(`untilReset(${Math.floor(Date.now() / 1000) + 4 * 86400})`), /^in 4d$/);
});

test('the sources panel reports what the scan actually found', () => {
  const { run, get } = dashboard();
  run("DATA.sources = [{agent:'Pi', path:'~/.pi/agent/sessions', found:true, files:5}, {agent:'Claude Code', path:'~/.claude/projects', found:false, files:0}]; renderSources();");
  const html = get('sourcesList').innerHTML;
  assert.match(html, /5 files/);
  assert.match(html, /not installed/);
  assert.match(html, /~\/\.pi\/agent\/sessions/);
});

test('a 90-day chart buckets into weeks and the model filter narrows every panel', () => {
  const { run } = dashboard();
  const days = Array.from({ length: 90 }, (_, i) => ({ day: `2026-01-${String(i + 1).padStart(2, '0')}` }));
  assert.equal(run('bucketDays([{day:"2026-01-01"},{day:"2026-01-02"}], false).length'), 2);
  assert.equal(run(`bucketDays(${JSON.stringify(days)}, true).length`), 13);
  run("selectedModel = '(tools only)'; DATA.rows = [{...blank(), model: 'gpt', requests: 1}, {...blank(), model: '', requests: 2}];");
  const rows = run('applyModelFilter(DATA.rows)');
  assert.equal(rows.length, 1);
  assert.equal(rows[0].requests, 2);
});

test('agent and cache breakdown modes render metrics and support filtering', () => {
  const { run, get } = dashboard();
  run(`DATA.rows = [
    {...blank(), day: '2026-01-01', agent: 'Pi', model: 'claude-3-7-sonnet', input: 1000, cache_read: 4000, cache_write: 500, output: 500, requests: 10, cost: 2.50, cost_known: true, savings: 5.20},
    {...blank(), day: '2026-01-01', agent: 'Codex', model: 'gpt-4o', input: 2000, cache_read: 1000, cache_write: 0, output: 1000, requests: 5, cost: 1.50, cost_known: true, savings: 1.10}
  ];
  DATA.sessions = [{day: '2026-01-01', agent: 'Pi', sessions: 3}, {day: '2026-01-01', agent: 'Codex', sessions: 2}];
  `);

  // Test Agent breakdown mode
  run("breakMode = 'agent'; renderBreakdown();");
  let html = get('breakdown').innerHTML;
  assert.match(html, /Pi/);
  assert.match(html, /Codex/);
  assert.match(html, /3 sess/);
  assert.match(html, /Agent/);

  // Test Cache breakdown mode
  run("breakMode = 'cache'; renderBreakdown();");
  html = get('breakdown').innerHTML;
  assert.match(html, /claude-3-7-sonnet/);
  assert.match(html, /gpt-4o/);
  assert.match(html, /Saved/);
  assert.match(html, /Hit % · Read/);
  // Claude had 4000 read out of 5500 prompt tokens (4000/(1000+4000+500)) = 72.7%
  assert.match(html, /72\.7% · 4\.0K/);
  assert.match(html, /\$5\.20/);

  // Cache mode data structure
  const cacheData = run('breakdownData()');
  assert.equal(cacheData.label, 'Model (Cache)');
  assert.equal(cacheData.rows[0].key, 'claude-3-7-sonnet');
  assert.equal(cacheData.rows[0].rawSavings, 5.20);
});

test('rate limits render Tokemon-style health badges, pace markers, and burn velocity', () => {
  const { run, get } = dashboard();
  const nowSec = Math.floor(Date.now() / 1000);
  run(`DATA.limits = [
    { agent: 'Codex', window: '5h', used_percent: 85, window_minutes: 300, resets_at: ${nowSec + 3600}, plan: 'Plus' },
    { agent: 'Codex', window: 'weekly', used_percent: 15, window_minutes: 10080, resets_at: ${nowSec + 86400 * 5}, plan: 'Plus' }
  ];
  renderLimits();`);

  const html = get('limitsView').innerHTML;
  assert.match(html, /badge-pace/);
  assert.match(html, /Exhaustion Risk|Cap Imminent/);
  assert.match(html, /On Pace/);
  assert.match(html, /pace-marker/);
  assert.match(html, /⚡/);
  assert.match(html, /exhaustion in/);
});

test('dashboard totals and stacked bar display prompt cache stats and percentages', () => {
  const { run, get } = dashboard();
  run(`DATA.rows = [
    {...blank(), day: '2026-01-01', agent: 'Pi', model: 'claude-3-7-sonnet', input: 2000, cache_read: 8000, cache_write: 1000, output: 1000, cost: 4.0, cost_known: true, savings: 9.50}
  ];
  DATA.sessions = [{day: '2026-01-01', agent: 'Pi', sessions: 1}];
  render();`);

  const totalsHtml = get('totals').innerHTML;
  assert.match(totalsHtml, /Cache hit rate/);
  assert.match(totalsHtml, /72\.7%/);
  assert.match(totalsHtml, /Cache savings/);
  assert.match(totalsHtml, /\$9\.50/);
  assert.match(totalsHtml, /Prompt cached/);
  assert.match(totalsHtml, /8\.0K/);

  const legendHtml = get('stackLegend').innerHTML;
  assert.match(legendHtml, /Cache read/);
  assert.match(legendHtml, /legend-pct/);
});

test("today is the reader's own day, not the UTC one", () => {
  const { run } = dashboard();
  const now = new Date();
  const local = new Date(now.getTime() - now.getTimezoneOffset() * 60000).toISOString().slice(0, 10);
  const utc = now.toISOString().slice(0, 10);
  // The collector files a row under the reader's own day, so the day the
  // dashboard calls "today" has to be that day.
  assert.equal(run('todayStr()'), local);
  // Off UTC the two differ, and the dashboard has to follow the reader.
  if (local !== utc) assert.notEqual(run('todayStr()'), utc);
  // And the window that ends today ends on that day.
  const yesterday = new Date(now.getTime() - now.getTimezoneOffset() * 60000 - 86400000)
    .toISOString().slice(0, 10);
  assert.equal(run('todayStr(-1)'), yesterday);
});
