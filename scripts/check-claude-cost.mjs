// What the Claude Code CLI itself said each call cost.
//
//   node scripts/check-claude-cost.mjs [YYYY-MM-DD ...]
//
// The CLI writes a `costUSD` on every assistant message: its own figure for
// what the call cost, worked out from the price the provider charged. The
// collector prices the same tokens out of its own catalogue, so the two can be
// compared - the same way the token counts are compared.
//
// A run on a subscription writes the field but leaves it empty: there is no
// per-call cost to report. The script says so instead of pretending it checked
// something. The collector falls back to the list price in that case, and
// marks the row as estimated.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

const days = process.argv.slice(2);
if (!days.length) {
  const d = new Date(Date.now() - 2 * 86400000).toISOString().slice(0, 10);
  days.push(d);
}

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

const files = walk(join(homedir(), '.claude', 'projects'));
const perModel = new Map();
let written = 0;
let numbered = 0;

for (const file of files) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || !line.includes('"costUSD"')) continue;
    let v;
    try {
      v = JSON.parse(line);
    } catch {
      continue;
    }
    if (v.type !== 'assistant') continue;
    written++;
    const cost = typeof v.costUSD === 'number' ? v.costUSD : null;
    if (cost === null) continue;
    numbered++;
    const d = String(v.timestamp || '').slice(0, 10);
    if (!days.includes(d)) continue;
    const model = (v.message && v.message.model) || 'unknown';
    const e = perModel.get(model) || { n: 0, cost: 0 };
    e.n++;
    e.cost += cost;
    perModel.set(model, e);
  }
}

for (const day of days) {
  console.log(`DAY\t${day}\tClaude Code`);
  for (const model of [...perModel.keys()].sort()) {
    const e = perModel.get(model);
    console.log(`COST\t${model}\t${e.cost.toFixed(6)}\t${e.n} calls`);
  }
}

console.log(`SUMMARY\tcost figures written: ${written}, of which a number: ${numbered}`);
if (numbered === 0) {
  console.log('The CLI wrote no usable figure: this run is on a subscription, so the collector prices it from the list.');
}
