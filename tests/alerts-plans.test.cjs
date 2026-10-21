const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/main.js'), 'utf8');
const markup = fs.readFileSync(path.join(__dirname, '../src/index.html'), 'utf8');

// The harness runs main.js in a vm with a fake DOM. The backend commands can
// be answered; everything else stays pending, the way a scan in progress
// does.
function dashboard(commands = {}) {
  const elements = new Map();
  const handlers = {};
  const element = () => ({
    innerHTML: '', textContent: '', hidden: false, className: '', title: '',
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
      __TAURI__: { core: { invoke: (cmd, args) => (commands[cmd] ? commands[cmd](args) : new Promise(() => {})) } },
      addEventListener: (name, callback) => { handlers[name] = callback; },
    },
    document: { body: element(), getElementById: get, querySelector: get, querySelectorAll: () => [] },
  });
  vm.runInContext(source, context);
  const run = code => vm.runInContext(code, context);
  run("rangeDays = 0; DATA = { rows: [], sessions: [], limits: [] };");
  return { run, get, handlers };
}

// let the pending promises settle
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test('the alerts banner names what is wrong, and stays away when nothing is', () => {
  const { run, get } = dashboard();
  run(`DATA.alerts = [
    { id: 'quota:Claude Code:5h', agent: 'Claude Code', severity: 'warn', title: 'Claude Code 5h window is on pace to use the whole window', detail: '40% used, on pace for 100%' },
    { id: 'budget:Pi', agent: 'Pi', severity: 'critical', title: 'Pi is over its budget', detail: '$50.00 of $40.00' }
  ];
  renderLimits();`);
  const html = get('limitsView').innerHTML;
  assert.match(html, /Claude Code 5h window is on pace to use the whole window/);
  assert.match(html, /40% used, on pace for 100%/);
  assert.match(html, /Pi is over its budget/);
  assert.match(html, /\$50\.00 of \$40\.00/);
  // the severity drives the colour
  assert.match(html, /var\(--warn\)/);
  assert.match(html, /var\(--bad\)/);

  // no alerts, no section at all
  run("DATA.alerts = []; renderLimits();");
  assert.doesNotMatch(get('limitsView').innerHTML, /alert-row/);
});

test('the header badge counts the alerts and disappears with them', () => {
  const { run, get } = dashboard();
  run(`DATA.alerts = [
    { id: 'a', agent: 'Claude Code', severity: 'warn', title: 'one', detail: 'first' },
    { id: 'b', agent: 'Pi', severity: 'critical', title: 'two', detail: 'second' }
  ];
  render();`);
  const btn = get('#view button[data-v="limits"]');
  assert.match(btn.innerHTML, /alert-badge/);
  assert.match(btn.innerHTML, />2</);
  assert.equal(get('limitsAlertBadge').textContent, '2');

  run("DATA.alerts = []; render();");
  assert.doesNotMatch(get('#view button[data-v="limits"]').innerHTML, /alert-badge/);
});

test('a budget reads as a burn-down', () => {
  const { run, get } = dashboard();
  run(`DATA.plans = [{ agent: 'Pi', kind: 'budget', label: 'monthly', limit: 50, used: 12.4, percent: 24.8, multiple: 0.248 }];
  renderLimits();`);
  const html = get('limitsView').innerHTML;
  assert.match(html, /\$12\.40 of your \$50\.00 budget \(25%\)/);
  assert.match(html, /class="track"/);
  assert.match(html, /class="bar"/);
});

test('a subscription says what the plan costs and what the work was worth', () => {
  const { run, get } = dashboard();
  run(`DATA.plans = [{ agent: 'Claude Code', kind: 'subscription', label: 'Claude Pro', limit: 20, used: 142.5, percent: 712.5, multiple: 7.125 }];
  renderLimits();`);
  const html = get('limitsView').innerHTML;
  assert.match(html, /Claude Pro · \$20\.00 a month · the work was worth \$142\.50 \(7\.1x the plan\)/);
  // a plan is not something that gets used up: no burn-down wording, no bar
  assert.doesNotMatch(html, /of your \$/);
  assert.doesNotMatch(html, /class="track"/);
  // and the figure is the value of the work, never a bill
  assert.doesNotMatch(html, /bill/i);
});

test('a plan that was never declared shows nothing at all', () => {
  const { run, get } = dashboard();
  run("DATA.plans = []; renderLimits();");
  assert.doesNotMatch(get('limitsView').innerHTML, /plan-row/);
});

test('a window the provider counts in money says so in money', () => {
  const { run, get } = dashboard();
  const nowSec = Math.floor(Date.now() / 1000);
  run(`DATA.limits = [
    { agent: 'Claude Code', window: '5h', used_percent: 50, window_minutes: 300, resets_at: ${nowSec + 3600}, used_amount: 25, limit_amount: 50 },
    { agent: 'Codex', window: 'weekly', used_percent: 15, window_minutes: 10080, resets_at: ${nowSec + 86400}, used_amount: 0, limit_amount: 0 }
  ];
  renderLimits();`);
  const html = get('limitsView').innerHTML;
  assert.match(html, /\$25\.00 of \$50\.00/);
  // the window without an allowance in money keeps to the percentage
  assert.equal((html.match(/of \$/g) || []).length, 1);
});

test('the plans tab lists the declarations that were loaded', async () => {
  const { run, get } = dashboard({
    get_plans: () => Promise.resolve([
      { agent: 'Claude Code', kind: 'subscription', limit: 20, label: 'Claude Pro' },
      { agent: 'Pi', kind: 'budget', limit: 50, label: 'monthly' },
    ]),
  });
  run("openSettings('plans')");
  await settle();
  const html = get('plansList').innerHTML;
  assert.match(html, /Claude Pro/);
  assert.match(html, /Claude Code/);
  assert.match(html, /monthly/);
  assert.match(html, /value="subscription" selected/);
  assert.match(html, /value="budget" selected/);
  assert.match(html, /value="20"/);
});

test('saving sends the declarations and drops the rows without a limit', async () => {
  let sent = null;
  const { run } = dashboard({
    get_plans: () => Promise.resolve([]),
    set_plans: args => { sent = args.plans; return Promise.resolve(args.plans); },
  });
  run(`PLANS = [
    { agent: 'Claude Code', kind: 'subscription', limit: 20, label: 'Claude Pro' },
    { agent: 'Pi', kind: 'budget', limit: 0, label: 'empty' }
  ];
  savePlans();`);
  await settle();
  assert.equal(JSON.stringify(sent), JSON.stringify([
    { agent: 'Claude Code', kind: 'subscription', limit: 20, label: 'Claude Pro' },
  ]));
});

test('a plans tab that cannot reach the backend still opens', async () => {
  const { run, get } = dashboard({ get_plans: () => Promise.reject('not in Tauri') });
  run("openSettings('plans')");
  await settle();
  assert.equal(get('panelPlans').hidden, false);
  assert.doesNotThrow(() => run('render()'));
});

test('the plans tab sits between preferences and data sources', () => {
  const prefs = markup.indexOf('data-tab="preferences"');
  const plans = markup.indexOf('data-tab="plans"');
  const sources = markup.indexOf('data-tab="sources"');
  assert.ok(plans > prefs, 'the plans tab comes after preferences');
  assert.ok(plans < sources, 'the plans tab comes before data sources');
  assert.match(markup, /id="panelPlans"/);
});
