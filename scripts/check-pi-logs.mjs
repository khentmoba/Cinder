// Checks Cinder's Pi numbers against the raw logs, with an implementation
// that shares no code with the collector.
//
//   node scripts/check-pi-logs.mjs [YYYY-MM-DD]
//
// It reads ~/.pi/agent/sessions the way Pi writes them and prints one "ROW"
// line per model:
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
const root = join(homedir(), '.pi', 'agent', 'sessions');

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

const files = walk(root);
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
  // The day of the session header is the fallback for a message that does
  // not carry a timestamp of its own.
  let fileDay = '';
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
    const ts = typeof v.timestamp === 'string' ? v.timestamp : '';
    if (v.type === 'session') {
      if (!fileDay) fileDay = localDay(ts);
      continue;
    }
    if (v.type !== 'message') continue;
    const msg = v.message;
    if (!msg || msg.role !== 'assistant') continue;
    const u = msg.usage || {};
    const input = u.input || 0;
    const output = u.output || 0;
    const cacheRead = u.cacheRead || 0;
    const cacheWrite = u.cacheWrite || 0;
    // A message that used nothing is not a request.
    if (input + output + cacheRead + cacheWrite === 0) continue;
    const d = localDay(ts) || fileDay;
    if (d !== day) continue;
    const model = String(msg.model || 'unknown').trim();
    const e = perModel.get(model) || [0, 0, 0, 0, 0];
    e[0]++;
    e[1] += input;
    e[2] += cacheRead;
    e[3] += cacheWrite;
    e[4] += output;
    perModel.set(model, e);
    requests++;
  }
}

console.log(`DAY\t${day}\tPi`);
console.log(`read ${files.length} files, ${lines} lines, ${requests} requests`);
for (const model of [...perModel.keys()].sort()) {
  const [r, input, cacheRead, cacheWrite, output] = perModel.get(model);
  console.log(`ROW\t${model}\t${r}\t${input}\t${cacheRead}\t${cacheWrite}\t${output}`);
}
