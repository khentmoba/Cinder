// Checks Cinder's DSH numbers against the raw logs, with an implementation
// that shares no code with the collector.
//
//   node scripts/check-dsh-logs.mjs [YYYY-MM-DD]
//
// It reads ~/.dsh/sessions the way DSH writes it — zstd-compressed JSONL —
// and prints one "ROW" line per model:
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
import { zstdDecompressSync } from 'node:zlib';

const day = process.argv[2] || localDay(Date.now() - 2 * 86400000);
const root = join(homedir(), '.dsh', 'sessions');

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
    else if (e.name.endsWith('.jsonl.zstd')) out.push(p);
  }
  return out;
}

// The day of a millisecond moment.
const dayOf = (ms) => (ms > 0 ? localDay(Math.floor(ms / 1000) * 1000) : '');

// DSH writes a session as a chain of zstd frames, one per chunk. Node's zstd
// reads the first frame and stops, so the frames are walked by hand: the next
// frame starts at the earliest place the magic appears that still decodes.
const MAGIC = [0x28, 0xb5, 0x2f, 0xfd];
const isMagic = (buf, at) =>
  at + 4 <= buf.length && MAGIC.every((b, i) => buf[at + i] === b);

function decodeFrames(buf) {
  const parts = [];
  let from = 0;
  let frames = 0;
  while (from + 4 <= buf.length) {
    let found = -1;
    let text = null;
    for (let at = from; at + 4 <= buf.length; at++) {
      if (!isMagic(buf, at)) continue;
      let out;
      try {
        out = zstdDecompressSync(buf.subarray(at));
      } catch {
        continue; // not a frame boundary after all
      }
      const s = out.toString('utf8');
      // A real frame holds JSON lines; a chance match in the compressed data
      // does not.
      if (!s.includes('"type"')) continue;
      found = at;
      text = s;
      break;
    }
    if (found < 0) break;
    parts.push(text);
    frames++;
    from = found + 1;
  }
  return { text: parts.join(''), frames };
}

const files = walk(root);
const perModel = new Map();
let requests = 0;
let lines = 0;
let frames = 0;

for (const file of files) {
  let buf;
  try {
    buf = readFileSync(file);
  } catch {
    continue;
  }
  const decoded = decodeFrames(buf);
  frames += decoded.frames;
  const text = decoded.text;
  // The model and the day of the session are properties of the file.
  let model = 'unknown';
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
    if (v.type === 'session') {
      if (!fileDay) fileDay = dayOf(v.createdAt || 0);
      continue;
    }
    if (v.type === 'model/selection') {
      const d = v.data || {};
      if (d.model) model = d.provider ? `${d.provider}/${d.model}` : d.model;
      continue;
    }
    if (v.type !== 'assistant/message') continue;
    const data = v.data || {};
    const src = (data.message || {}).source || {};
    if (src.model) model = src.provider ? `${src.provider}/${src.model}` : src.model;
    const u = data.usage || {};
    const input = u.inputTokens || 0;
    const output = u.outputTokens || 0;
    const cacheRead = u.cacheReadTokens || 0;
    const cacheWrite = u.cacheWriteTokens || 0;
    const d = dayOf(v.time || 0) || fileDay;
    if (d !== day) continue;
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

console.log(`DAY\t${day}\tDSH`);
console.log(`read ${files.length} files, ${lines} lines, ${requests} requests`);
for (const model of [...perModel.keys()].sort()) {
  const [r, input, cacheRead, cacheWrite, output] = perModel.get(model);
  console.log(`ROW\t${model}\t${r}\t${input}\t${cacheRead}\t${cacheWrite}\t${output}`);
}
