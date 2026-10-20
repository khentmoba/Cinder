const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/main.js'), 'utf8');

function setupContext() {
  const elements = new Map();
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
      __TAURI__: { core: { invoke: () => new Promise(() => {}) } },
      addEventListener: () => {},
    },
    document: { body: element(), getElementById: get, querySelector: get, querySelectorAll: () => [] },
  });
  vm.runInContext(source, context);
  return code => vm.runInContext(code, context);
}

test('GLM models map to GLM logo instead of Alibaba Cloud', () => {
  const run = setupContext();
  assert.equal(run('providerLogo("glm-5.3-flash")'), 'icons/glm.svg');
  assert.equal(run('providerLogo("z-ai/glm-5.3-flash")'), 'icons/glm.svg');
  assert.equal(run('providerLogo("opencode-go/glm-5.1")'), 'icons/glm.svg');
  assert.equal(run('providerLogo("chatglm-6b")'), 'icons/glm.svg');
  assert.notEqual(run('providerLogo("glm-5.3-flash")'), 'icons/alibabacloud.svg');
});

test('Gemini models map to Google Gemini logo instead of Antigravity', () => {
  const run = setupContext();
  assert.equal(run('providerLogo("gemini-3.8-flash")'), 'icons/googlegemini.svg');
  assert.equal(run('providerLogo("google/gemini-3.7-flash")'), 'icons/googlegemini.svg');
  assert.equal(run('providerLogo("gemini-3.1-pro")'), 'icons/googlegemini.svg');
  assert.equal(run('providerLogo("gemini-2.5-flash")'), 'icons/googlegemini.svg');
  assert.notEqual(run('providerLogo("gemini-3.8-flash")'), 'icons/antigravity.png');
});

test('model name matching handles case-insensitivity and proxy routes correctly', () => {
  const run = setupContext();
  // Case insensitivity
  assert.equal(run('providerLogo("Claude Opus 4")'), 'icons/anthropic.svg');
  assert.equal(run('providerLogo("claude-sonnet-4-6")'), 'icons/anthropic.svg');

  // Proxy routes (opencode-go, custom) resolve to the underlying model vendor
  assert.equal(run('providerLogo("opencode-go/qwen3.7-max")'), 'icons/alibabacloud.svg');
  assert.equal(run('providerLogo("opencode-go/deepseek-v4-pro")'), 'icons/deepseek.svg');
  assert.equal(run('providerLogo("opencode-go/kimi-k2.6")'), 'icons/kimi.svg');
  assert.equal(run('providerLogo("opencode-go/mimo-v2.5")'), 'icons/xiaomi.svg');
  assert.equal(run('providerLogo("custom/muse-spark-1.3-contributor")'), 'icons/meta.svg');

  // OpenCode proprietary models
  assert.equal(run('providerLogo("space-bunny-free")'), 'icons/opencode.svg');
  assert.equal(run('providerLogo("opencode-go/ox-alpha")'), 'icons/opencode.svg');
});

test('all referenced icon files exist on disk and are valid SVGs or PNGs', () => {
  const run = setupContext();
  const sampleModels = [
    'glm-5.3-flash',
    'gemini-3.8-flash',
    'qwen3.8-flash',
    'deepseek-v4-flash',
    'gpt-5.5',
    'claude-sonnet-4-6',
    'muse-spark-1.3',
    'mimo-v2.5',
    'kimi-k2.7-code',
    'mistral-large',
    'space-bunny-free',
    'opencode-go/minimax-m3',
  ];
  for (const m of sampleModels) {
    const iconPath = run(`providerLogo(${JSON.stringify(m)})`);
    assert.ok(iconPath, `Expected icon for model ${m}`);
    const fullPath = path.join(__dirname, '../src', iconPath);
    assert.ok(fs.existsSync(fullPath), `Icon file ${fullPath} should exist on disk`);
  }
});
