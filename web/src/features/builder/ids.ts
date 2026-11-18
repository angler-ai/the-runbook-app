// UI-4: step and runbook ID rules (LANG7), without the YAML parser, so the
// creation dialog (in the main bundle) can use them.

export const STEP_ID = /^[a-z][a-z0-9_]{0,62}$/;
export const RUNBOOK_ID = /^[a-z0-9]+(-[a-z0-9]+)*$/;

/** LANG7's derived title: `check-health` → "Check health", `ping_api` → "Ping api". */
export function derivedTitle(id: string): string {
  const text = id
    .split(/[-_]/)
    .filter((w) => w !== '')
    .join(' ');
  return text === '' ? id : text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * Words CEL reserves: a step with one of these IDs cannot be referenced
 * (`steps.loop`), so the compiler refuses it (expression.schema_error).
 */
export const RESERVED_IDS = new Set([
  'as',
  'break',
  'const',
  'continue',
  'else',
  'false',
  'for',
  'function',
  'if',
  'import',
  'in',
  'let',
  'loop',
  'namespace',
  'null',
  'package',
  'return',
  'true',
  'var',
  'void',
  'while',
]);

/** A step ID from a title: lowercase words joined by `_`, starting with a letter. */
export function idFromTitle(title: string): string {
  let id = title
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '');
  if (id === '') return 'step';
  if (!/^[a-z]/.test(id)) id = `step_${id}`;
  id = id.slice(0, 63).replace(/_+$/g, '');
  return RESERVED_IDS.has(id) ? `${id}_step` : id;
}

/** `base`, else `base_2`, `base_3`… whichever is free. */
export function uniqueId(base: string, taken: ReadonlySet<string>): string {
  const stem = RESERVED_IDS.has(base) ? `${base}_step` : base.slice(0, 58);
  if (!taken.has(stem)) return stem;
  for (let n = 2; ; n += 1) {
    const candidate = `${stem}_${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}
