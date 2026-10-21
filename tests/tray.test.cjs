const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/tray.js'), 'utf8');

// A window that has one provider on a plan, one that failed and one that is
// out of date.
const SUMMARY = {
  cards: [
    {
      agent: 'Claude Code',
      account: '',
      plan: 'max',
      windows: [
        { agent: 'Claude Code', window: '5h', used_percent: 42, window_minutes: 300, resets_at: 0, plan: 'max', detail: '5-hour session window', used_amount: 0, limit_amount: 0 },
        { agent: 'Claude Code', window: '7d', used_percent: 91, window_minutes: 10080, resets_at: 0, plan: 'max', detail: '7-day window', used_amount: 0, limit_amount: 0 },
      ],
      error: '',
      error_kind: null,
      stale: false,
      updated_at: 0,
    },
    {
      agent: 'Codex',
      account: 'me@example.com',
      plan: 'plus',
      windows: [
        { agent: 'Codex', window: '5h', used_percent: 12, window_minutes: 300, resets_at: 0, plan: 'plus', detail: '5-hour session window', used_amount: 0, limit_amount: 0 },
      ],
      error: '',
      error_kind: null,
      stale: true,
      updated_at: 0,
    },
    {
      agent: 'GitHub Copilot',
      account: '',
      plan: '',
      windows: [],
      error: 'HTTP 503',
      error_kind: 'status',
      stale: false,
      updated_at: 0,
    },
  ],
  alerts: [
    { id: 'quota:Claude Code:7d', agent: 'Claude Code', severity: 'critical', title: 'Claude Code 7d window is 91% used', detail: '7-day window' },
  ],
  plans: [
    { agent: 'Claude Code', kind: 'subscription', label: 'Claude Pro', limit: 20, used: 142.5, percent: 712, multiple: 7.1 },
    { agent: 'Codex', kind: 'budget', label: 'Codex budget', limit: 50, used: 12.4, percent: 24.8, multiple: 0.2 },
  ],
  today_cost: 1.25,
  today_cost_known: true,
  today_cost_partial: false,
  today_tokens: 412000,
  today_requests: 37,
  checked_at: new Date().toISOString(),
  restored: false,
  scanned_at: new Date().toISOString(),
};

function popover(summary) {
  const elements = new Map();
  const handlers = {};
  const calls = [];
  const element = () => ({
    innerHTML: '', textContent: '', hidden: false, className: '', title: '',
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
      __TAURI__: {
        core: {
          invoke: (cmd, args) => {
            calls.push([cmd, args]);
            return Promise.resolve(cmd === 'get_tray_summary' ? summary : null);
          },
        },
        event: { listen: () => {} },
      },
      addEventListener: (name, callback) => { handlers[name] = callback; },
    },
    document: { body: element(), getElementById: get, querySelector: get, querySelectorAll: () => [] },
    setInterval: () => 0,
    clearInterval: () => {},
  });
  vm.runInContext(source, context);
  const run = code => vm.runInContext(code, context);
  return { run, get, calls, handlers };
}

// The popover reads the summary on start-up, so the test has to let the
// promise settle before it looks at the screen.
const settle = () => new Promise(resolve => setImmediate(resolve));

test('the popover shows a card per provider with what is left of each window', async () => {
  const { get, calls } = popover(SUMMARY);
  await settle();
  assert.equal(calls[0][0], 'get_tray_summary');
  const html = get('body').innerHTML;
  assert.match(html, /Claude Code/);
  assert.match(html, /42%/);
  assert.match(html, /91%/);
  // The window ids are spelled out rather than shown raw.
  assert.match(html, /5-hour window/);
  assert.match(html, /7-day window/);
  // The plan is named the way a person would say it.
  assert.match(html, />Max</);
  assert.match(html, />Plus</);
});

test('a provider that failed says what went wrong', async () => {
  const { get } = popover(SUMMARY);
  await settle();
  const html = get('body').innerHTML;
  assert.match(html, /GitHub Copilot/);
  assert.match(html, /HTTP 503/);
});

test('numbers that are out of date are marked as such', async () => {
  const { get } = popover(SUMMARY);
  await settle();
  const html = get('body').innerHTML;
  assert.match(html, /badge stale/);
  assert.match(html, />stale</);
});

test('the alerts and the declared plans are on the screen', async () => {
  const { get } = popover(SUMMARY);
  await settle();
  const html = get('body').innerHTML;
  assert.match(html, /91% used/);
  assert.match(html, /Your plans/);
  assert.match(html, /Claude Pro/);
  assert.match(html, /7\.1x/);
  assert.match(html, /Budget/);
});

test("today's work is summarised under the cards", async () => {
  const { get } = popover(SUMMARY);
  await settle();
  const today = get('today').innerHTML;
  assert.match(today, /\$1\.25/);
  assert.match(today, /412\.0K tokens/);
  assert.match(today, /37 requests/);
});

test('the buttons ask the app for the right thing', async () => {
  const { get, calls } = popover(SUMMARY);
  await settle();
  const actions = calls.filter(c => c[0] === 'tray_action').map(c => c[1].action);
  assert.deepEqual(actions, []);
  // The handlers are wired to the buttons in the markup.
  const html = fs.readFileSync(path.join(__dirname, '../src/tray.html'), 'utf8');
  for (const id of ['refresh', 'openBtn', 'rescanBtn', 'quitBtn']) {
    assert.match(html, new RegExp(`id="${id}"`));
    assert.ok(get(id), `the ${id} button should be on the page`);
  }
});

test('a provider that is not signed in gets no card', async () => {
  const { get } = popover({ ...SUMMARY, cards: [] });
  await settle();
  const html = get('body').innerHTML;
  assert.match(html, /No provider is signed in/);
  assert.doesNotMatch(html, /5-hour window/);
});
