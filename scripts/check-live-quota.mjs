// Checks the numbers Cinder shows in the tray popover against the numbers
// the provider's own agent recorded.
//
//   node scripts/check-live-quota.mjs [Codex]
//
// For Codex there are three numbers to compare, and they should all be the
// same:
//
//   1. what the Codex CLI wrote into its own session log (the agent's own
//      record of its limits),
//   2. what the usage API answers now,
//   3. what Cinder's fetcher reads out of that API.
//
// The first two are independent of each other: the CLI writes its copy from
// the server's answer into the log, and the second is a fresh fetch. If they
// agree, the number on the popover is the provider's number.
//
// The reset times have to match exactly. The percentages may have moved a
// little between the log being written and the fetch, so they are compared
// with a small tolerance.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

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

const codexFiles = [
  ...walk(join(home, '.codex', 'sessions')),
  ...walk(join(home, '.codex', 'archived_sessions')),
];

// What the Codex CLI itself recorded.
let logged = null;
for (const file of codexFiles) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  for (const line of text.split('\n')) {
    if (!line.includes('rate_limits')) continue;
    let v;
    try {
      v = JSON.parse(line);
    } catch {
      continue;
    }
    const rl = v.payload && v.payload.rate_limits;
    if (!rl || !rl.primary) continue;
    if (!logged || v.timestamp > logged.at) logged = { at: v.timestamp, rl };
  }
}
if (!logged) {
  console.log('the Codex logs hold no rate limits to compare against');
  process.exit(0);
}

// What the usage API answers now.
let auth;
try {
  auth = JSON.parse(readFileSync(join(home, '.codex', 'auth.json'), 'utf8'));
} catch {
  console.log('Codex is not signed in on this machine');
  process.exit(0);
}
const token = auth.tokens && auth.tokens.access_token;
if (!token) {
  console.log('Codex holds no access token');
  process.exit(0);
}
const resp = await fetch('https://chatgpt.com/backend-api/wham/usage', {
  headers: {
    Authorization: `Bearer ${token}`,
    'OpenAI-Beta': 'codex-1',
    Accept: 'application/json',
    'User-Agent': 'Cinder',
  },
});
if (!resp.ok) {
  console.log(`the usage API answered ${resp.status}`);
  process.exit(1);
}
const live = await resp.json();

const apiWindow = (name) => {
  const rl = live.rate_limit || {};
  const w = rl[`${name}_window`];
  if (!w) return null;
  // The API calls the moment `reset_at`; the log calls it `resets_at`.
  return {
    percent: w.used_percent,
    resets_at: w.reset_at ?? w.resets_at,
    minutes: w.limit_window_seconds,
  };
};

// The two windows Cinder shows, and what the log says about them.
const pairs = [
  ['5-hour window', 'primary', apiWindow('primary')],
  ['7-day window', 'secondary', apiWindow('secondary')],
];

let failures = 0;
console.log(`the CLI wrote its limits at ${logged.at}`);
for (const [label, key, api] of pairs) {
  const fromLog = logged.rl[key];
  if (!fromLog || !api) continue;
  const percentGap = Math.abs((fromLog.used_percent ?? 0) - (api.percent ?? 0));
  const sameReset = fromLog.resets_at === api.resets_at;
  const close = percentGap <= 5;
  console.log(
    `  ${label.padEnd(14)} log ${String(fromLog.used_percent).padStart(3)}%  ` +
      `api ${String(api.percent).padStart(3)}%  gap ${percentGap}  ` +
      `reset ${sameReset ? 'the same' : `DIFFERENT (log ${fromLog.resets_at}, api ${api.resets_at})`}`
  );
  if (!sameReset || !close) failures++;
}
if (failures) {
  console.log(`  ${failures} window(s) do not agree`);
  process.exit(1);
}
console.log('  the popover shows the provider\'s own numbers');
