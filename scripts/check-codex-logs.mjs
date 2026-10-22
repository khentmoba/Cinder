// Checks Cinder's Codex numbers against the raw logs, with an implementation
// that shares no code with the collector.
//
//   node scripts/check-codex-logs.mjs [YYYY-MM-DD]
//
// It reads ~/.codex/sessions and ~/.codex/archived_sessions the way Codex
// writes them and prints one "ROW" line per model:
//
//   ROW <model> <requests> <input> <cache read> <cache write> <output>
//
// The same lines come out of the collector with:
//
//   cargo test --bin cinder -- --ignored --nocapture real_home_day_check
//
// The two sets have to be identical for the numbers to be right.

import { readdirSync, readFileSync } from 'node:fs';
import { localDay } from './local-day.mjs';
import { join } from 'node:path';
import { homedir } from 'node:os';

const day = process.argv[2] || localDay(Date.now() - 2 * 86400000);
const home = homedir();

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

const files = [
  ...walk(join(home, '.codex', 'sessions')),
  ...walk(join(home, '.codex', 'archived_sessions')),
];

const perModel = new Map();
let lines = 0;
let requests = 0;

for (const file of files) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  // The model and the day are properties of the file, not of the line.
  let model = 'unknown';
  let fileDay = '';
  // A forked rollout replays the parent's earlier turns, so the same usage can
  // show up twice in one file. Those pairs are the parent's, not the fork's.
  const seen = new Set();

  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line) continue;
    lines++;
    let v;
    try {
      v = JSON.parse(line);
    } catch {
      continue;
    }
    const ty = v.type;
    const p = v.payload || {};
    const ts = typeof v.timestamp === 'string' ? v.timestamp : '';
    if (ty === 'session_meta' && !fileDay) fileDay = localDay(ts);
    if (ty === 'turn_context' && typeof p.model === 'string') model = p.model.trim();
    if (ty !== 'event_msg' || p.type !== 'token_count') continue;
    const info = p.info;
    const last = info && info.last_token_usage;
    if (!last) continue;
    const input = last.input_tokens || 0;
    const cached = last.cached_input_tokens || 0;
    const output = last.output_tokens || 0;
    if (input + output === 0) continue;
    const key = `${ts}|${input}|${output}|${cached}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const d = localDay(ts) || fileDay;
    if (d !== day) continue;
    const uncached = Math.max(0, input - cached);
    const e = perModel.get(model) || [0, 0, 0, 0, 0];
    e[0]++;
    e[1] += uncached;
    e[2] += cached;
    e[3] += 0;
    e[4] += output;
    perModel.set(model, e);
    requests++;
  }
}

console.log(`DAY\t${day}\tCodex`);
console.log(`read ${files.length} files, ${lines} lines, ${requests} requests`);
for (const model of [...perModel.keys()].sort()) {
  const [r, input, cacheRead, cacheWrite, output] = perModel.get(model);
  console.log(`ROW\t${model}\t${r}\t${input}\t${cacheRead}\t${cacheWrite}\t${output}`);
}
