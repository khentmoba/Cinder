// Checks Cinder's T3 (Antigravity) token numbers against the raw database,
// with an implementation that shares no code with the collector.
//
//   node scripts/check-t3-logs.mjs [YYYY-MM-DD]
//
// T3 keeps the exact per-turn token usage in its own orchestration database,
// under ~/.t3/userdata/statev2.sqlite. It prints one "TOKENS" line per
// model:
//
//   TOKENS <model> <input> <cache read> <output>
//
// The same lines come out of the collector with:
//
//   cargo test --bin cinder -- --ignored --nocapture real_home_day_check
//
// The requests are left out on purpose: they depend on the transcripts as
// well, and the check is about the tokens, which are the biggest number the
// dashboard shows.

import { DatabaseSync } from 'node:sqlite';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { localDay } from './local-day.mjs';

const day = process.argv[2] || localDay(Date.now() - 2 * 86400000);
const path = join(homedir(), '.t3', 'userdata', 'statev2.sqlite');
if (!existsSync(path)) {
  console.log('T3 keeps no usage database on this machine');
  process.exit(0);
}

function walk(dir, out = [], only = null) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const e of entries) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, out, only);
    else if (!only || e.name === only) out.push(p);
  }
  return out;
}

const db = new DatabaseSync(path, { readOnly: true });

// Both tables have a payload_json, so they are told apart by their aliases.
const turns = db
  .prepare(
    `SELECT t.provider_thread_id AS thread, t.started_at AS started, t.payload_json AS turn_payload,
            r.payload_json AS run_payload
     FROM orchestration_v2_projection_provider_turns t
     JOIN orchestration_v2_projection_run_attempts a ON a.attempt_id = t.run_attempt_id
     JOIN orchestration_v2_projection_runs r ON r.run_id = a.run_id`
  )
  .all();

const perModel = new Map();
let counted = 0;
for (const turn of turns) {
  let run;
  try {
    run = JSON.parse(turn.run_payload);
  } catch {
    continue;
  }
  const raw = run.modelSelection && run.modelSelection.model;
  if (!raw || !String(raw).toLowerCase().startsWith('antigravity/')) continue;
  let payload;
  try {
    payload = JSON.parse(turn.turn_payload);
  } catch {
    continue;
  }
  const usage = payload.tokenUsage;
  if (!usage) continue;
  const input = usage.inputTokens || 0;
  const cacheRead = usage.cachedInputTokens || 0;
  const output = usage.outputTokens || 0;
  if (input + cacheRead + output === 0) continue;
  // The day of the turn, or of the usage itself when the turn does not say.
  const d = localDay(turn.started || '') || localDay(usage.updatedAt || '');
  if (d !== day) continue;
  const e = perModel.get(raw) || [0, 0, 0];
  e[0] += input;
  e[1] += cacheRead;
  e[2] += output;
  perModel.set(raw, e);
  counted++;
}

console.log(`DAY\t${day}\tT3`);
console.log(`read ${turns.length} turns, ${counted} with usage`);
for (const model of [...perModel.keys()].sort()) {
  const [input, cacheRead, output] = perModel.get(model);
  console.log(`TOKENS\t${model}\t${input}\t${cacheRead}\t${output}`);
}

// The IDE transcripts carry the same usage in the other place Antigravity
// keeps it: ~/.gemini/antigravity*/brain/**/transcript.jsonl.
const ide = [];
for (const variant of ['antigravity', 'antigravity-cli']) {
  walk(join(homedir(), '.gemini', variant, 'brain'), ide, 'transcript.jsonl');
}

const perModelIde = new Map();
for (const file of ide) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  let model = 'unknown';
  const seenSteps = new Set();
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line) continue;
    let v;
    try {
      v = JSON.parse(line);
    } catch {
      continue;
    }
    // The model is named in a line of prose.
    if (typeof v.content === 'string') {
      const idx = v.content.lastIndexOf('Model Selection` from');
      if (idx >= 0) {
        const rest = v.content.slice(idx);
        const to = rest.indexOf(' to ');
        if (to >= 0) {
          const tail = rest.slice(to + 4);
          let end = tail.indexOf('. ');
          if (end >= 0) end += 1;
          else if (tail.endsWith('.')) end = tail.length;
          else end = tail.indexOf('\n');
          if (end < 0) end = tail.length;
          const m = tail.slice(0, end).trim().replace(/\.+$/, '').trim();
          if (m && m !== 'None') model = m;
        }
      }
    }
    if (v.type !== 'PLANNER_RESPONSE') continue;
    if (v.step_index !== undefined) {
      if (seenSteps.has(v.step_index)) continue;
      seenSteps.add(v.step_index);
    }
    const input = v.input_tokens || 0;
    const cacheRead = v.cache_read_tokens || 0;
    const output = v.output_tokens || 0;
    if (input + cacheRead + output === 0) continue;
    const d = localDay(v.created_at || '');
    if (d !== day) continue;
    const e = perModelIde.get(model) || [0, 0, 0];
    e[0] += input;
    e[1] += cacheRead;
    e[2] += output;
    perModelIde.set(model, e);
  }
}
console.log(`read ${ide.length} IDE transcripts`);
for (const model of [...perModelIde.keys()].sort()) {
  const [input, cacheRead, output] = perModelIde.get(model);
  console.log(`TOKENS\t${model}\t${input}\t${cacheRead}\t${output}`);
}
