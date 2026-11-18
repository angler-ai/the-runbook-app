// UI-3 review: a page printed from the dark theme prints on the light
// tokens (white paper, dark text), so every dark value applies on screen
// only. Read from the sources (vitest serves CSS imports empty).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const read = (name: string) =>
  fs.readFileSync(path.resolve(here, name), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

test('both dark token blocks apply on screen only', () => {
  const css = read('tokens.css');
  assert.doesNotMatch(css, /@media\s*\(\s*prefers-color-scheme:\s*dark\s*\)/);
  assert.match(css, /@media screen and \(prefers-color-scheme: dark\)\s*\{\s*:root:not/);
  assert.match(css, /@media screen\s*\{\s*:root\[data-theme="dark"\],/);
});

test('print sets form controls and scrollbars light', () => {
  assert.match(read('index.css'), /@media print\s*\{[\s\S]*color-scheme:\s*light/);
});
