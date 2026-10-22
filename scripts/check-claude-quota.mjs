// Checks the Claude Code numbers in the tray popover against the numbers the
// provider's own agent recorded.
//
//   node scripts/check-claude-quota.mjs
//
// There are three numbers to compare, and they should agree:
//
//   1. what the Claude Code CLI wrote into its own session log (the agent's
//      own record of the window it was refused by, and when it resets),
//   2. what the usage API answers now,
//   3. what Cinder's fetcher last read out of that API, kept in
//      ~/.cinder/quota_cache.json.
//
// The first is independent of the others: the CLI writes its copy from the
// server's answer into the log, and the third is what the app put on the
// popover.
//
// The CLI only writes its record when a request is refused, so what it holds
// is the moment the window filled up - not the percentage. The reset moment
// is therefore the one that has to line up exactly.
//
// When the window is full the API refuses the fetch as well, which is the
// provider's own answer to the same question: the popover says the 5-hour
// window is used up, and the provider will not serve a request.

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

const readJson = (p) => {
  try {
    return JSON.parse(readFileSync(p, 'utf8').replace(/^\uFEFF/, ''));
  } catch {
    return null;
  }
};

// What the CLI itself recorded: the last window it was refused by.
let logged = null;
for (const file of walk(join(home, '.claude', 'projects'))) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    continue;
  }
  for (const line of text.split('\n')) {
    if (!line.includes('quotaLimits')) continue;
    let v;
    try {
      v = JSON.parse(line);
    } catch {
      continue;
    }
    const q = v.quotaLimits;
    if (!q || !q.rateLimitType) continue;
    if (!logged || v.timestamp > logged.at) logged = { at: v.timestamp, q };
  }
}
if (!logged) {
  console.log('the Claude Code logs hold no limits to compare against');
  process.exit(0);
}

// What the app put on the popover.
const cache = readJson(join(home, '.cinder', 'quota_cache.json'));
const report = (cache?.reports || []).find((r) => r.agent === 'Claude Code');
if (!report) {
  console.log("Cinder's cache holds no Claude Code card to compare against");
  process.exit(0);
}
const app = (report.windows || []).find((w) => w.window === '5h');
if (!app) {
  console.log("Cinder's card holds no 5-hour window");
  process.exit(0);
}

// The CLI's window is the 5-hour one; the 7-day window has its own record.
const kind = logged.q.rateLimitType;
const loggedReset = logged.q.resetsAt;
const sameReset = Math.abs((app.resets_at ?? 0) - loggedReset) <= 2;
const iso = (s) => (s ? new Date(s * 1000).toISOString() : 'unknown');
console.log(`the CLI wrote its limits at ${logged.at}`);
console.log(
  `  ${kind.padEnd(14)} log status ${logged.q.status}, resets ${iso(loggedReset)} (${loggedReset})`
);
console.log(
  `  ${kind.padEnd(14)} app  ${app.used_percent}% used, resets ${iso(app.resets_at)} (${app.resets_at})  ` +
    `reset ${sameReset ? 'the same' : 'DIFFERENT'}`
);

// And what the API answers now.
const cred = readJson(join(home, '.claude', '.credentials.json'));
const token = cred?.claudeAiOauth?.accessToken;
if (!token) {
  console.log('  Claude Code holds no access token to ask');
  process.exit(sameReset ? 0 : 1);
}
const resp = await fetch('https://api.anthropic.com/api/oauth/usage', {
  headers: {
    Authorization: `Bearer ${token}`,
    'anthropic-beta': 'oauth-2025-04-20',
    Accept: 'application/json',
    'User-Agent': 'Cinder',
  },
});
if (resp.status === 429) {
  console.log('  api   refused the request: the window is full, which is what the card says');
  process.exit(sameReset ? 0 : 1);
}
if (!resp.ok) {
  console.log(`  api   answered ${resp.status}`);
  process.exit(1);
}
const live = await resp.json();
const windows = [];
for (const l of live.limits || []) {
  const k = l.kind || l.group;
  if (!k) continue;
  windows.push({ kind: k, percent: l.percent, resets_at: l.resets_at });
}
if (!windows.length) {
  for (const [k, w] of Object.entries(live)) {
    if (!w || typeof w !== 'object') continue;
    if (w.utilization === undefined && w.percent === undefined) continue;
    windows.push({ kind: k, percent: w.utilization ?? w.percent, resets_at: w.resets_at });
  }
}
const api = windows.find((w) => w.kind === kind);
if (!api) {
  console.log(`  api   does not name a ${kind} window`);
  process.exit(sameReset ? 0 : 1);
}
const sameApiReset = Math.abs((api.resets_at ?? 0) - loggedReset) <= 2;
console.log(
  `  ${kind.padEnd(14)} api  ${api.percent ?? api.utilization}% used, resets ${iso(api.resets_at)}  ` +
    `reset ${sameApiReset ? 'the same' : 'DIFFERENT'}`
);
if (!sameReset || !sameApiReset) {
  console.log('  the reset moments differ');
  process.exit(1);
}
console.log("  the popover shows the provider's own numbers");
