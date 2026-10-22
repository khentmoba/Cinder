// The totals the Claude Code CLI itself wrote down, per day and model.
//
//   node scripts/check-claude-own-totals.mjs [YYYY-MM-DD]
//
// The CLI writes a cost-state line per session: what it charged and the
// per-model usage that made up the charge. Taking the last line of each
// session (they are snapshots of a running total) gives the CLI's own
// record of every turn it made.
//
// The collector's rows for the same day have to match:
//
//   cargo test --bin cinder -- --ignored --nocapture real_home_day_check

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { localDay } from './local-day.mjs';

const day = process.argv[2];
const root = join(homedir(), '.claude', 'projects');

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

// The last snapshot of each session: the cost-state lines are a running
// total, written over and over.
const sessions = new Map();
for (const file of walk(root)) {
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
    if (!v.sessionId) continue;
    const prev = sessions.get(v.sessionId);
    if (!prev || (v.totalDuration || 0) >= (prev.totalDuration || 0)) sessions.set(v.sessionId, v);
  }
}

const perModel = new Map();
let charged = 0;
for (const v of sessions.values()) {
  const d = localDay(v.startTime || 0);
  if (day && d !== day) continue;
  for (const [model, u] of Object.entries(v.modelUsage || {})) {
    const e = perModel.get(model) || { calls: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
    e.calls++;
    e.input += u.inputTokens || 0;
    e.output += u.outputTokens || 0;
    e.cacheRead += u.cacheReadInputTokens || 0;
    e.cacheWrite += u.cacheCreationInputTokens || 0;
    perModel.set(model, e);
    charged += u.costUSD || 0;
  }
}

console.log(`DAY\t${day || 'all'}\tClaude Code`);
console.log(`sessions: ${sessions.size}, charged by the CLI: $${charged.toFixed(4)}`);
for (const [model, e] of [...perModel.entries()].sort()) {
  console.log(`ROW\t${model}\t${e.calls}\t${e.input}\t${e.cacheRead}\t${e.cacheWrite}\t${e.output}`);
}
