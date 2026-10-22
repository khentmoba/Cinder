// The price the Claude Code CLI charged, against the price Cinder works out.
//
//   node scripts/check-claude-price.mjs
//
// The CLI writes a cost-state line per session: what it charged and the
// per-model usage behind it. Cinder prices the same tokens out of its own
// catalogue, so the two can be compared - the same way the token counts are
// compared.
//
// The estimate is worked out per request, because a long request is priced
// at the premium rate and a flat average would hide that. Two things are
// priced the way the CLI prices them: a cache write that lasts an hour costs
// twice the input price, and the thinking tokens are counted as output.
//
// A session's transcript can run past the CLI's last snapshot, so the
// comparison is made over the calls the snapshot covers.

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

// The cost of one request, at the rates that apply to it.
function costOf(p, u) {
  const prompt = (u.input || 0) + (u.cacheRead || 0) + (u.cacheWrite || 0);
  const tier = p.tier && prompt > p.tier.above ? p.tier : null;
  const i = tier ? tier.i : p.i;
  const o = tier ? tier.o : p.o;
  const cr = tier ? tier.cr : p.cr || 0;
  const cw = tier ? tier.cw : p.cw || 0;
  const cw1h = tier ? tier.cw1h || tier.i * 2 : p.i * 2;
  // The writes that last an hour are billed at twice the input price; the
  // rest at the 5-minute rate.
  const oneHour = Math.min(u.cacheWrite1h || 0, u.cacheWrite || 0);
  const fiveMin = (u.cacheWrite || 0) - oneHour;
  return (
    (u.input * i +
      u.output * o +
      u.cacheRead * cr +
      fiveMin * cw +
      oneHour * cw1h) /
    1e6
  );
}

const perModel = new Map();
let sessions = 0;
let within1 = 0;
let compared = 0;
const errors = [];

for (const file of walk(join(homedir(), '.claude', 'projects'))) {
  const id = file.split(/[\\/]/).pop().replace(/\.jsonl$/, '');
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  const calls = [];
  const seen = new Set();
  let snapshot = null;
  for (const raw of text.split('\n')) {
    if (!raw.includes('"usage"') && !raw.includes('"type":"cost-state"')) continue;
    let v;
    try {
      v = JSON.parse(raw);
    } catch {
      continue;
    }
    if (v.type === 'assistant' && v.message && v.message.usage) {
      const mid = v.message.id;
      if (mid) {
        if (seen.has(mid)) continue;
        seen.add(mid);
      }
      const u = v.message.usage;
      calls.push({
        model: v.message.model || 'unknown',
        input: u.input_tokens || 0,
        output: u.output_tokens || 0,
        cacheRead: u.cache_read_input_tokens || 0,
        cacheWrite: u.cache_creation_input_tokens || 0,
        cacheWrite1h: (u.cache_creation && u.cache_creation.ephemeral_1h_input_tokens) || 0,
      });
    } else if (v.type === 'cost-state' && v.sessionId === id) {
      if (!snapshot || (v.totalDuration || 0) >= (snapshot.totalDuration || 0)) snapshot = v;
    }
  }
  if (!snapshot || !snapshot.modelUsage) continue;

  // The calls the snapshot covers, per model: the transcript runs in order,
  // and the snapshot's totals say how far into it the CLI had got.
  const covered = new Map();
  const used = new Map();
  for (const c of calls) {
    const key = c.model.replace(/\[[^\]]*\]/g, '');
    const e = used.get(key) || { out: 0, read: 0, list: [] };
    e.out += c.output;
    e.read += c.cacheRead;
    e.list.push(c);
    used.set(key, e);
  }

  let charged = 0;
  let estimate = 0;
  let priced = true;
  for (const [model, u] of Object.entries(snapshot.modelUsage)) {
    if (!u.costUSD) continue;
    charged += u.costUSD;
    const p = priceFor(model);
    if (!p) {
      priced = false;
      continue;
    }
    const e = perModel.get(model) || { charged: 0, estimate: 0 };
    e.charged += u.costUSD;
    perModel.set(model, e);
    // The calls of this model the snapshot covers.
    const key = model.replace(/\[[^\]]*\]/g, '');
    const all = (used.get(key) || { list: [] }).list;
    const list = [];
    let out = 0;
    let read = 0;
    for (const c of all) {
      list.push(c);
      out += c.output;
      read += c.cacheRead;
      if (out >= (u.outputTokens || 0) && read >= (u.cacheReadInputTokens || 0)) break;
    }
    covered.set(key, list);
    for (const c of list) {
      estimate += costOf(p, c);
      e.estimate += costOf(p, c);
    }
  }
  if (!priced || !charged) continue;
  sessions++;
  compared++;
  const err = ((estimate - charged) / charged) * 100;
  errors.push(err);
  if (Math.abs(err) < 1) within1++;
}

errors.sort((a, b) => a - b);
const median = errors.length ? errors[Math.floor(errors.length / 2)] : 0;
console.log(`sessions compared: ${compared}, within 1%: ${within1}, median difference: ${median.toFixed(1)}%`);
for (const [model, e] of [...perModel.entries()].sort()) {
  const diff = e.charged > 0 ? ((e.estimate - e.charged) / e.charged) * 100 : 0;
  console.log(
    `${model.padEnd(28)} charged $${e.charged.toFixed(4)}  list price $${e.estimate.toFixed(4)}  diff ${diff >= 0 ? '+' : ''}${diff.toFixed(1)}%`
  );
}
