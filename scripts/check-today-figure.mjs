// What the popover's "today" line shows, read out of the app's own cache.
//
//   node scripts/check-today-figure.mjs
//
// The tray popover answers from what the last scan found, so the figure it
// shows for today is the sum over the rows of today's day. This reads those
// rows out of the scan cache and adds them up, so the number on the popover
// can be checked against the day's rows.
//
// The day is the reader's own, not the UTC one: the agents write their
// timestamps in UTC, but the day on the screen is the one the reader is
// living in.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { localDay } from './local-day.mjs';

const cache = JSON.parse(
  readFileSync(join(homedir(), '.cinder', 'scan_cache.json'), 'utf8').replace(/^\uFEFF/, '')
);

const rows = [];
for (const file of Object.values(cache.files || {})) for (const r of file.rows || []) rows.push(r);
if (cache.opencode_db) for (const r of cache.opencode_db.rows || []) rows.push(r);

const today = localDay(Date.now());
const utc = new Date().toISOString().slice(0, 10);

for (const day of [...new Set([today, utc])]) {
  const on = rows.filter((r) => r.day === day);
  const cost = on.reduce((a, r) => a + (r.cost || 0), 0);
  const tokens = on.reduce((a, r) => a + (r.input || 0) + (r.cache_read || 0) + (r.cache_write || 0) + (r.output || 0), 0);
  const requests = on.reduce((a, r) => a + (r.requests || 0), 0);
  const label = day === today ? 'the reader\'s own day - what the popover shows' : 'the UTC day';
  console.log(`${day}  ${requests} requests  ${tokens} tokens  $${cost.toFixed(2)}   (${label})`);
}
