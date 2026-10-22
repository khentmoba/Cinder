// Checks Cinder's numbers against the raw logs, with an implementation that
// shares no code with the collector.
//
//   node scripts/check-claude-logs.mjs [YYYY-MM-DD]
//
// It reads ~/.claude/projects the way Claude Code writes it and prints one
// "ROW" line per model:
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
const root = join(homedir(), '.claude', 'projects');

// Every .jsonl file under the projects folder.
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
let assistantMessages = 0;
let repeats = 0;

// The CLI writes one record per content block of a turn, and every record
// carries the whole turn's usage, so a turn shows up once per tool call it
// made. The API message id is the same in all of them, so the usage is
// counted once per id. The CLI's own session total (the cost-state line it
// writes) agrees with that count and not with the raw line count.
const seen = new Set();

for (const file of files) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line) continue;
    lines++;
    if (!line.includes('"assistant"')) continue;
    let v;
    try {
      v = JSON.parse(line);
    } catch {
      continue;
    }
    if (v.type !== 'assistant') continue;
    const usage = v.message && v.message.usage;
    if (!usage) continue;
    const input = usage.input_tokens || 0;
    const output = usage.output_tokens || 0;
    const cacheRead = usage.cache_read_input_tokens || 0;
    const cacheWrite = usage.cache_creation_input_tokens || 0;
    // A message that used nothing is not a request.
    if (input + output + cacheRead + cacheWrite === 0) continue;
    const ts = typeof v.timestamp === 'string' ? v.timestamp : '';
    if (localDay(ts) !== day) continue;
    const model = String((v.message && v.message.model) || 'unknown').trim();
    const id = v.message && v.message.id;
    if (id) {
      if (seen.has(id)) {
        repeats++;
        continue;
      }
      seen.add(id);
    }
    const e = perModel.get(model) || [0, 0, 0, 0, 0];
    e[0]++;
    e[1] += input;
    e[2] += cacheRead;
    e[3] += cacheWrite;
    e[4] += output;
    perModel.set(model, e);
    assistantMessages++;
  }
}

console.log(`DAY\t${day}\tClaude Code`);
console.log(`read ${files.length} files, ${lines} lines`);
console.log(`counted ${assistantMessages} turns, skipped ${repeats} repeats of a turn already counted`);
for (const model of [...perModel.keys()].sort()) {
  const [requests, input, cacheRead, cacheWrite, output] = perModel.get(model);
  console.log(`ROW\t${model}\t${requests}\t${input}\t${cacheRead}\t${cacheWrite}\t${output}`);
}
