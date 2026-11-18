// UI-4: the builder's change to the YAML, as one replacement for Monaco.
//
// Review round 1: the common start and end are found per UTF-16 unit, which
// can stop inside a surrogate pair (😀 → 😃 share the high half); Monaco
// snaps such a position and the replacement left a lone surrogate. The span
// is widened to whole user-perceived characters (grapheme clusters: a flag,
// a ZWJ emoji, a letter with its combining marks), in both texts.

const segmenter =
  typeof Intl !== 'undefined' && 'Segmenter' in Intl
    ? new Intl.Segmenter(undefined, { granularity: 'grapheme' })
    : null;

function isHigh(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

function isLow(code: number): boolean {
  return code >= 0xdc00 && code <= 0xdfff;
}

/**
 * The last character boundary at or before `pos`. Graphemes are found in
 * the line holding `pos` only (a line break is always a boundary, except
 * inside CR LF, which a line start never splits), so a long file costs
 * nothing extra.
 */
function boundaryAtOrBefore(text: string, pos: number): number {
  if (pos <= 0 || pos >= text.length) return pos;
  let at = pos;
  // A surrogate pair is never split, Segmenter or not.
  if (isLow(text.charCodeAt(at)) && isHigh(text.charCodeAt(at - 1))) at -= 1;
  if (!segmenter) return at;
  const lineStart = text.lastIndexOf('\n', at - 1) + 1;
  const nl = text.indexOf('\n', at);
  const lineEnd = nl === -1 ? text.length : nl + 1;
  let best = lineStart;
  for (const { index } of segmenter.segment(text.slice(lineStart, lineEnd))) {
    if (lineStart + index > at) break;
    best = lineStart + index;
  }
  return best;
}

/** The one replacement that turns `before` into `after` (common ends kept, whole characters). */
export function minimalChange(
  before: string,
  after: string,
): { start: number; end: number; text: string } | null {
  if (before === after) return null;
  const max = Math.min(before.length, after.length);
  let start = 0;
  while (start < max && before[start] === after[start]) start += 1;
  // Back to a boundary in both texts (the prefix is common, the next character not).
  for (;;) {
    const next = Math.min(boundaryAtOrBefore(before, start), boundaryAtOrBefore(after, start));
    if (next === start) break;
    start = next;
  }
  let end = 0;
  while (end < max - start && before[before.length - 1 - end] === after[after.length - 1 - end])
    end += 1;
  // The common suffix must start on a boundary in both texts: shrink it until it does.
  while (end > 0) {
    const b = before.length - end;
    const a = after.length - end;
    if (boundaryAtOrBefore(before, b) === b && boundaryAtOrBefore(after, a) === a) break;
    end -= 1;
  }
  return { start, end: before.length - end, text: after.slice(start, after.length - end) };
}
