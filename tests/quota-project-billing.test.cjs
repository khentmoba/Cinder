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

// usage rows, with the zero fields the backend always sends filled in
function rows(run, list) {
  run(`DATA.rows = ${JSON.stringify(list)}.map(r => ({ ...blank(), ...r }));`);
}

test('the billing badge and the hero subtitle name how the money is paid for', () => {
  const { run, get } = dashboard();
  run("metric = 'cost';");
  rows(run, [
    { day: '2026-01-01', agent: 'Claude Code', model: 'm', requests: 4, input: 100, cost: 2, cost_known: true, billing: 'subscription' },
  ]);
  run('render()');
  assert.match(get('bigBadge').textContent, /Plan/);
  assert.match(get('bigSub').textContent, /API-equivalent value — covered by your plan/);
  assert.match(get('bigSub').textContent, /4 requests/);
  assert.match(get('agentList').innerHTML, /badge-plan/);

  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 4, input: 100, cost: 2, cost_known: true, billing: 'api' },
  ]);
  run('render()');
  assert.match(get('bigBadge').textContent, /API/);
  assert.match(get('bigSub').textContent, /Direct API spend/);
  assert.match(get('agentList').innerHTML, /badge-api/);

  rows(run, [
    { day: '2026-01-01', agent: 'LM Studio', model: 'local-model', requests: 4, input: 100, cost: 0, cost_known: true, billing: 'local' },
  ]);
  run('render()');
  assert.match(get('bigBadge').textContent, /Local/);
  assert.match(get('bigSub').textContent, /Local inference — zero API cost/);
  assert.match(get('agentList').innerHTML, /badge-local/);
});

test('rows without a known billing mode get no badge and no reading', () => {
  const { run, get } = dashboard();
  run("metric = 'cost';");
  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 4, input: 100, cost: 2, cost_known: true, billing: 'unknown' },
  ]);
  run('render()');
  assert.equal(get('bigBadge').textContent, '');
  assert.doesNotMatch(get('bigSub').textContent, /Direct API spend/);
  assert.doesNotMatch(get('agentList').innerHTML, /badge-/);
});

test('the dominant billing mode is the one covering the most money', () => {
  const { run } = dashboard();
  run("metric = 'cost';");
  rows(run, [
    { day: '2026-01-01', agent: 'Claude Code', model: 'm', requests: 1, input: 100, cost: 9, cost_known: true, billing: 'subscription' },
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 1, input: 100, cost: 1, cost_known: true, billing: 'api' },
  ]);
  run('render()');
  assert.equal(run('byAgent().grand.billing'), 'subscription');

  rows(run, [
    { day: '2026-01-01', agent: 'Claude Code', model: 'm', requests: 1, input: 100, cost: 1, cost_known: true, billing: 'subscription' },
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 9, input: 100, cost: 9, cost_known: true, billing: 'api' },
  ]);
  run('render()');
  assert.equal(run('byAgent().grand.billing'), 'api');
});

test('a fully local setup reads as local, and a local run cannot hijack a plan total', () => {
  const { run, get } = dashboard();
  run("metric = 'cost';");
  rows(run, [
    { day: '2026-01-01', agent: 'LM Studio', model: 'local', requests: 9, input: 100, cost: 0, cost_known: true, billing: 'local', cloud_savings: 4.25 },
  ]);
  run('render()');
  assert.equal(run('byAgent().grand.billing'), 'local');
  assert.match(get('bigSub').textContent, /Local inference — zero API cost/);

  // a local run that saved a lot is not what the total is made of
  rows(run, [
    { day: '2026-01-01', agent: 'LM Studio', model: 'local', requests: 9, input: 100, cost: 0, cost_known: true, billing: 'local', cloud_savings: 40 },
    { day: '2026-01-01', agent: 'Claude Code', model: 'm', requests: 1, input: 10, cost: 1.5, cost_known: true, billing: 'subscription' },
  ]);
  run('render()');
  assert.equal(run('byAgent().grand.billing'), 'subscription');
  assert.match(get('bigSub').textContent, /API-equivalent value — covered by your plan/);
});

test('the tokens and requests subtitle stays the way it was', () => {  const { run, get } = dashboard();
  run("metric = 'tokens';");
  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 4, input: 100, tools: 2, cost: 2, cost_known: true, billing: 'subscription' },
  ]);
  run('render()');
  assert.equal(get('bigSub').textContent, '0 sessions · 4 requests · 2 tool calls');
});

test('the project breakdown groups the rows by repository', () => {
  const { run, get } = dashboard();
  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 2, input: 100, cost: 1, cost_known: true, project: 'cinder' },
    { day: '2026-01-01', agent: 'Codex', model: 'm', requests: 3, input: 300, cost: 3, cost_known: true, project: 'cinder' },
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 1, input: 50, cost: 0.5, cost_known: true, project: 'mock' },
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 4, input: 10, cost: 0.1, cost_known: true, project: '' },
  ]);
  run("breakMode = 'project'; renderBreakdown();");
  const html = get('breakdown').innerHTML;
  assert.match(html, /Project/);
  assert.match(html, /cinder/);
  assert.match(html, /mock/);
  assert.match(html, /No project/);
  assert.match(html, /2 agents/);

  const data = run('breakdownData()');
  assert.equal(data.label, 'Project');
  assert.equal(data.rows.map(r => r.key).join('|'), 'cinder|mock|');
  assert.equal(data.rows[0].requests, 5);
  assert.equal(data.rows[0].num, '400');

  // the number column follows the metric, like every other breakdown
  run("metric = 'requests';");
  assert.equal(run('breakdownData()').rows[0].num, '5');
});

test('selecting a project narrows every panel and shows up in the breadcrumb', () => {
  const { run, get } = dashboard();
  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 2, input: 100, cost: 1, cost_known: true, project: 'cinder' },
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 1, input: 50, cost: 0.5, cost_known: true, project: 'mock' },
  ]);
  run("breakMode = 'project'; renderBreakdown(); selectBreakdownRow('cinder');");
  assert.equal(run('selectedProject'), 'cinder');
  assert.equal(run('filtered().length'), 1);
  assert.equal(run('byAgent().list.length'), 1);
  assert.equal(run('byModel().list.length'), 1);
  assert.equal(run('byProject().list.length'), 1);
  assert.equal(run('byDay().length'), 1);
  assert.match(get('crumb').innerHTML, /cinder/);
  assert.match(get('crumb').innerHTML, /data-clear="project"/);

  // clicking the same project again lets go of the filter
  run("selectBreakdownRow('cinder');");
  assert.equal(run('selectedProject'), null);
  assert.equal(run('filtered().length'), 2);
});

test('a delegated run is badged and counted, never dropped', () => {
  const { run, get } = dashboard();
  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 3, input: 100, cost: 1, cost_known: true, subagent: true },
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 5, input: 100, cost: 1, cost_known: true },
  ]);
  run('render();');
  assert.match(get('agentList').innerHTML, /3 delegated/);
  assert.equal(run('byAgent().grand.requests'), 8);
  assert.equal(run('byAgent().grand.subagent_requests'), 3);

  run("breakMode = 'model'; renderBreakdown();");
  assert.match(get('breakdown').innerHTML, /badge-sub/);
  assert.match(get('breakdown').innerHTML, /subagent/);
});

test('a local model shows what it saved against a cloud price', () => {
  const { run, get } = dashboard();
  rows(run, [
    { day: '2026-01-01', agent: 'LM Studio', model: 'local', requests: 4, input: 1000, cost: 0, cost_known: true, billing: 'local', cloud_savings: 4.25 },
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 1, input: 10, cost: 1, cost_known: true, billing: 'api' },
  ]);
  run('render();');
  assert.match(get('bigSaved').innerHTML, /Saved vs cloud/);
  assert.match(get('bigSaved').innerHTML, /\$4\.25/);
  assert.equal(get('bigSaved').hidden, false);
  assert.match(get('agentList').innerHTML, /saved \$4\.25 vs cloud/);

  // nothing local, nothing to save
  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 1, input: 10, cost: 1, cost_known: true, billing: 'api' },
  ]);
  run('render();');
  assert.equal(get('bigSaved').hidden, true);
});

test('a live quota carries its own badge, detail and timestamp', () => {
  const { run, get } = dashboard();
  const nowSec = Math.floor(Date.now() / 1000);
  run(`DATA.limits = [
    { agent: 'Claude Code', window: '5h', used_percent: 40, window_minutes: 300, resets_at: ${nowSec + 3600}, plan: 'Max', source: 'live', detail: '5-hour session window' },
    { agent: 'Codex', window: 'weekly', used_percent: 10, window_minutes: 10080, resets_at: ${nowSec + 86400}, plan: 'Plus', source: 'log' }
  ];
  DATA.quota_checked_at = new Date(Date.now() - 3 * 60000).toISOString();
  renderLimits();`);
  const html = get('limitsView').innerHTML;
  assert.match(html, /badge-live/);
  assert.match(html, /5-hour session window/);
  assert.match(html, /updated 3m ago/);
  // the log window keeps the wording it always had
  assert.match(html, /Codex · weekly window/);
  assert.equal((html.match(/badge-live/g) || []).length, 1);
});

test('a quota that could not be read says so', () => {
  const { run, get } = dashboard();
  run(`DATA.limits = [];
  DATA.quota_error = 'no Claude credentials on this machine';
  renderLimits();`);
  const html = get('limitsView').innerHTML;
  assert.match(html, /Live quotas could not be read/);
  assert.match(html, /no Claude credentials on this machine/);
});

test('the artifacts section only shows up when there are artifacts', () => {
  const { run, get } = dashboard();
  run('DATA.artifacts = []; renderArtifacts();');
  assert.equal(get('artifacts').hidden, true);
  assert.equal(get('artifactList').innerHTML, '');

  run(`DATA.artifacts = [
    { agent: 'Claude Code', url: 'https://example.test/a', day: '2026-01-02', title: 'Release notes' },
    { agent: 'Pi', url: 'https://example.test/b', day: '2026-01-03', title: 'Nightly build' }
  ];
  renderArtifacts();`);
  assert.equal(get('artifacts').hidden, false);
  const html = get('artifactList').innerHTML;
  assert.match(html, /href="https:\/\/example\.test\/a"/);
  assert.match(html, /target="_blank"/);
  assert.match(html, /Release notes/);
  assert.match(html, /Claude Code · 2026-01-02/);
  assert.match(html, /Pi · 2026-01-03/);
});

test('the sources panel reports how long each scan took', () => {
  const { run, get } = dashboard();
  run("DATA.sources = [{agent:'Pi', path:'~/.pi/agent/sessions', found:true, files:1204, scan_ms:812}, {agent:'Codex', path:'~/.codex', found:true, files:12, scan_ms:0}]; renderSources();");
  const html = get('sourcesList').innerHTML;
  assert.match(html, /1,204 files · 812 ms/);
  assert.match(html, /12 files<\/div>/);
});

test('the csv export carries the project and billing columns', () => {
  const { run } = dashboard();
  rows(run, [
    { day: '2026-01-01', agent: 'Pi', model: 'm', requests: 2, input: 100, cost: 1, cost_known: true, project: 'cinder', billing: 'subscription' },
  ]);
  run("breakMode = 'project';");
  const projectCsv = run('breakdownCSV()');
  assert.match(projectCsv, /^rank,project,cost_usd/);
  assert.match(projectCsv, /cinder/);
  assert.match(projectCsv, /subscription/);

  run("breakMode = 'model';");
  const modelCsv = run('breakdownCSV()');
  assert.match(modelCsv, /^rank,name,cost_usd/);
  assert.match(modelCsv, /subscription/);
});
