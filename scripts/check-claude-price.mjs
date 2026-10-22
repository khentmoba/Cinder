// The price the Claude Code CLI charged, per model, against the price Cinder
// would use.
//
//   node scripts/check-claude-price.mjs
//
// The CLI writes a `cost-state` line per session: what it charged, and the
// per-model usage that made up the charge. Cinder prices the same tokens out
// of its own catalogue, so the two can be compared - the same way the token
// counts are compared.
//
// Two things are priced here the way the CLI prices them: a cache write that
// lasts an hour costs twice the input price, and the thinking tokens are
// counted as output.
//
// A model whose requests went past the long-context threshold is reported as
// a difference: Anthropic charges a premium past that point, and Cinder
// estimates with one flat rate, so the two legitimately differ.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

function walk(dir, out = []) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const e of entries) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (e.name.endsWith('.jsonl')) out.push(p);
  }
  return out;
}

// The catalogue Cinder prices with: the bundled list, with the fetched one on
// top of it, exactly as the collector loads it.
function loadPrices() {
  const out = new Map();
  const files = [
    join(process.cwd(), 'src-tauri', 'prices.json'),
    join(homedir(), '.cinder', 'prices.json'),
  ];
  for (const file of files) {
    let j;
    try {
      j = JSON.parse(readFileSync(file, 'utf8').replace(/^\uFEFF/, ''));
    } catch {
      continue;
    }
    for (const [id, p] of Object.entries(j.models || {})) {
      if (typeof p?.i !== 'number') continue;
      out.set(id.toLowerCase(), p);
    }
  }
  return out;
}

const prices = loadPrices();

// The name the CLI uses: "claude-opus-5-5[1m]" is the 1M-context variant.
function priceFor(model) {
  const bare = model.replace(/\[[^\]]*\]/g, '').toLowerCase();
  return prices.get(bare) || null;
}

const perModel = new Map();
let sessions = 0;
let seen = 0;

for (const file of walk(join(homedir(), '.claude', 'projects'))) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  for (const raw of text.split('\n')) {
    if (!raw.includes('"type":"cost-state"')) continue;
    let v;
    try {
      v = JSON.parse(raw);
    } catch {
      continue;
    }
    const usage = v.modelUsage;
    if (!usage || typeof usage !== 'object') continue;
    sessions++;
    for (const [model, u] of Object.entries(usage)) {
      if (!u || typeof u.costUSD !== 'number' || u.costUSD === 0) continue;
      seen++;
      const e = perModel.get(model) || { calls: 0, charged: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
      e.calls++;
      e.charged += u.costUSD;
      e.input += u.inputTokens || 0;
      e.output += u.outputTokens || 0;
      e.cacheRead += u.cacheReadInputTokens || 0;
      e.cacheWrite += u.cacheCreationInputTokens || 0;
      perModel.set(model, e);
    }
  }
}

console.log(`cost-state lines: ${sessions}, priced models: ${seen}`);
let worst = 0;
let worstModel = '';
for (const [model, e] of [...perModel.entries()].sort()) {
  const p = priceFor(model);
  let line = `${model.padEnd(28)} charged $${e.charged.toFixed(4)}`;
  if (!p) {
    line += '  no catalogue price';
  } else {
    const estimate =
      (e.input * p.i + e.output * p.o + e.cacheRead * (p.cr || 0) + e.cacheWrite * (p.i * 2)) / 1e6;
    const diff = estimate - e.charged;
    const pct = e.charged > 0 ? (diff / e.charged) * 100 : 0;
    line += `  list price $${estimate.toFixed(4)}  diff ${pct >= 0 ? '+' : ''}${pct.toFixed(1)}%`;
    if (Math.abs(pct) > worst) {
      worst = Math.abs(pct);
      worstModel = model;
    }
  }
  console.log(line);
}
console.log(`largest difference: ${worst.toFixed(1)}% (${worstModel})`);
if (/\[1m\]$/.test(worstModel)) {
  console.log('The long-context variant is charged a premium past the context threshold, which a flat list price does not model.');
}
