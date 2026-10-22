// The collector's count against the CLI's own count, session by session.
//
//   node scripts/check-claude-sessions.mjs
//
// The CLI writes a cost-state line per session: the usage it made and what it
// charged. That is the agent's own record of the same turns the collector
// reads from the transcript, so the two can be compared directly.
//
// The transcripts hold one record per content block of a turn, and every
// record carries the whole turn's usage. Counting the records as they come
// counts a turn once per tool call it made; counting one record per API
// message id counts each turn once. This says which one agrees with the CLI.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

const root = join(homedir(), '.claude', 'projects');

// --no-dedup counts every record as it comes, which is what the collector did
// before it learned that a turn is written down once per content block.
const dedup = !process.argv.includes('--no-dedup');

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

let sessions = 0;
let agree = 0;
let cliBehind = 0;
let cliAhead = 0;
let noTotal = 0;
const ratios = [];
const differences = [];

for (const file of walk(root)) {
  const id = file.split(/[\\/]/).pop().replace(/\.jsonl$/, '');
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  const perModel = new Map();
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
      if (mid && dedup) {
        if (seen.has(mid)) continue;
        seen.add(mid);
      }
      const u = v.message.usage;
      const model = (v.message.model || 'unknown').replace(/\[[^\]]*\]/g, '');
      const e = perModel.get(model) || { in: 0, out: 0, cr: 0, cw: 0 };
      e.in += u.input_tokens || 0;
      e.out += u.output_tokens || 0;
      e.cr += u.cache_read_input_tokens || 0;
      e.cw += u.cache_creation_input_tokens || 0;
      perModel.set(model, e);
    } else if (v.type === 'cost-state' && v.sessionId === id) {
      if (!snapshot || (v.totalDuration || 0) >= (snapshot.totalDuration || 0)) snapshot = v;
    }
  }
  if (!snapshot || !snapshot.modelUsage) continue;
  let cliTokens = 0;
  let ours = 0;
  let same = true;
  // The CLI can list the same model under two names ("claude-opus-5-5" and
  // "claude-opus-5-5[1m]"), so its usage is gathered per model first.
  const cli = new Map();
  for (const [model, u] of Object.entries(snapshot.modelUsage)) {
    const key = model.replace(/\[[^\]]*\]/g, '');
    const e = cli.get(key) || { in: 0, out: 0, cr: 0, cw: 0 };
    e.in += u.inputTokens || 0;
    e.out += u.outputTokens || 0;
    e.cr += u.cacheReadInputTokens || 0;
    e.cw += u.cacheCreationInputTokens || 0;
    cli.set(key, e);
  }
  for (const [model, u] of cli) {
    cliTokens += u.in + u.out + u.cr + u.cw;
    const e = perModel.get(model) || { in: 0, out: 0, cr: 0, cw: 0 };
    const mine = e.in + e.out + e.cr + e.cw;
    ours += mine;
    if (mine !== u.in + u.out + u.cr + u.cw) {
      same = false;
      // Which kind of token differs? The CLI counts a session's own overhead
      // (the system prompt, the tool definitions) in its input total, which
      // the transcript does not repeat per message.
      const kinds = [];
      if (e.in !== u.in) kinds.push(`input ${e.in} vs ${u.in}`);
      if (e.out !== u.out) kinds.push(`output ${e.out} vs ${u.out}`);
      if (e.cr !== u.cr) kinds.push(`cache read ${e.cr} vs ${u.cr}`);
      if (e.cw !== u.cw) kinds.push(`cache write ${e.cw} vs ${u.cw}`);
      if (!kinds.length) kinds.push('same');
      if (differences.length < 6) differences.push(`${model}: ${kinds.join(', ')}`);
    }
  }
  sessions++;
  if (!cliTokens) {
    noTotal++;
    continue;
  }
  ratios.push(ours / cliTokens);
  if (same) agree++;
  else if (ours > cliTokens) cliAhead++;
  else cliBehind++;
}

ratios.sort((a, b) => a - b);
const median = ratios.length ? ratios[Math.floor(ratios.length / 2)] : 0;
console.log(`sessions with a cost-state: ${sessions}`);
console.log(`  the transcript and the CLI's total agree exactly: ${agree}`);
console.log(`  the transcript has more (the CLI's snapshot lags): ${cliAhead}`);
console.log(`  the CLI has more: ${cliBehind}`);
console.log(`  the CLI recorded no usage: ${noTotal}`);
console.log(`  median ratio transcript/CLI: ${median.toFixed(3)}`);
for (const d of differences) console.log('  ' + d);