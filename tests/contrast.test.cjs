const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const css = fs.readFileSync(path.join(__dirname, '../src/styles.css'), 'utf8');

// Reads one custom property out of the :root block.
function token(name) {
  const block = css.slice(css.indexOf(':root'), css.indexOf('}', css.indexOf(':root')));
  const match = block.match(new RegExp(`--${name}:\\s*(#[0-9a-fA-F]{6})`));
  assert.ok(match, `--${name} missing from :root`);
  return match[1];
}

function luminance(hex) {
  const channels = hex.match(/\w\w/g).map(x => parseInt(x, 16) / 255)
    .map(v => (v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)));
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}

function contrast(a, b) {
  const [l1, l2] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (l1 + 0.05) / (l2 + 0.05);
}

// Every colour that carries text must clear WCAG AA on both surfaces it can
// land on. The panel greys are the easy ones to "tidy" back down to invisible.
test('text tokens clear 4.5:1 on the background and on panels', () => {
  for (const surface of ['bg', 'panel', 'panel-2']) {
    for (const ink of ['text', 'dim', 'faint']) {
      const ratio = contrast(token(ink), token(surface));
      assert.ok(ratio >= 4.5,
        `--${ink} on --${surface} is ${ratio.toFixed(2)}:1, needs 4.5:1`);
    }
  }
});

test('the accent keeps enough contrast for links and focus rings', () => {
  assert.ok(contrast(token('accent'), token('bg')) >= 4.5);
});
