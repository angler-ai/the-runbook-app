// UI-4: a line-based three-way merge for "Reload and reapply". Both sides
// are diffed against the revision the author started from; changes in
// different places are combined, and any two changes that overlap or touch
// (as git's merge treats them) make the merge refuse rather than guess.
//
// Review round 1: a line diff places a change by position, and two diffs
// can place the same change at different lines (a duplicated step, a move
// past identical lines), so combining them could apply it twice. The merge
// is therefore strict, and refuses rather than guess:
// - any change both sides made to the same or adjacent lines, even an
//   identical one (a step one side deleted and the other moved deletes the
//   same lines on both sides);
// - any line, ignoring indentation, that both sides insert, or both delete,
//   anywhere (the same step added twice, re-indented into another list, or
//   one change aligned at two places).
// And a save whose answer never came may have landed, which no three-way
// merge can see (a later edit that undoes it looks like "I changed nothing
// there"): `reapply` takes the texts sent without an answer and refuses
// unless the newer draft is exactly one of them (then it is the base).
// A refused merge leaves the other two choices (reload theirs, keep mine);
// a wrong merge would go unnoticed.

interface Hunk {
  /** Replace base lines [start, end) with `lines`. */
  start: number;
  end: number;
  lines: string[];
}

/** Above this many cells the line diff is not attempted (the merge refuses). */
const MAX_CELLS = 4_000_000;

function diff(a: string[], b: string[]): Hunk[] | null {
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < a.length - prefix &&
    suffix < b.length - prefix &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  )
    suffix += 1;
  const x = a.slice(prefix, a.length - suffix);
  const y = b.slice(prefix, b.length - suffix);
  const n = x.length;
  const m = y.length;
  if ((n + 1) * (m + 1) > MAX_CELLS) return null;
  // LCS lengths from the end, so the walk below goes forwards.
  const width = m + 1;
  const lcs = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      lcs[i * width + j] =
        x[i] === y[j]
          ? (lcs[(i + 1) * width + j + 1] ?? 0) + 1
          : Math.max(lcs[(i + 1) * width + j] ?? 0, lcs[i * width + j + 1] ?? 0);
    }
  }
  const hunks: Hunk[] = [];
  let i = 0;
  let j = 0;
  let open: Hunk | null = null;
  const close = () => {
    if (open) hunks.push(open);
    open = null;
  };
  while (i < n || j < m) {
    if (i < n && j < m && x[i] === y[j]) {
      close();
      i += 1;
      j += 1;
    } else if (
      j < m &&
      (i === n || (lcs[i * width + j + 1] ?? 0) >= (lcs[(i + 1) * width + j] ?? 0))
    ) {
      open ??= { start: prefix + i, end: prefix + i, lines: [] };
      open.lines.push(y[j] as string);
      j += 1;
    } else {
      open ??= { start: prefix + i, end: prefix + i, lines: [] };
      open.end = prefix + i + 1;
      i += 1;
    }
  }
  close();
  return hunks;
}

export type MergeResult =
  | { ok: true; text: string }
  | {
      ok: false;
      /** `overlap`: both changed the same thing; `unconfirmed`: an earlier save may have landed. */
      reason: 'overlap' | 'unconfirmed';
    };

export function merge3(base: string, mine: string, theirs: string): MergeResult {
  if (mine === base) return { ok: true, text: theirs };
  if (theirs === base || theirs === mine) return { ok: true, text: mine };
  const baseLines = base.split('\n');
  const ours = diff(baseLines, mine.split('\n'));
  const other = diff(baseLines, theirs.split('\n'));
  if (!ours || !other) return { ok: false, reason: 'overlap' };
  const all = [
    ...ours.map((h) => ({ ...h, side: 0 })),
    ...other.map((h) => ({ ...h, side: 1 })),
  ].sort((a, b) => a.start - b.start || a.end - b.end || a.side - b.side);
  const groups: (Hunk & { side: number })[] = [];
  let k = 0;
  while (k < all.length) {
    const group = [all[k] as (typeof all)[number]];
    let end = group[0]?.end ?? 0;
    k += 1;
    // Changes that overlap or touch belong together.
    while (k < all.length && (all[k]?.start ?? 0) <= end) {
      const next = all[k] as (typeof all)[number];
      group.push(next);
      end = Math.max(end, next.end);
      k += 1;
    }
    if (new Set(group.map((h) => h.side)).size > 1) return { ok: false, reason: 'overlap' };
    groups.push(...group);
  }
  if (sharedContent(groups, baseLines)) return { ok: false, reason: 'overlap' };
  const out: string[] = [];
  let at = 0;
  for (const hunk of groups) {
    out.push(...baseLines.slice(at, hunk.start), ...hunk.lines);
    at = hunk.end;
  }
  out.push(...baseLines.slice(at));
  return { ok: true, text: out.join('\n') };
}

/** Whether both sides insert, or both delete, a line with the same text (indentation aside). */
function sharedContent(hunks: readonly (Hunk & { side: number })[], baseLines: string[]): boolean {
  const added = [new Set<string>(), new Set<string>()];
  const removed = [new Set<string>(), new Set<string>()];
  for (const hunk of hunks) {
    for (const line of hunk.lines) added[hunk.side]?.add(line.trim());
    for (let i = hunk.start; i < hunk.end; i += 1)
      removed[hunk.side]?.add((baseLines[i] as string).trim());
  }
  const meets = (a: Set<string> | undefined, b: Set<string> | undefined) =>
    [...(a ?? [])].some((line) => b?.has(line));
  return meets(added[0], added[1]) || meets(removed[0], removed[1]);
}

export interface ReapplyInput {
  /** The text of the revision the author's edits started from (the last one confirmed). */
  base: string;
  /** Texts sent in saves that got no answer: each may have become a revision. */
  unconfirmed: readonly string[];
  /** The author's text now. */
  mine: string;
  /** The newer draft on the server. */
  theirs: string;
}

/** "Reload and reapply": the author's changes on top of the newer draft, or a refusal. */
export function reapply({ base, unconfirmed, mine, theirs }: ReapplyInput): MergeResult {
  // The newer draft is a save of ours that got no answer: nothing of theirs to keep.
  if (unconfirmed.includes(theirs)) return merge3(theirs, mine, theirs);
  // One may have landed under a later revision: whose changes are whose is unknowable.
  if (unconfirmed.some((sent) => sent !== base)) return { ok: false, reason: 'unconfirmed' };
  return merge3(base, mine, theirs);
}
