// @vitest-environment jsdom
// UI-4 review round 1: the builder's edit reaches Monaco as one replacement
// over the span that differs. That span must never cut a character in two
// (a surrogate pair) or a user-perceived character (a flag, a ZWJ emoji, a
// letter with combining marks): Monaco snaps such positions, and the
// replacement then left a lone surrogate behind.
import { beforeAll, describe, expect, it } from 'vitest';
import type * as MonacoModule from 'monaco-editor';
import { minimalChange } from './textChange';

const CASES: [string, string, string][] = [
  ['emoji', 'title: "😀 Deploy"\nnext: 1\n', 'title: "😃 Deploy"\nnext: 1\n'],
  ['emoji, same low half', 'a: 😀\n', 'a: 𐘀\n'],
  ['emoji added', 'a: x\n', 'a: x😀\n'],
  ['emoji removed', 'a: 😀😀\n', 'a: 😀\n'],
  ['flag', 'region: 🇺🇸\n', 'region: 🇺🇦\n'],
  ['flag to flag sharing a half', 'r: 🇫🇷🇩🇪\n', 'r: 🇫🇮🇩🇪\n'],
  ['ZWJ family', 'who: 👨‍👩‍👧\n', 'who: 👨‍👩‍👦\n'],
  ['ZWJ joined', 'who: 👨👩\n', 'who: 👨‍👩\n'],
  ['skin tone', 'hand: 👋🏽\n', 'hand: 👋🏿\n'],
  ['combining mark', 'name: René\n', 'name: Renè\n'],
  ['combining mark added', 'name: Rene\n', 'name: René\n'],
  ['two combining marks', 'x: ạ́b\n', 'x: á̤b\n'],
  ['CRLF', 'a: 1\r\nb: 2\r\n', 'a: 1\r\nc: 3\r\nb: 2\r\n'],
];

function isLone(text: string, i: number): boolean {
  const c = text.charCodeAt(i);
  if (c >= 0xd800 && c <= 0xdbff) {
    const n = text.charCodeAt(i + 1);
    return !(n >= 0xdc00 && n <= 0xdfff);
  }
  if (c >= 0xdc00 && c <= 0xdfff) {
    const p = text.charCodeAt(i - 1);
    return !(p >= 0xd800 && p <= 0xdbff);
  }
  return false;
}

function boundaries(text: string): Set<number> {
  const out = new Set<number>([text.length]);
  for (const s of new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(text))
    out.add(s.index);
  return out;
}

// Monaco takes seconds to load in jsdom: once, outside any test's time limit.
let monaco: typeof MonacoModule;
beforeAll(async () => {
  monaco = await import('monaco-editor');
}, 60_000);

describe('minimalChange', () => {
  it.each(CASES)('%s: the span starts and ends on character boundaries', (_, before, after) => {
    const change = minimalChange(before, after);
    expect(change).not.toBeNull();
    if (!change) return;
    expect(before.slice(0, change.start) + change.text + before.slice(change.end)).toBe(after);
    const b = boundaries(before);
    const a = boundaries(after);
    const afterEnd = after.length - (before.length - change.end);
    expect(b.has(change.start) && a.has(change.start)).toBe(true);
    expect(b.has(change.end) && a.has(afterEnd)).toBe(true);
    for (let i = 0; i < change.text.length; i += 1) expect(isLone(change.text, i)).toBe(false);
  });

  it('is null for equal text', () => {
    expect(minimalChange('a', 'a')).toBeNull();
  });

  it.each(CASES)(
    '%s: a real Monaco model ends up with exactly the new text',
    async (_, before, after) => {
      const model = monaco.editor.createModel(before, 'plaintext');
      try {
        const change = minimalChange(model.getValue(), after);
        if (!change) throw new Error('no change');
        const from = model.getPositionAt(change.start);
        const to = model.getPositionAt(change.end);
        model.pushEditOperations(
          [],
          [
            {
              range: new monaco.Range(from.lineNumber, from.column, to.lineNumber, to.column),
              text: change.text,
            },
          ],
          () => null,
        );
        expect(model.getValue()).toBe(after);
      } finally {
        model.dispose();
      }
    },
  );
});
