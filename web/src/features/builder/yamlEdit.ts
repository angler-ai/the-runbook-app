// UI-4: builder edits on the YAML text, with minimal diffs.
//
// Every edit is made twice. The `yaml` package's Document API makes it on a
// parsed copy: that is the meaning of the edit (the expected document). The
// same edit is also made as a text splice at the edited node's source range:
// one scalar replaced, one pair or list item inserted or removed, one item's
// lines moved (re-indented, its comments and formatting kept). The spliced
// text is parsed again and must equal the expected document as data; if it
// does not (an odd layout the splicer does not handle), the result is the
// expected document serialized whole (`minimal: false`), so an edit is never
// wrong, only occasionally less tidy. Nothing else in the file moves: keys
// the builder does not model, comments, quoting, flow maps and blank lines
// outside the edited node stay byte for byte.
//
// Review round 1: a block value's source range runs past its trailing
// comments into the next key's indentation; a pair or item now ends at a
// line start, and comment lines at or left of its own column are left to
// the key after it. Missing or empty (`spec:`) parent maps are created. A
// file with CRLF line endings keeps them. Anchors, aliases and tags, which
// the compiler forbids, are refused (`ForbiddenYaml`). A tab right after a
// list dash, which the compiler accepts and the `yaml` package does not, is
// read as a space (`parseSource`, the same length, so ranges still hold).
import {
  Document,
  isAlias,
  isCollection,
  isMap,
  isNode,
  isPair,
  isScalar,
  isSeq,
  parseDocument,
  visit,
  YAMLMap,
  YAMLSeq,
  type Node,
  type Pair,
} from 'yaml';

export type YamlPath = readonly (string | number)[];

export interface EditResult {
  text: string;
  /** False when the text had to be serialized whole (see the header). */
  minimal: boolean;
  /**
   * UI-4b: the edit replaced a value that was not empty and not what the
   * edit needed (a list where `steps: 5` stood): its path and old text.
   */
  replaced?: { path: YamlPath; was: string };
}

/** How fresh YAML is written: no line folding, two-space indentation. */
const RENDER = { lineWidth: 0, minContentWidth: 0, indent: 2, flowCollectionPadding: true };

/** Thrown for an edit on text that does not parse: the builder is read-only then. */
export class UnparsableSource extends Error {
  constructor() {
    super('The YAML does not parse, so it cannot be edited from the builder.');
    this.name = 'UnparsableSource';
  }
}

/** Thrown for anchors, aliases and tags: the compiler refuses them, the builder does not edit them. */
export class ForbiddenYaml extends Error {
  constructor() {
    super(
      'This YAML uses anchors, aliases or tags (&name, *name, !tag), which runbooks do not allow, so the builder does not edit it.',
    );
    this.name = 'ForbiddenYaml';
  }
}

/**
 * The text as the `yaml` package should read it: a tab in the space after a
 * block list's dash (`-\tid: a`, accepted by the compiler) becomes a space.
 * The length never changes, so every source range is valid in `text`.
 */
export function parseSource(text: string): string {
  return text.includes('\t')
    ? text.replace(
        /^([ ]*-)([ \t]+)/gm,
        (_, dash: string, gap: string) => dash + gap.replace(/\t/g, ' '),
      )
    : text;
}

/** Whether the document uses an anchor, an alias or a tag (the compiler's yaml_feature_forbidden). */
export function usesForbiddenYaml(doc: Document): boolean {
  let found = false;
  visit(doc, {
    Alias() {
      found = true;
      return visit.BREAK;
    },
    Node(_, node) {
      if (isAlias(node)) return undefined;
      if (node.anchor || node.tag) {
        found = true;
        return visit.BREAK;
      }
      return undefined;
    },
  });
  return found;
}

function parse(text: string): Document {
  const doc = parseDocument(parseSource(text));
  if (doc.errors.length > 0) throw new UnparsableSource();
  if (usesForbiddenYaml(doc)) throw new ForbiddenYaml();
  return doc;
}

/**
 * Run an edit on text whose every line break is CR LF as LF text, and give
 * the result CR LF again (a file with mixed endings is edited as it is).
 */
function keepingLineEndings(text: string, edit: (lf: string) => EditResult): EditResult {
  if (!text.includes('\r\n') || /(^|[^\r])\n/.test(text)) return edit(text);
  const result = edit(text.replace(/\r\n/g, '\n'));
  return { ...result, text: result.text.replace(/\n/g, '\r\n') };
}

// ---------------------------------------------------------------- key order

const BODY_KEYS = new Set(['steps', 'parallel', 'catch', 'finally']);
const ROOT_ORDER = ['apiVersion', 'kind', 'metadata', 'spec'];
const METADATA_ORDER = ['id', 'title', 'description', 'owners', 'tags'];
const SPEC_ORDER = [
  'execution',
  'inputs',
  'triggers',
  'fields',
  'vars',
  'defaults',
  'skills',
  'agents',
  'steps',
  'finally',
  'outputs',
  'report',
];
const STEP_ORDER = [
  'id',
  'title',
  'if',
  'actor',
  'assignee',
  'assignment',
  'check',
  'destructive',
  'skippable',
  'allow_auto_decision',
  'skill',
  'instructions',
  'with',
  'checklist',
  'outputs',
  'evidence',
  'agent',
  'retry',
  'timeout',
  'on_failure',
  'join',
  'fail_fast',
  'parallel',
  'steps',
  'catch',
  'finally',
  'foreach',
  'loop',
  'switch',
  'wait',
  'approval',
  'set',
  'end',
  'call',
];
const DECLARATION_ORDER = [
  'type',
  'title',
  'description',
  'required',
  'default',
  'options',
  'items',
  'properties',
  'min',
  'max',
  'pattern',
  'indexed',
  'locked',
];
const MAP_ORDERS: Record<string, readonly string[]> = {
  metadata: METADATA_ORDER,
  spec: SPEC_ORDER,
  execution: ['title', 'tags', 'timeout', 'cleanup_timeout', 'concurrency'],
  concurrency: ['key', 'limit', 'on_conflict'],
  defaults: ['actor', 'timeout', 'retry', 'on_failure'],
  retry: ['max_attempts', 'backoff'],
  evidence: ['required', 'accepted'],
  approval: [
    'prompt',
    'options',
    'approvers',
    'min',
    'timeout',
    'on_timeout',
    'escalate_to',
    'escalation_timeout',
    'allow_agents',
    'on_expired',
  ],
  approvers: ['users', 'teams'],
  wait: ['signal', 'for', 'until', 'timeout'],
  foreach: ['items', 'as', 'key', 'concurrency', 'max_items', 'steps'],
  loop: ['until', 'while', 'max_iterations', 'delay', 'steps'],
  switch: ['cases', 'default'],
  call: ['runbook', 'version', 'with', 'outputs'],
  set: ['vars', 'fields'],
  end: ['status', 'message'],
};

/** The order new keys take in the map at `path` (unknown keys sort last). */
export function keyOrder(path: YamlPath): readonly string[] {
  if (path.length === 0) return ROOT_ORDER;
  const last = path[path.length - 1];
  const parent = path[path.length - 2];
  if (typeof last === 'number') {
    if (typeof parent === 'string' && BODY_KEYS.has(parent)) return STEP_ORDER;
    if (parent === 'cases') return ['when', 'steps'];
    if (parent === 'options') return ['id', 'title', 'auto'];
    return [];
  }
  if (parent === 'inputs' || parent === 'fields' || parent === 'outputs' || parent === 'properties')
    return DECLARATION_ORDER;
  if (last === 'items' && path.length >= 3) return DECLARATION_ORDER;
  return (last !== undefined && MAP_ORDERS[last]) || [];
}

// ------------------------------------------------------------- text helpers

function lineStartOf(text: string, pos: number): number {
  return text.lastIndexOf('\n', pos - 1) + 1;
}

/** The position after the newline that ends the line holding `pos` (or the end). */
function lineEndOf(text: string, pos: number): number {
  const nl = text.indexOf('\n', pos);
  return nl === -1 ? text.length : nl + 1;
}

function columnOf(text: string, pos: number): number {
  return pos - lineStartOf(text, pos);
}

function startsLine(text: string, pos: number): boolean {
  return text.slice(lineStartOf(text, pos), pos).trim() === '';
}

function spaces(n: number): string {
  return ' '.repeat(Math.max(0, n));
}

/** Move `pos` (a line start) up over comment lines directly above it, not past `floor`. */
function leadingStart(text: string, pos: number, floor: number): number {
  let at = pos;
  while (at > floor) {
    const prev = lineStartOf(text, at - 1);
    if (prev < floor) break;
    if (/^\s*#/.test(text.slice(prev, at - 1))) at = prev;
    else break;
  }
  return at;
}

function indentOf(line: string): number {
  return line.length - line.trimStart().length;
}

/**
 * Where a node's own text ends (a line start): its content and the comment
 * lines after it, except comment lines at or left of `column` directly
 * above what follows, which belong to the next key or item. The `yaml`
 * package's end runs into the next key's indentation; that is never ours.
 */
function ownEnd(text: string, range: [number, number, number], column: number): number {
  let end = range[2];
  if (end < text.length && text[end - 1] !== '\n') end = lineStartOf(text, end);
  const content = range[1];
  const floor = content > 0 && text[content - 1] === '\n' ? content : lineEndOf(text, content);
  if (end < floor) return floor;
  while (end > floor) {
    const prev = lineStartOf(text, end - 1);
    if (prev < floor) break;
    const line = text.slice(prev, end - 1);
    if (/^\s*#/.test(line) && indentOf(line) <= column) end = prev;
    else break;
  }
  return end;
}

function splice(text: string, start: number, end: number, insert: string): string {
  return text.slice(0, start) + insert + text.slice(end);
}

/** Prefix every non-empty line but the first with `pad`. */
function indentRest(rendered: string, pad: string): string {
  return rendered
    .split('\n')
    .map((line, i) => (i === 0 || line === '' ? line : pad + line))
    .join('\n');
}

/** Prefix every non-empty line with `pad`; the result ends with a newline. */
function indentLines(rendered: string, pad: string): string {
  const body = rendered.endsWith('\n') ? rendered.slice(0, -1) : rendered;
  return `${body
    .split('\n')
    .map((line) => (line === '' ? line : pad + line))
    .join('\n')}\n`;
}

/** Shift every non-empty line by `delta` columns (removing only spaces). */
function shift(raw: string, delta: number): string {
  if (delta === 0) return raw;
  return raw
    .split('\n')
    .map((line) => {
      if (line === '') return line;
      if (delta > 0) return spaces(delta) + line;
      const lead = line.length - line.trimStart().length;
      return line.slice(Math.min(lead, -delta));
    })
    .join('\n');
}

// --------------------------------------------------------------- rendering

/** A node (or plain value) as YAML text at column 0, comments around it left out. */
function render(value: unknown): string {
  const doc = new Document();
  const node = isNode(value) ? value : doc.createNode(value);
  const saved = isNode(node)
    ? { before: node.commentBefore, comment: node.comment, space: node.spaceBefore }
    : null;
  if (isNode(node)) {
    node.commentBefore = undefined;
    node.comment = undefined;
    node.spaceBefore = undefined;
  }
  doc.contents = node as Node;
  try {
    return doc.toString(RENDER);
  } finally {
    if (isNode(node) && saved) {
      node.commentBefore = saved.before;
      node.comment = saved.comment;
      node.spaceBefore = saved.space;
    }
  }
}

/** Written on its own lines below its key (a block map or list with items). */
function isBlockValue(node: unknown): boolean {
  return isCollection(node) && !node.flow && node.items.length > 0;
}

/**
 * The house style of flow collections: maps padded (`{ type: string }`),
 * lists not (`[a, b]`). Quoted text is left alone.
 */
function tidyFlow(text: string): string {
  let out = '';
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i] as string;
    if (quote) {
      out += c;
      if (quote === '"' && c === '\\') {
        out += text[i + 1] ?? '';
        i += 1;
      } else if (c === quote) {
        if (quote === "'" && text[i + 1] === "'") {
          out += "'";
          i += 1;
        } else quote = null;
      }
      continue;
    }
    if (c === '"' || c === "'") quote = c;
    if (c === '[' && text[i + 1] === ' ') {
      out += '[';
      i += 1;
      continue;
    }
    if (c === ' ' && text[i + 1] === ']') continue;
    out += c;
  }
  return out;
}

function renderInline(node: unknown): string {
  const raw = render(node).replace(/\n$/, '');
  const text = /^[[{]/.test(raw) ? tidyFlow(raw) : raw;
  const [head, ...rest] = text.split('\n');
  // A block scalar at the document root has its content at column 0; as a
  // value it is indented one level below its key.
  if (rest.length > 0 && /^[|>][-+0-9]*$/.test(head ?? '')) {
    return [head, ...rest.map((line) => (line === '' ? line : `  ${line}`))].join('\n');
  }
  return text;
}

/** `key: value` lines at `pad`, ending with a newline. */
function renderPair(key: unknown, value: unknown, pad: string, seqIndent: number): string {
  const keyText = renderInline(key);
  if (isBlockValue(value)) {
    const inner = spaces(pad.length + (isSeq(value) ? seqIndent : 2));
    return `${pad}${keyText}:\n${indentLines(render(value), inner)}`;
  }
  return `${pad}${keyText}: ${indentRest(renderInline(value), pad)}\n`;
}

/** `- item` lines with the dash at `dashCol` and the content at `contentCol`. */
function renderItem(value: unknown, dashCol: number, contentCol: number): string {
  const lead = `${spaces(dashCol)}-${spaces(contentCol - dashCol - 1)}`;
  const inner = render(value).replace(/\n$/, '');
  const pad = spaces(contentCol);
  const lines = inner.split('\n');
  return `${lead}${lines[0]}\n${lines
    .slice(1)
    .map((line) => (line === '' ? '\n' : `${pad}${line}\n`))
    .join('')}`;
}

// ----------------------------------------------------------- tree lookups

function nodeAt(doc: Document, path: YamlPath): unknown {
  return path.length === 0 ? doc.contents : doc.getIn(path, true);
}

function pairOf(map: YAMLMap, key: string | number): Pair | undefined {
  return map.items.find((p) => isPair(p) && keyValue(p) === key) as Pair | undefined;
}

function keyValue(pair: Pair): unknown {
  return isScalar(pair.key) ? pair.key.value : pair.key;
}

function rangeOf(node: unknown): [number, number, number] | null {
  return isNode(node) && node.range ? node.range : null;
}

/** The column of a list's dashes in the source (indentation of nested lists). */
function detectSeqIndent(doc: Document, text: string): number {
  let found: number | null = null;
  const visit = (node: unknown, keyCol: number | null) => {
    if (found !== null) return;
    if (isMap(node)) {
      for (const pair of node.items) {
        const key = rangeOf(pair.key);
        visit(pair.value, key ? columnOf(text, key[0]) : null);
      }
    } else if (isSeq(node)) {
      const first = rangeOf(node.items[0]);
      if (!node.flow && first && keyCol !== null) {
        const dash = text.lastIndexOf('-', first[0] - 1);
        if (dash >= 0) found = columnOf(text, dash) - keyCol;
        return;
      }
      for (const item of node.items) visit(item, null);
    }
  };
  visit(doc.contents, null);
  return found === null || found < 0 ? 2 : found;
}

/** Where a pair's text ends: the start of the line after its value. */
function pairEnd(text: string, pair: Pair): number {
  const key = rangeOf(pair.key);
  const value = rangeOf(pair.value);
  const keyLineEnd = key ? lineEndOf(text, key[0]) : 0;
  if (!value || (isScalar(pair.value) && pair.value.value === null)) return keyLineEnd;
  return Math.max(ownEnd(text, value, key ? columnOf(text, key[0]) : 0), keyLineEnd);
}

function itemDash(text: string, item: unknown): number | null {
  const range = rangeOf(item);
  if (!range) return null;
  const dash = text.lastIndexOf('-', range[0] - 1);
  if (dash < 0 || text.slice(dash + 1, range[0]).trim() !== '') return null;
  return dash;
}

/** An item's lines, with the comment lines directly above it: [start, end). */
function itemRegion(text: string, seq: YAMLSeq, index: number): [number, number] | null {
  const item = seq.items[index];
  const dash = itemDash(text, item);
  const range = rangeOf(item);
  if (dash === null || !range || !startsLine(text, dash)) return null;
  const previous = index > 0 ? seq.items[index - 1] : undefined;
  const floor = previous ? itemEnd(text, previous) : 0;
  return [leadingStart(text, lineStartOf(text, dash), floor), itemEnd(text, item)];
}

/** Where a list item's own text ends (see `ownEnd`; the column is its dash's). */
function itemEnd(text: string, item: unknown): number {
  const range = rangeOf(item) as [number, number, number];
  const dash = itemDash(text, item);
  return ownEnd(text, range, dash === null ? columnOf(text, range[0]) : columnOf(text, dash));
}

function sameData(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((item, i) => sameData(item, b[i]));
  }
  if (a instanceof Map || b instanceof Map) return false;
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const ka = Object.keys(a as object);
    const kb = Object.keys(b as object);
    if (ka.length !== kb.length) return false;
    return ka.every((k) =>
      sameData((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]),
    );
  }
  return Number.isNaN(a) && Number.isNaN(b);
}

/** The spliced text if it means exactly the expected document, else the whole document. */
function finish(spliced: string | null, expected: Document): EditResult {
  if (spliced !== null) {
    const check = parseDocument(parseSource(spliced));
    if (check.errors.length === 0 && sameData(check.toJS(), expected.toJS())) {
      return { text: spliced, minimal: true };
    }
  }
  return { text: expected.toString(RENDER), minimal: false };
}

// --------------------------------------------------------------- splicing

/**
 * Replace the value of `pair` (in a block map) from just after its colon:
 * a block value goes on the lines below, anything else after `: `.
 */
function replacePairValue(
  text: string,
  pair: Pair,
  next: unknown,
  seqIndent: number,
): string | null {
  const key = rangeOf(pair.key);
  if (!key) return null;
  const colon = text.indexOf(':', key[1]);
  if (colon < 0) return null;
  const end = pairEnd(text, pair);
  const keyCol = columnOf(text, key[0]);
  // UI-4b: a comment on the key's line (`steps: # todo`, `spec: null # why`)
  // stays on it.
  const comment = lineComment(text, colon + 1, pair.value);
  const note = comment ? ` ${comment}` : '';
  if (isBlockValue(next)) {
    const inner = spaces(keyCol + (isSeq(next) ? seqIndent : 2));
    return splice(text, colon + 1, end, `${note}\n${indentLines(render(next), inner)}`);
  }
  return splice(
    text,
    colon + 1,
    end,
    ` ${indentRest(renderInline(next), spaces(keyCol))}${note}\n`,
  );
}

/**
 * The comment that ends the line of `from` (just after a key's colon), after
 * `value` when the value starts on that line: `# todo` in `steps: # todo` or
 * `steps: null # todo`. Null when the line has none. Reading past the value
 * node's own range keeps a `#` inside quotes or a flow collection part of it.
 */
function lineComment(text: string, from: number, value: unknown): string | null {
  const nl = text.indexOf('\n', from);
  const eol = nl === -1 ? text.length : nl;
  const range = isNode(value) ? rangeOf(value) : null;
  // An empty value (`steps: # todo`) has a zero-length range: nothing to skip.
  const start =
    range && range[1] > range[0] && range[0] < eol && range[1] > from
      ? Math.min(range[1], eol)
      : from;
  // A comment always follows whitespace (`a# b` is a scalar).
  return /^\s+(#.*?)\s*$/.exec(text.slice(start, eol))?.[1] ?? null;
}

/** Replace a scalar or flow collection where it stands. */
function replaceInline(text: string, old: Node, next: unknown, pad: string): string | null {
  const range = rangeOf(old);
  if (!range) return null;
  let rendered = indentRest(renderInline(next), pad);
  const replaced = text.slice(range[0], range[1]);
  if (replaced.endsWith('\n') && !rendered.endsWith('\n')) rendered += '\n';
  // A value turned into a block scalar keeps its line's comment on the
  // header (`title: |- # note`), where a comment may stand.
  const [head, ...rest] = rendered.split('\n');
  const nl = text.indexOf('\n', range[1]);
  const tail = text.slice(range[1], nl === -1 ? text.length : nl);
  if (rest.length > 0 && /^[|>][-+0-9]*$/.test(head ?? '') && /^\s+#/.test(tail)) {
    const body = rest.join('\n').replace(/\n$/, '');
    return splice(text, range[0], nl === -1 ? text.length : nl, `${head} ${tail.trim()}\n${body}`);
  }
  return splice(text, range[0], range[1], rendered);
}

function isInlineNode(node: unknown): boolean {
  if (isScalar(node)) return node.value !== null || Boolean(node.source);
  return isCollection(node) && (node.flow === true || node.items.length === 0);
}

/** The continuation-line indentation for a value inside `parent` at `key`. */
function valuePad(text: string, parent: unknown, key: string | number): string {
  if (isMap(parent)) {
    const pair = pairOf(parent, key);
    const range = pair ? rangeOf(pair.key) : null;
    return spaces(range ? columnOf(text, range[0]) : 0);
  }
  if (isSeq(parent) && typeof key === 'number') {
    const range = rangeOf(parent.items[key]);
    return spaces(range ? columnOf(text, range[0]) - 2 : 0);
  }
  return '';
}

/** Splice in the node at `path` of `expected` over the same node of `original`. */
function replaceAt(
  text: string,
  original: Document,
  expected: Document,
  path: YamlPath,
  seqIndent: number,
): string | null {
  if (path.length === 0) return null;
  const parent = nodeAt(original, path.slice(0, -1));
  const key = path[path.length - 1] as string | number;
  const old = nodeAt(original, path);
  const next = nodeAt(expected, path);
  if (isCollection(parent) && parent.flow) {
    const nextParent = nodeAt(expected, path.slice(0, -1));
    const grand = nodeAt(original, path.slice(0, -2));
    return replaceInline(
      text,
      parent,
      nextParent,
      valuePad(text, grand, path[path.length - 2] as string | number),
    );
  }
  if (isInlineNode(old) && !isBlockValue(next)) {
    return replaceInline(text, old as Node, next, valuePad(text, parent, key));
  }
  if (isMap(parent)) {
    const pair = pairOf(parent, key);
    return pair ? replacePairValue(text, pair, next, seqIndent) : null;
  }
  return null;
}

/** Insert a new pair into a block map, at its canonical place. */
function insertPair(
  text: string,
  map: YAMLMap,
  mapPath: YamlPath,
  key: string,
  value: unknown,
  seqIndent: number,
): string | null {
  const first = map.items[0];
  const firstKey = first ? rangeOf(first.key) : null;
  const mapRange = rangeOf(map);
  if (!firstKey || !mapRange) return null;
  const pad = spaces(columnOf(text, firstKey[0]));
  const order = keyOrder(mapPath);
  const rank = (k: unknown) => {
    const i = typeof k === 'string' ? order.indexOf(k) : -1;
    return i < 0 ? Number.POSITIVE_INFINITY : i;
  };
  const own = rank(key);
  const last = map.items[map.items.length - 1];
  let pos = last ? pairEnd(text, last) : mapRange[2];
  if (own !== Number.POSITIVE_INFINITY) {
    const index = map.items.findIndex((p) => rank(keyValue(p)) > own);
    const before = index >= 0 ? map.items[index] : undefined;
    const beforeKey = before ? rangeOf(before.key) : null;
    if (before && beforeKey && startsLine(text, beforeKey[0])) {
      const previous = index > 0 ? map.items[index - 1] : undefined;
      const floor = previous ? pairEnd(text, previous) : 0;
      pos = leadingStart(text, lineStartOf(text, beforeKey[0]), floor);
    } else if (before) {
      pos = pairEnd(text, before);
    }
  }
  const lead = pos > 0 && text[pos - 1] !== '\n' ? '\n' : '';
  return splice(text, pos, pos, lead + renderPair(key, value, pad, seqIndent));
}

/** Insert `value` as item `index` of a non-empty block list. */
function insertItemText(text: string, seq: YAMLSeq, index: number, value: unknown): string | null {
  const first = seq.items[0];
  const dash = itemDash(text, first);
  const range = rangeOf(first);
  if (dash === null || !range) return null;
  const dashCol = columnOf(text, dash);
  const contentCol = columnOf(text, range[0]);
  const rendered = renderItem(value, dashCol, contentCol);
  if (index < seq.items.length) {
    const region = itemRegion(text, seq, index);
    if (!region) return null;
    return splice(text, region[0], region[0], rendered);
  }
  const last = seq.items[seq.items.length - 1];
  if (!rangeOf(last)) return null;
  const pos = itemEnd(text, last);
  const lead = pos > 0 && text[pos - 1] !== '\n' ? '\n' : '';
  return splice(text, pos, pos, lead + rendered);
}

// ------------------------------------------------------------------- edits

/** Turn missing or empty (`spec:`, `spec: null`) parents of `path` into empty collections. */
function ensureParents(doc: Document, path: YamlPath): void {
  const empty = (node: unknown) =>
    node === undefined || node === null || (isScalar(node) && node.value === null);
  if (empty(doc.contents)) doc.contents = new YAMLMap() as unknown as typeof doc.contents;
  for (let depth = 1; depth < path.length; depth += 1) {
    const prefix = path.slice(0, depth);
    if (empty(nodeAt(doc, prefix)))
      doc.setIn(prefix, typeof path[depth] === 'number' ? new YAMLSeq() : new YAMLMap());
  }
}

export interface SetOptions {
  /** Write a new map or list in flow style (`{ type: string }`). */
  flow?: boolean;
}

/** Set the value at `path`, creating missing parent maps. `undefined` deletes. */
function setInLf(
  text: string,
  path: YamlPath,
  value: unknown,
  options: SetOptions = {},
): EditResult {
  if (value === undefined) return deleteIn(text, path);
  const original = parse(text);
  const expected = parse(text);
  const existing = nodeAt(expected, path);
  if (isScalar(existing) && !isCollection(value) && (value === null || typeof value !== 'object')) {
    const wasMulti = typeof existing.value === 'string' && existing.value.includes('\n');
    const isMulti = typeof value === 'string' && value.includes('\n');
    // Keep the author's quoting, but not quotes the old value merely needed
    // ("2" while "2h" is typed): those are the stringifier's, not a choice.
    const forced = typeof existing.value === 'string' && /^["']/.test(renderInline(existing.value));
    if (wasMulti !== isMulti || typeof value !== typeof existing.value || value === '' || forced)
      existing.type = undefined;
    existing.value = value;
  } else {
    // A scalar that becomes a collection stays on its line (`join: { at_least: 2 }`),
    // and a flow collection replaced whole stays flow.
    const flow =
      options.flow === true ||
      (isScalar(existing) && existing.value !== null) ||
      (isCollection(existing) && existing.flow === true);
    ensureParents(expected, path);
    expected.setIn(path, isNode(value) ? value : expected.createNode(value, { flow }));
  }
  return finish(spliceSet(text, original, expected, path), expected);
}

/** Rename key `from` of the map at `mapPath` to `to`, where it stands. */
function renameKeyLf(text: string, mapPath: YamlPath, from: string, to: string): EditResult {
  const original = parse(text);
  const expected = parse(text);
  const map = nodeAt(expected, mapPath);
  const before = nodeAt(original, mapPath);
  if (!isMap(map) || !isMap(before) || from === to) return { text, minimal: true };
  if (pairOf(map, to) || !pairOf(map, from)) return { text, minimal: true };
  const pair = pairOf(map, from) as Pair;
  pair.key = expected.createNode(to);
  const old = pairOf(before, from)?.key;
  const range = rangeOf(old);
  return finish(
    range ? splice(text, range[0], range[1], renderInline(expected.createNode(to))) : null,
    expected,
  );
}

function spliceSet(
  text: string,
  original: Document,
  expected: Document,
  path: YamlPath,
): string | null {
  const seqIndent = detectSeqIndent(original, text);
  let depth = 0;
  while (depth < path.length && nodeAt(original, path.slice(0, depth + 1)) !== undefined)
    depth += 1;
  if (depth === path.length) return replaceAt(text, original, expected, path, seqIndent);
  const parentPath = path.slice(0, depth);
  const parent = nodeAt(original, parentPath);
  const key = path[depth];
  const value = nodeAt(expected, path.slice(0, depth + 1));
  if (isMap(parent) && !parent.flow && parent.items.length > 0 && typeof key === 'string') {
    return insertPair(text, parent, parentPath, key, value, seqIndent);
  }
  if (isSeq(parent) && !parent.flow && parent.items.length > 0 && key === parent.items.length) {
    return insertItemText(text, parent, key, value);
  }
  // An empty or flow parent, or a null value: rewrite the parent in place.
  return replaceAt(text, original, expected, parentPath, seqIndent);
}

export interface DeleteOptions {
  /** Also remove parent maps the deletion leaves empty (never a step or `spec`). */
  prune?: boolean;
}

/** Remove the key or list item at `path`. A missing path changes nothing. */
function deleteInLf(text: string, path: YamlPath, options: DeleteOptions = {}): EditResult {
  const probe = parse(text);
  if (nodeAt(probe, path) === undefined) return { text, minimal: true };
  let target = path;
  if (options.prune) {
    while (target.length >= 3 && typeof target[target.length - 2] === 'string') {
      const parent = nodeAt(probe, target.slice(0, -1));
      if (isMap(parent) && parent.items.length === 1) target = target.slice(0, -1);
      else break;
    }
  }
  const parentPath = target.slice(0, -1);
  const key = target[target.length - 1] as string | number;
  if (isSeq(nodeAt(probe, parentPath)) && typeof key === 'number') {
    return removeItem(text, parentPath, key);
  }
  const original = parse(text);
  const expected = parse(text);
  expected.deleteIn(target);
  const seqIndent = detectSeqIndent(original, text);
  const map = nodeAt(original, parentPath);
  if (!isMap(map)) return finish(null, expected);
  if (map.flow || map.items.length === 1) {
    return finish(
      parentPath.length === 0 ? null : replaceAt(text, original, expected, parentPath, seqIndent),
      expected,
    );
  }
  const index = map.items.findIndex((p) => keyValue(p) === key);
  const pair = map.items[index];
  const keyRange = pair ? rangeOf(pair.key) : null;
  if (!pair || !keyRange) return finish(null, expected);
  if (!startsLine(text, keyRange[0])) {
    // The first pair of a list item (`- title: …`): the next key moves up.
    const next = map.items[index + 1];
    const nextKey = next ? rangeOf(next.key) : null;
    return finish(nextKey ? splice(text, keyRange[0], nextKey[0], '') : null, expected);
  }
  const previous = index > 0 ? map.items[index - 1] : undefined;
  const floor = previous ? pairEnd(text, previous) : 0;
  const start = leadingStart(text, lineStartOf(text, keyRange[0]), floor);
  return finish(splice(text, start, pairEnd(text, pair), ''), expected);
}

/** Insert `value` at `index` of the list at `seqPath` (created when missing). */
function insertItemLf(text: string, seqPath: YamlPath, index: number, value: unknown): EditResult {
  const original = parse(text);
  const expected = parse(text);
  const seq = nodeAt(expected, seqPath);
  const node = expected.createNode(value);
  if (isSeq(seq)) {
    seq.items.splice(index, 0, node);
    if (seq.flow && isCollection(node)) seq.flow = false;
  } else {
    const fresh = new YAMLSeq();
    fresh.items.push(node);
    ensureParents(expected, seqPath);
    expected.setIn(seqPath, fresh);
  }
  const before = nodeAt(original, seqPath);
  const spliced =
    isSeq(before) && !before.flow && before.items.length > 0
      ? insertItemText(text, before, index, node)
      : spliceSet(text, original, expected, seqPath);
  const result = finish(spliced, expected);
  // UI-4b: a value that was neither a list nor empty (`steps: 5`) is gone;
  // the builder says so.
  if (!isSeq(before) && before !== undefined && !(isScalar(before) && before.value === null)) {
    const range = rangeOf(before);
    const was = range ? text.slice(range[0], range[1]).trim() : renderInline(before);
    return { ...result, replaced: { path: seqPath, was } };
  }
  return result;
}

/** Remove item `index` of the list at `seqPath`; an emptied list is written `[]`. */
function removeItemLf(text: string, seqPath: YamlPath, index: number): EditResult {
  const original = parse(text);
  const expected = parse(text);
  const seq = nodeAt(expected, seqPath);
  if (!isSeq(seq) || index < 0 || index >= seq.items.length) return { text, minimal: true };
  seq.items.splice(index, 1);
  if (seq.items.length === 0) seq.flow = true;
  const before = nodeAt(original, seqPath) as YAMLSeq;
  const seqIndent = detectSeqIndent(original, text);
  if (before.flow || before.items.length === 1) {
    return finish(replaceAt(text, original, expected, seqPath, seqIndent), expected);
  }
  const region = itemRegion(text, before, index);
  return finish(region ? splice(text, region[0], region[1], '') : null, expected);
}

export interface ItemRef {
  seq: YamlPath;
  index: number;
}

/**
 * Move a list item to `to` (an index among the destination's current items,
 * so moving down within a list names the position after the target). The
 * item's own lines move, re-indented, with the comments directly above it.
 */
function moveItemLf(text: string, from: ItemRef, to: ItemRef): EditResult {
  const original = parse(text);
  const expected = parse(text);
  const sourceSeq = nodeAt(expected, from.seq);
  if (!isSeq(sourceSeq) || from.index < 0 || from.index >= sourceSeq.items.length) {
    return { text, minimal: true };
  }
  const sameSeq = samePath(from.seq, to.seq);
  if (sameSeq && (to.index === from.index || to.index === from.index + 1)) {
    return { text, minimal: true };
  }
  // Resolve the destination before removing anything: indices shift after.
  let destination = nodeAt(expected, to.seq);
  if (!isSeq(destination)) {
    ensureParents(expected, to.seq);
    expected.setIn(to.seq, new YAMLSeq());
    destination = nodeAt(expected, to.seq);
  }
  const destSeq = destination as YAMLSeq;
  const [moved] = sourceSeq.items.splice(from.index, 1);
  const at = sameSeq && from.index < to.index ? to.index - 1 : to.index;
  destSeq.items.splice(at, 0, moved);
  if (destSeq.flow && isCollection(moved)) destSeq.flow = false;
  if (sourceSeq.items.length === 0) sourceSeq.flow = true;
  return finish(spliceMove(text, original, from, to), expected);
}

function samePath(a: YamlPath, b: YamlPath): boolean {
  return a.length === b.length && a.every((s, i) => s === b[i]);
}

function spliceMove(text: string, original: Document, from: ItemRef, to: ItemRef): string | null {
  const seqIndent = detectSeqIndent(original, text);
  const source = nodeAt(original, from.seq);
  if (!isSeq(source) || source.flow) return null;
  const region = itemRegion(text, source, from.index);
  const dash = itemDash(text, source.items[from.index]);
  if (!region || dash === null) return null;
  let raw = text.slice(region[0], region[1]);
  if (!raw.endsWith('\n')) raw += '\n';
  const sourceCol = columnOf(text, dash);

  // The removal: the item's lines, or `[]` when the list is left empty.
  let removal: { start: number; end: number; insert: string };
  if (source.items.length === 1) {
    const holder = nodeAt(original, from.seq.slice(0, -1));
    const pair = isMap(holder) ? pairOf(holder, from.seq[from.seq.length - 1] as string) : null;
    const key = pair ? rangeOf(pair.key) : null;
    if (!pair || !key) return null;
    const colon = text.indexOf(':', key[1]);
    removal = { start: colon + 1, end: pairEnd(text, pair), insert: ' []\n' };
  } else {
    removal = { start: region[0], end: region[1], insert: '' };
  }

  // The insertion: among the destination's items, or as a new block list.
  const dest = nodeAt(original, to.seq);
  let insertion: { start: number; end: number; insert: string };
  if (isSeq(dest) && !dest.flow && dest.items.length > 0) {
    const firstDash = itemDash(text, dest.items[0]);
    if (firstDash === null) return null;
    const lines = shift(raw, columnOf(text, firstDash) - sourceCol);
    if (to.index < dest.items.length) {
      const target = itemRegion(text, dest, to.index);
      if (!target) return null;
      insertion = { start: target[0], end: target[0], insert: lines };
    } else {
      const last = dest.items[dest.items.length - 1];
      if (!rangeOf(last)) return null;
      const at = itemEnd(text, last);
      const lead = at > 0 && text[at - 1] !== '\n' ? '\n' : '';
      insertion = { start: at, end: at, insert: lead + lines };
    }
  } else if (isSeq(dest) && dest.items.length === 0) {
    const holder = nodeAt(original, to.seq.slice(0, -1));
    const pair = isMap(holder) ? pairOf(holder, to.seq[to.seq.length - 1] as string) : null;
    const key = pair ? rangeOf(pair.key) : null;
    if (!pair || !key || holder === null || (isMap(holder) && holder.flow)) return null;
    const colon = text.indexOf(':', key[1]);
    const dashCol = columnOf(text, key[0]) + seqIndent;
    insertion = {
      start: colon + 1,
      end: pairEnd(text, pair),
      insert: `\n${shift(raw, dashCol - sourceCol)}`,
    };
  } else {
    // A missing or flow destination: the expected document is written whole.
    return null;
  }
  if (overlaps(removal, insertion)) return null;
  const [later, earlier] =
    removal.start >= insertion.start ? [removal, insertion] : [insertion, removal];
  let out = splice(text, later.start, later.end, later.insert);
  out = splice(out, earlier.start, earlier.end, earlier.insert);
  return out;
}

function overlaps(a: { start: number; end: number }, b: { start: number; end: number }): boolean {
  return a.start < b.end && b.start < a.end;
}

/** A fresh empty flow list (`[]`). */
export function emptyList(): YAMLSeq {
  const seq = new YAMLSeq();
  seq.flow = true;
  return seq;
}

// The edits, each keeping a CRLF file's line endings (see `keepingLineEndings`).

export function setIn(
  text: string,
  path: YamlPath,
  value: unknown,
  options: SetOptions = {},
): EditResult {
  return keepingLineEndings(text, (lf) => setInLf(lf, path, value, options));
}

export function renameKey(text: string, mapPath: YamlPath, from: string, to: string): EditResult {
  return keepingLineEndings(text, (lf) => renameKeyLf(lf, mapPath, from, to));
}

export function deleteIn(text: string, path: YamlPath, options: DeleteOptions = {}): EditResult {
  return keepingLineEndings(text, (lf) => deleteInLf(lf, path, options));
}

export function insertItem(
  text: string,
  seqPath: YamlPath,
  index: number,
  value: unknown,
): EditResult {
  return keepingLineEndings(text, (lf) => insertItemLf(lf, seqPath, index, value));
}

export function removeItem(text: string, seqPath: YamlPath, index: number): EditResult {
  return keepingLineEndings(text, (lf) => removeItemLf(lf, seqPath, index));
}

export function moveItem(text: string, from: ItemRef, to: ItemRef): EditResult {
  return keepingLineEndings(text, (lf) => moveItemLf(lf, from, to));
}
