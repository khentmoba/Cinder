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
    assert.match(html, /data-v="1">Today \(UTC\)<\/button>/);
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
