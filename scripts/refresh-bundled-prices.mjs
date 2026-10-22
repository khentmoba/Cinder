// Refresh the bundled price catalogue.
//
//   node scripts/refresh-bundled-prices.mjs
//
// The catalogue in src-tauri/prices.json is the snapshot the app falls back
// to when the daily fetch fails. This merges the freshly fetched list over
// it - the same rule the collector uses at run time - so the fallback carries
// the current prices and the long-context premiums, and keeps the models the
// upstream list has since dropped.
//
// The fetched list is written by the app itself (pricing::refresh), so the
// two are the same shape.

import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

const bundledPath = join(process.cwd(), 'src-tauri', 'prices.json');
const fetchedPath = join(homedir(), '.cinder', 'prices.json');

const read = (p) => JSON.parse(readFileSync(p, 'utf8').replace(/^\uFEFF/, ''));
const bundled = read(bundledPath);
const fetched = read(fetchedPath);

// The prices are quoted per million tokens; six decimals is a millionth of a
// cent per million tokens, and it keeps the file readable.
const round = (n) => (typeof n === 'number' ? Math.round(n * 1e6) / 1e6 : n);
const clean = (p) => {
  const out = { cr: round(p.cr ?? 0), cw: round(p.cw ?? 0), i: round(p.i ?? 0), o: round(p.o ?? 0), p: p.p ?? '' };
  if (p.tier) {
    out.tier = {
      above: p.tier.above,
      cr: round(p.tier.cr ?? 0),
      cw: round(p.tier.cw ?? 0),
      cw1h: round(p.tier.cw1h ?? 0),
      i: round(p.tier.i ?? 0),
      o: round(p.tier.o ?? 0),
    };
  }
  return out;
};

const models = {};
for (const [id, p] of Object.entries(bundled.models)) models[id] = clean(p);
let added = 0;
let updated = 0;
let premiums = 0;
for (const [id, p] of Object.entries(fetched.models)) {
  const before = models[id];
  if (!before) added++;
  else if (JSON.stringify(before) !== JSON.stringify(clean(p))) updated++;
  models[id] = clean(p);
  if (p.tier) premiums++;
}

const sorted = {};
for (const id of Object.keys(models).sort()) sorted[id] = models[id];

const out = {
  meta: { ...fetched.meta, models: Object.keys(sorted).length },
  models: sorted,
};
writeFileSync(bundledPath, JSON.stringify(out));
console.log(`bundled catalogue: ${Object.keys(sorted).length} models`);
console.log(`  added by the fetched list: ${added}`);
console.log(`  whose price moved: ${updated}`);
console.log(`  carrying a long-context premium: ${premiums}`);
