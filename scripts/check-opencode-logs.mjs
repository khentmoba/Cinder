// Checks Cinder's OpenCode numbers against the raw database, with an
// implementation that shares no code with the collector.
//
//   node scripts/check-opencode-logs.mjs [YYYY-MM-DD]
//
// It reads ~/.local/share/opencode/opencode.db the way OpenCode writes it
// and prints one "ROW" line per model:
//
//   ROW <model> <requests> <input> <cache read> <cache write> <output>
//
// The same lines come out of the collector with:
//
//   cargo test --bin cinder -- --ignored --nocapture real_home_day_check
//
// The two sets have to be identical for the numbers to be right.

import { DatabaseSync } from 'node:sqlite';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

const day = process.argv[2] || new Date(Date.now() - 2 * 86400000).toISOString().slice(0, 10);
const path = join(homedir(), '.local', 'share', 'opencode', 'opencode.db');
if (!existsSync(path)) {
  console.log('OpenCode keeps no database on this machine');
  process.exit(0);
}

const db = new DatabaseSync(path, { readOnly: true });

// The same day the collector uses: a unix second, whatever the column holds.
const dayOf = (ts) => {
  const secs = ts > 1000000000000 ? Math.floor(ts / 1000) : ts;
  return new Date(secs * 1000).toISOString().slice(0, 10);
};

const rows = db
  .prepare(
    `SELECT time_created AS ts, data FROM message
     WHERE json_valid(data) AND json_extract(data,'$.role') = 'assistant'`
  )
  .all();

const perModel = new Map();
let requests = 0;
for (const row of rows) {
  let v;
  try {
    v = JSON.parse(row.data);
  } catch {
    continue;
  }
  const d = dayOf(row.ts);
  if (d !== day) continue;
  const provider = typeof v.providerID === 'string' && v.providerID ? v.providerID : null;
  const id = typeof v.modelID === 'string' && v.modelID ? v.modelID : 'unknown';
  const model = provider ? `${provider}/${id}` : id;
  const t = v.tokens || {};
  const cache = t.cache || {};
  const e = perModel.get(model) || [0, 0, 0, 0, 0];
  e[0]++;
  e[1] += t.input || 0;
  e[2] += cache.read || 0;
  e[3] += cache.write || 0;
  e[4] += t.output || 0;
  perModel.set(model, e);
  requests++;
}

console.log(`DAY\t${day}\tOpenCode`);
console.log(`read ${rows.length} messages, ${requests} requests`);
for (const model of [...perModel.keys()].sort()) {
  const [r, input, cacheRead, cacheWrite, output] = perModel.get(model);
  console.log(`ROW\t${model}\t${r}\t${input}\t${cacheRead}\t${cacheWrite}\t${output}`);
}
