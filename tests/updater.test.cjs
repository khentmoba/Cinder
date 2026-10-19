const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/main.js'), 'utf8');

function setupUpdaterContext(invokeMock = () => new Promise(() => {})) {
  const elements = new Map();
  const handlers = {};
  const element = (id) => ({
    id,
    innerHTML: '',
    textContent: '',
    hidden: false,
    className: '',
    style: {},
    dataset: {},
    disabled: false,
    href: '',
    clientWidth: 800,
    clientHeight: 400,
    classList: {
      add() {},
      remove() {},
      toggle() {},
    },
    addEventListener(name, fn) {
      handlers[`${id}:${name}`] = fn;
    },
    setAttribute() {},
    querySelector: () => element(),
    querySelectorAll: () => [],
    closest: () => null,
  });

  const get = (id) => {
    if (!elements.has(id)) elements.set(id, element(id));
    return elements.get(id);
  };

  const eventListeners = {};
  const context = vm.createContext({
    window: {
      __TAURI__: {
        core: { invoke: invokeMock },
        event: {
          listen: (name, cb) => {
            eventListeners[name] = cb;
          },
        },
      },
      addEventListener: (name, cb) => {
        handlers[name] = cb;
      },
      open: () => {},
    },
    document: {
      body: element('body'),
      getElementById: get,
      querySelector: get,
      querySelectorAll: () => [],
    },
    setTimeout: (fn) => fn(),
  });

  vm.runInContext(source, context);
  const run = (code) => vm.runInContext(code, context);
  return { run, get, handlers, eventListeners, context };
}

test('fmtBytes formats units properly', () => {
  const { run } = setupUpdaterContext();
  assert.equal(run('fmtBytes(0)'), '0 B');
  assert.equal(run('fmtBytes(512)'), '512 B');
  assert.equal(run('fmtBytes(1024)'), '1.0 KB');
  assert.equal(run('fmtBytes(1048576)'), '1.0 MB');
  assert.equal(run('fmtBytes(26214400)'), '25.0 MB');
  assert.equal(run('fmtBytes(1073741824)'), '1.00 GB');
});

test('checkForUpdates displays badge and links when update is available', async () => {
  const mockUpdate = {
    available: true,
    current_version: '0.1.0',
    latest_version: '0.2.0',
    release_name: 'Cinder v0.2.0',
    release_notes: 'Exciting new features',
    published_at: '2026-10-19T12:00:00Z',
    asset_name: 'Cinder_0.2.0_x64-setup.exe',
    asset_size: 25000000,
    download_url: 'https://example.com/Cinder_setup.exe',
    release_url: 'https://github.com/khentmoba/Cinder/releases/tag/v0.2.0',
  };

  const { run, get } = setupUpdaterContext((cmd) => {
    if (cmd === 'check_update') return Promise.resolve(mockUpdate);
    if (cmd === 'get_app_version') return Promise.resolve('0.1.0');
    return Promise.resolve();
  });

  await run('checkForUpdates(false)');

  assert.equal(get('updateBadge').hidden, false);
  assert.equal(get('updateBadgeText').textContent, 'v0.2.0 available');
  assert.equal(get('updateStatusText').textContent, '✦ Update v0.2.0');
  assert.equal(get('updateStatusText').hidden, false);
});

test('showUpdateModal fills release metadata and controls', () => {
  const { run, get } = setupUpdaterContext();
  const sample = {
    available: true,
    current_version: '0.1.0',
    latest_version: '0.2.0',
    release_name: 'Cinder v0.2.0',
    release_notes: 'Fixed bugs\nAdded update system',
    published_at: '2026-10-19T12:00:00Z',
    asset_name: 'Cinder_0.2.0_x64-setup.exe',
    asset_size: 25000000,
    download_url: 'https://example.com/Cinder_setup.exe',
    release_url: 'https://github.com/khentmoba/Cinder/releases/tag/v0.2.0',
  };

  run(`showUpdateModal(${JSON.stringify(sample)}, false)`);

  assert.equal(get('updateModal').hidden, false);
  assert.equal(get('modalTag').textContent, 'v0.2.0');
  assert.equal(get('updateCurrentVer').textContent, 'v0.1.0');
  assert.equal(get('updateNewVer').textContent, 'v0.2.0');
  assert.equal(get('updateNotes').textContent, sample.release_notes);
  assert.equal(get('modalGithubBtn').href, sample.release_url);
  assert.equal(get('modalActionBtn').textContent, 'Update Now');

  run('hideUpdateModal()');
  assert.equal(get('updateModal').hidden, true);
});

test('showUpdateModal handles up-to-date state', () => {
  const { run, get } = setupUpdaterContext();
  const upToDate = {
    available: false,
    current_version: '0.1.0',
    latest_version: '0.1.0',
  };

  run(`showUpdateModal(${JSON.stringify(upToDate)}, true)`);
  assert.equal(get('updateModal').hidden, false);
  assert.equal(get('modalTag').textContent, 'Latest');
  assert.match(get('updateMsg').textContent, /latest version/);
  assert.equal(get('modalActionBtn').textContent, 'OK');
});

test('updateProgressUI updates progress bar width and labels', () => {
  const { run, get } = setupUpdaterContext();

  run('updateProgressUI(10485760, 20971520, 50, false)');
  assert.equal(get('updateProgressBox').hidden, false);
  assert.equal(get('progressBar').style.width, '50%');
  assert.equal(get('progressPercent').textContent, '50%');
  assert.equal(get('progressSub').textContent, '10.0 MB / 20.0 MB');

  run('updateProgressUI(20971520, 20971520, 100, true)');
  assert.equal(get('progressBar').style.width, '100%');
  assert.equal(get('progressPercent').textContent, '100%');
  assert.match(get('progressLabel').textContent, /restarting/);
});

test('settings button and openSettings open the modal', () => {
  const { run, get, handlers } = setupUpdaterContext();

  assert.equal(get('updateModal').hidden, false); // initial default in mock is false, let us hide it
  run('hideUpdateModal()');
  assert.equal(get('updateModal').hidden, true);

  // Clicking settingsBtn should open settings
  const settingsHandler = handlers['settingsBtn:click'];
  assert.ok(settingsHandler, 'settingsBtn should have a click handler');
  settingsHandler();
  assert.equal(get('updateModal').hidden, false);
});

test('switchSettingsTab activates selected panel and tab', () => {
  const { run, get } = setupUpdaterContext();

  run("switchSettingsTab('preferences')");
  assert.equal(get('panelPreferences').hidden, false);

  run("switchSettingsTab('sources')");
  assert.equal(get('panelSources').hidden, false);

  run("switchSettingsTab('about')");
  assert.equal(get('panelAbout').hidden, false);

  run("switchSettingsTab('updates')");
  assert.equal(get('panelUpdates').hidden, false);
});

test('checkForUpdates displays update dot on settings button when update available', async () => {
  const mockUpdate = {
    available: true,
    current_version: '0.1.0',
    latest_version: '0.2.0',
    release_name: 'Cinder v0.2.0',
    release_notes: 'Exciting new features',
    published_at: '2026-10-19T12:00:00Z',
    asset_name: 'Cinder_0.2.0_x64-setup.exe',
    asset_size: 25000000,
    download_url: 'https://example.com/Cinder_setup.exe',
    release_url: 'https://github.com/khentmoba/Cinder/releases/tag/v0.2.0',
  };

  const { run, get } = setupUpdaterContext((cmd) => {
    if (cmd === 'check_update') return Promise.resolve(mockUpdate);
    if (cmd === 'get_app_version') return Promise.resolve('0.1.0');
    return Promise.resolve();
  });

  await run('checkForUpdates(false)');

  assert.equal(get('settingsUpdateDot').hidden, false);
  assert.equal(get('tabUpdateDot').hidden, false);
});
