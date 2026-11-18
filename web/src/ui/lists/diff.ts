/**
 * Pure LCS (Longest Common Subsequence) Line Diff implementation.
 * Computes unified diffs without external dependencies.
 */

export interface DiffHunk {
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
  lines: string[];
}

export type DiffOp =
  | { type: 'equal'; line: string }
  | { type: 'delete'; line: string }
  | { type: 'insert'; line: string };

/**
 * Computes the longest common subsequence line operations between two texts.
 */
export function computeLineOperations(before: string, after: string): DiffOp[] {
  // If identical, return all equal
  if (before === after) {
    if (!before) return [];
    return before.split('\n').map((line) => ({ type: 'equal', line }));
  }

  const a = before.length > 0 ? before.split('\n') : [];
  const b = after.length > 0 ? after.split('\n') : [];

  // Trim the shared head and tail first: version diffs usually change a few
  // lines, so the LCS table only covers the changed middle.
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  ) {
    tail++;
  }
  const prefix: DiffOp[] = a.slice(0, head).map((line) => ({ type: 'equal', line }));
  const suffix: DiffOp[] = a.slice(a.length - tail).map((line) => ({ type: 'equal', line }));
  const midA = a.slice(head, a.length - tail);
  const midB = b.slice(head, b.length - tail);
  return [...prefix, ...lcsOperations(midA, midB), ...suffix];
}

/**
 * Largest LCS table (cells) computed in the browser. Above it the changed
 * middle is shown as one deleted block and one inserted block: still a
 * correct diff, never a frozen tab (sources can reach 1 MiB).
 */
export const MAX_LCS_CELLS = 4_000_000;

function lcsOperations(a: string[], b: string[]): DiffOp[] {
  const n = a.length;
  const m = b.length;
  if (n === 0) return b.map((line) => ({ type: 'insert', line }));
  if (m === 0) return a.map((line) => ({ type: 'delete', line }));
  if (n * m > MAX_LCS_CELLS) {
    return [
      ...a.map((line): DiffOp => ({ type: 'delete', line })),
      ...b.map((line): DiffOp => ({ type: 'insert', line })),
    ];
  }

  // Flat (n + 1) x (m + 1) table of LCS lengths.
  const width = m + 1;
  const dp = new Uint32Array((n + 1) * width);
  for (let i = 0; i < n; i++) {
    const aLine = a[i];
    for (let j = 0; j < m; j++) {
      dp[(i + 1) * width + j + 1] =
        aLine === b[j]
          ? dp[i * width + j]! + 1
          : Math.max(dp[i * width + j + 1]!, dp[(i + 1) * width + j]!);
    }
  }

  const ops: DiffOp[] = [];
  let i = n;
  let j = m;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && a[i - 1] === b[j - 1]) {
      ops.push({ type: 'equal', line: a[i - 1]! });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || dp[i * width + j - 1]! >= dp[(i - 1) * width + j]!)) {
      ops.push({ type: 'insert', line: b[j - 1]! });
      j--;
    } else {
      ops.push({ type: 'delete', line: a[i - 1]! });
      i--;
    }
  }
  ops.reverse();
  return ops;
}

export interface UnifiedDiffOptions {
  context?: number;
  oldHeader?: string;
  newHeader?: string;
}

/**
 * Generates a standard unified diff string from two texts.
 */
export function computeUnifiedDiff(
  before: string,
  after: string,
  options: UnifiedDiffOptions = {},
): string {
  if (before === after) {
    return '';
  }

  const context = options.context ?? 3;
  const ops = computeLineOperations(before, after);

  // Group diff operations into hunks with context
  const diffIndices: number[] = [];
  for (let idx = 0; idx < ops.length; idx++) {
    if (ops[idx]!.type !== 'equal') {
      diffIndices.push(idx);
    }
  }

  if (diffIndices.length === 0) {
    return '';
  }

  // Find hunk ranges [startOpIdx, endOpIdx]
  interface Range {
    start: number;
    end: number;
  }
  const ranges: Range[] = [];
  let currentRange: Range = {
    start: Math.max(0, diffIndices[0]! - context),
    end: Math.min(ops.length - 1, diffIndices[0]! + context),
  };

  for (let k = 1; k < diffIndices.length; k++) {
    const diffIdx = diffIndices[k]!;
    const newStart = Math.max(0, diffIdx - context);
    const newEnd = Math.min(ops.length - 1, diffIdx + context);

    if (newStart <= currentRange.end + 1) {
      currentRange.end = Math.max(currentRange.end, newEnd);
    } else {
      ranges.push(currentRange);
      currentRange = { start: newStart, end: newEnd };
    }
  }
  ranges.push(currentRange);

  // Calculate line numbers for hunks
  // We need to trace line numbers up to each hunk
  const hunks: string[] = [];

  for (const range of ranges) {
    let oldLineNum = 1;
    let newLineNum = 1;

    for (let idx = 0; idx < range.start; idx++) {
      const op = ops[idx]!;
      if (op.type === 'equal' || op.type === 'delete') oldLineNum++;
      if (op.type === 'equal' || op.type === 'insert') newLineNum++;
    }

    const hunkOldStart = oldLineNum;
    const hunkNewStart = newLineNum;
    let oldCount = 0;
    let newCount = 0;
    const hunkLines: string[] = [];

    for (let idx = range.start; idx <= range.end; idx++) {
      const op = ops[idx]!;
      if (op.type === 'equal') {
        oldCount++;
        newCount++;
        hunkLines.push(` ${op.line}`);
      } else if (op.type === 'delete') {
        oldCount++;
        hunkLines.push(`-${op.line}`);
      } else if (op.type === 'insert') {
        newCount++;
        hunkLines.push(`+${op.line}`);
      }
    }

    const header = `@@ -${hunkOldStart},${oldCount} +${hunkNewStart},${newCount} @@`;
    hunks.push([header, ...hunkLines].join('\n'));
  }

  const fileHeaders: string[] = [];
  if (options.oldHeader) {
    fileHeaders.push(`--- ${options.oldHeader}`);
  }
  if (options.newHeader) {
    fileHeaders.push(`+++ ${options.newHeader}`);
  }

  if (fileHeaders.length > 0) {
    return `${fileHeaders.join('\n')}\n${hunks.join('\n')}`;
  }

  return hunks.join('\n');
}
