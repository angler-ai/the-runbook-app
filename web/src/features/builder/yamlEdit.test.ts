// UI-4: builder edits change the YAML through the `yaml` Document API, and
// the text changes only where the edit is: comments, key order, quoting,
// flow maps, block scalars and unknown keys elsewhere stay byte for byte.
import { describe, expect, it } from 'vitest';
import { parseDocument } from 'yaml';
import { deleteIn, insertItem, moveItem, removeItem, renameKey, setIn } from './yamlEdit';

const SOURCE = `# Release checks — owned by on-call.
apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: checks   # never rename
  title: "Checks"
  x-team-note: keep me
spec:
  inputs:
    service: { type: string, required: true }
  steps:
    - id: health
      title: Health answers 200
      check: true
      instructions: |
        Call the health endpoint.
        Pass on 200.
      x-runbook-ui: { color: blue }
    # The per-region checks
    - id: regions
      parallel:
        - id: us
        - id: eu
    - id: done
      end: { status: succeeded }
  finally:
  - id: summary
`;

/** Lines that differ between two texts, as a compact diff for assertions. */
function changed(before: string, after: string): { removed: string[]; added: string[] } {
  const a = before.split('\n');
  const b = after.split('\n');
  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start += 1;
  let endA = a.length - 1;
  let endB = b.length - 1;
  while (endA >= start && endB >= start && a[endA] === b[endB]) {
    endA -= 1;
    endB -= 1;
  }
  return { removed: a.slice(start, endA + 1), added: b.slice(start, endB + 1) };
}

const js = (text: string) => parseDocument(text).toJS() as Record<string, unknown>;

describe('setIn', () => {
  it('replaces one scalar and keeps its quoting and the trailing comment', () => {
    const result = setIn(SOURCE, ['metadata', 'title'], 'Release checks');
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text)).toEqual({
      removed: ['  title: "Checks"'],
      added: ['  title: "Release checks"'],
    });
  });

  it('drops quotes a previous value needed but the new one does not', () => {
    // Typing "2h" passes through "2", which must be quoted to stay a string.
    let text = setIn(SOURCE, ['spec', 'steps', 2, 'timeout'], '2').text;
    expect(text).toContain('      timeout: "2"\n');
    text = setIn(text, ['spec', 'steps', 2, 'timeout'], '2h').text;
    expect(text).toContain('      timeout: 2h\n');
    // Quotes the author chose stay.
    expect(setIn(SOURCE, ['metadata', 'title'], 'Other').text).toContain('  title: "Other"\n');
  });

  it('adds a missing key in canonical order (title after id)', () => {
    const result = setIn(SOURCE, ['spec', 'steps', 1, 'title'], 'Regions');
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text)).toEqual({
      removed: [],
      added: ['      title: Regions'],
    });
    expect(result.text).toContain('    - id: regions\n      title: Regions\n      parallel:');
  });

  it('creates missing parent maps as block maps', () => {
    const result = setIn(SOURCE, ['spec', 'execution', 'timeout'], '1h');
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text).added).toEqual(['  execution:', '    timeout: 1h']);
    // Canonical order: execution comes before inputs.
    expect(result.text).toContain('spec:\n  execution:\n    timeout: 1h\n  inputs:');
  });

  it('edits inside a flow map and keeps it a flow map', () => {
    const result = setIn(SOURCE, ['spec', 'inputs', 'service', 'required'], false);
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text)).toEqual({
      removed: ['    service: { type: string, required: true }'],
      added: ['    service: { type: string, required: false }'],
    });
  });

  it('rewrites a block scalar as a block scalar', () => {
    const result = setIn(SOURCE, ['spec', 'steps', 0, 'instructions'], 'One line.\nTwo lines.\n');
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text)).toEqual({
      removed: ['        Call the health endpoint.', '        Pass on 200.'],
      added: ['        One line.', '        Two lines.'],
    });
  });

  it('writes a new multi-line value as an indented block scalar', () => {
    const result = setIn(SOURCE, ['spec', 'steps', 2, 'instructions'], 'First.\nSecond.');
    expect(result.minimal).toBe(true);
    expect((js(result.text).spec as { steps: { instructions?: string }[] }).steps[2]).toEqual({
      id: 'done',
      instructions: 'First.\nSecond.',
      end: { status: 'succeeded' },
    });
    expect(result.text).toContain('      instructions: |-\n        First.\n        Second.\n');
  });

  it('turns a scalar into a flow map in place', () => {
    const text = 'spec:\n  steps:\n    - id: p\n      join: any\n      parallel: []\n';
    const result = setIn(text, ['spec', 'steps', 0, 'join'], { at_least: 2 });
    expect(result.minimal).toBe(true);
    expect(result.text).toBe(
      'spec:\n  steps:\n    - id: p\n      join: { at_least: 2 }\n      parallel: []\n',
    );
  });

  it('adds a key to a map whose last value is a block scalar', () => {
    const result = setIn(SOURCE, ['spec', 'steps', 0, 'timeout'], '5m');
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text).added).toEqual(['      timeout: 5m']);
    expect((js(result.text).spec as { steps: { timeout?: string }[] }).steps[0]?.timeout).toBe(
      '5m',
    );
  });

  it('keeps unknown keys and x- extensions untouched', () => {
    const result = setIn(SOURCE, ['spec', 'steps', 0, 'title'], 'Health');
    expect(result.text).toContain('      x-runbook-ui: { color: blue }\n');
    expect(result.text).toContain('  x-team-note: keep me\n');
    expect(result.text.startsWith('# Release checks — owned by on-call.\n')).toBe(true);
  });
});

describe('deleteIn', () => {
  it('removes one pair and nothing else', () => {
    const result = deleteIn(SOURCE, ['spec', 'steps', 0, 'check']);
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text)).toEqual({ removed: ['      check: true'], added: [] });
  });

  it('removes a pair with a block scalar value', () => {
    const result = deleteIn(SOURCE, ['spec', 'steps', 0, 'instructions']);
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text).removed).toEqual([
      '      instructions: |',
      '        Call the health endpoint.',
      '        Pass on 200.',
    ]);
  });

  it('prunes a map left empty', () => {
    const text =
      'spec:\n  steps:\n    - id: a\n      retry:\n        max_attempts: 2\n      timeout: 1m\n';
    const result = deleteIn(text, ['spec', 'steps', 0, 'retry', 'max_attempts'], { prune: true });
    expect(result.text).toBe('spec:\n  steps:\n    - id: a\n      timeout: 1m\n');
  });

  it('removes the first pair of a list item map', () => {
    const text = 'spec:\n  steps:\n    - title: First\n      id: a\n';
    const result = deleteIn(text, ['spec', 'steps', 0, 'title']);
    expect(result.text).toBe('spec:\n  steps:\n    - id: a\n');
  });

  it('removes a key from a flow map', () => {
    const result = deleteIn(SOURCE, ['spec', 'inputs', 'service', 'required']);
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text).added).toEqual(['    service: { type: string }']);
  });
});

describe('insertItem and removeItem', () => {
  it('inserts a step before another and keeps that step’s comment with it', () => {
    const result = insertItem(SOURCE, ['spec', 'steps'], 1, { id: 'smoke', check: true });
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text)).toEqual({
      removed: [],
      added: ['    - id: smoke', '      check: true'],
    });
    expect(result.text).toContain(
      '      x-runbook-ui: { color: blue }\n    - id: smoke\n      check: true\n    # The per-region checks\n    - id: regions',
    );
  });

  it('appends at the end of a body', () => {
    const result = insertItem(SOURCE, ['spec', 'steps', 1, 'parallel'], 2, { id: 'ap' });
    expect(result.minimal).toBe(true);
    expect(result.text).toContain('        - id: eu\n        - id: ap\n    - id: done');
  });

  it('follows a sequence written without indentation', () => {
    const result = insertItem(SOURCE, ['spec', 'finally'], 1, { id: 'notify' });
    expect(result.minimal).toBe(true);
    expect(result.text.endsWith('  finally:\n  - id: summary\n  - id: notify\n')).toBe(true);
  });

  it('turns an empty flow body into a block list', () => {
    const text = 'spec:\n  steps:\n    - id: g\n      steps: []\n    - id: after\n';
    const result = insertItem(text, ['spec', 'steps', 0, 'steps'], 0, { id: 'inner' });
    expect(result.minimal).toBe(true);
    expect(result.text).toBe(
      'spec:\n  steps:\n    - id: g\n      steps:\n        - id: inner\n    - id: after\n',
    );
  });

  it('creates a missing body', () => {
    const text = 'spec:\n  steps:\n    - id: a\n';
    const result = insertItem(text, ['spec', 'finally'], 0, { id: 'cleanup' });
    expect(result.text).toBe('spec:\n  steps:\n    - id: a\n  finally:\n    - id: cleanup\n');
  });

  it('inserts nested containers as block YAML', () => {
    const text = 'spec:\n  steps:\n    - id: a\n';
    const result = insertItem(text, ['spec', 'steps'], 1, {
      id: 'checks',
      parallel: [{ id: 'one' }, { id: 'two' }],
    });
    expect(result.text).toBe(
      'spec:\n  steps:\n    - id: a\n    - id: checks\n      parallel:\n        - id: one\n        - id: two\n',
    );
  });

  it('removes a step with its leading comment', () => {
    const result = removeItem(SOURCE, ['spec', 'steps'], 1);
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text).removed).toEqual([
      '    # The per-region checks',
      '    - id: regions',
      '      parallel:',
      '        - id: us',
      '        - id: eu',
    ]);
  });

  it('leaves an empty body as []', () => {
    const text = 'spec:\n  steps:\n    - id: g\n      steps:\n        - id: only\n    - id: b\n';
    const result = removeItem(text, ['spec', 'steps', 0, 'steps'], 0);
    expect(result.text).toBe('spec:\n  steps:\n    - id: g\n      steps: []\n    - id: b\n');
  });
});

describe('moveItem', () => {
  it('moves a step down within its body, comments and formatting intact', () => {
    const result = moveItem(
      SOURCE,
      { seq: ['spec', 'steps'], index: 0 },
      {
        seq: ['spec', 'steps'],
        index: 2,
      },
    );
    expect(result.minimal).toBe(true);
    const steps = (js(result.text).spec as { steps: { id: string }[] }).steps.map((s) => s.id);
    expect(steps).toEqual(['regions', 'health', 'done']);
    expect(result.text).toContain(
      '        - id: eu\n    - id: health\n      title: Health answers 200\n      check: true\n      instructions: |\n        Call the health endpoint.\n',
    );
  });

  it('moves a step into a container, re-indented', () => {
    const result = moveItem(
      SOURCE,
      { seq: ['spec', 'steps'], index: 2 },
      {
        seq: ['spec', 'steps', 1, 'parallel'],
        index: 0,
      },
    );
    expect(result.minimal).toBe(true);
    expect(result.text).toContain(
      '      parallel:\n        - id: done\n          end: { status: succeeded }\n        - id: us\n',
    );
  });

  it('moves a step out of a container', () => {
    const result = moveItem(
      SOURCE,
      { seq: ['spec', 'steps', 1, 'parallel'], index: 1 },
      {
        seq: ['spec', 'steps'],
        index: 3,
      },
    );
    expect(result.minimal).toBe(true);
    expect(result.text).toContain(
      '        - id: us\n    - id: done\n      end: { status: succeeded }\n    - id: eu\n  finally:',
    );
  });

  it('moves into an empty body', () => {
    const text = 'spec:\n  steps:\n    - id: g\n      steps: []\n    - id: b\n      title: B\n';
    const result = moveItem(
      text,
      { seq: ['spec', 'steps'], index: 1 },
      {
        seq: ['spec', 'steps', 0, 'steps'],
        index: 0,
      },
    );
    expect(result.text).toBe(
      'spec:\n  steps:\n    - id: g\n      steps:\n        - id: b\n          title: B\n',
    );
  });
});

describe('whole values and keys', () => {
  it('keeps a flow list a flow list when it is replaced whole', () => {
    const text = 'spec:\n  steps:\n    - id: a\n      checklist: [one, two]\n      title: A\n';
    const result = setIn(text, ['spec', 'steps', 0, 'checklist'], ['one', 'two', 'three']);
    expect(result.minimal).toBe(true);
    expect(result.text).toBe(
      'spec:\n  steps:\n    - id: a\n      checklist: [one, two, three]\n      title: A\n',
    );
  });

  it('writes a new small map as a flow map when asked', () => {
    const result = setIn(SOURCE, ['spec', 'inputs', 'version'], { type: 'string' }, { flow: true });
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text).added).toEqual(['    version: { type: string }']);
  });

  it('renames a key where it stands', () => {
    const result = renameKey(SOURCE, ['spec', 'inputs'], 'service', 'app');
    expect(result.minimal).toBe(true);
    expect(changed(SOURCE, result.text)).toEqual({
      removed: ['    service: { type: string, required: true }'],
      added: ['    app: { type: string, required: true }'],
    });
  });

  it('refuses to rename onto an existing key', () => {
    const text = 'a:\n  x: 1\n  y: 2\n';
    expect(renameKey(text, ['a'], 'x', 'y').text).toBe(text);
  });
});

// UI-4 review round 1.
const COMMENTED = `metadata:
  id: demo
spec:
  steps:
    - id: a
      retry:
        max_attempts: 3
        # inside retry, trailing
      # comment about timeout
      timeout: 5m
      # end-of-a comment
    - id: b # tb
      title: short # note
      with: { a: 1, # in flow
              b: 2 }
      checklist:
        - one # c1
        # between
        - two
      # after checklist
`;

describe('comments that belong to the next key', () => {
  it('deleting a block map keeps the comment above the key after it', () => {
    const r = deleteIn(COMMENTED, ['spec', 'steps', 0, 'retry']);
    expect(r.minimal).toBe(true);
    expect(changed(COMMENTED, r.text)).toEqual({
      removed: ['      retry:', '        max_attempts: 3', '        # inside retry, trailing'],
      added: [],
    });
  });

  it('pruning an emptied map keeps it too', () => {
    const r = deleteIn(COMMENTED, ['spec', 'steps', 0, 'retry', 'max_attempts'], { prune: true });
    expect(r.minimal).toBe(true);
    expect(r.text).toContain('      # comment about timeout\n      timeout: 5m');
    expect(r.text).not.toContain('retry');
  });

  it('adds a pair at the end of a map whose trailing comments run into the next key', () => {
    const r = setIn(COMMENTED, ['spec', 'steps', 0, 'retry', 'backoff'], '5s');
    expect(r.minimal).toBe(true);
    expect(changed(COMMENTED, r.text)).toEqual({ removed: [], added: ['        backoff: 5s'] });
  });

  it('removes and moves a step whose trailing comments run into the next item', () => {
    const removed = removeItem(COMMENTED, ['spec', 'steps'], 0);
    expect(removed.minimal).toBe(true);
    expect(removed.text).toContain('      with: { a: 1, # in flow\n              b: 2 }');
    expect(
      removed.text.startsWith('metadata:\n  id: demo\nspec:\n  steps:\n    - id: b # tb\n'),
    ).toBe(true);
    const moved = moveItem(
      COMMENTED,
      { seq: ['spec', 'steps'], index: 0 },
      { seq: ['spec', 'steps'], index: 2 },
    );
    expect(moved.minimal).toBe(true);
    expect(moved.text).toContain('      with: { a: 1, # in flow\n              b: 2 }');
    expect(moved.text).toContain('      # comment about timeout\n');
  });

  it('edits one list item in place, keeping comments on the others', () => {
    const r = setIn(COMMENTED, ['spec', 'steps', 1, 'checklist', 1], 'three');
    expect(r.minimal).toBe(true);
    expect(changed(COMMENTED, r.text)).toEqual({
      removed: ['        - two'],
      added: ['        - three'],
    });
    const gone = removeItem(COMMENTED, ['spec', 'steps', 1, 'checklist'], 1);
    expect(gone.text).toContain('        - one # c1\n');
    const added = insertItem(COMMENTED, ['spec', 'steps', 1, 'checklist'], 2, 'three');
    expect(changed(COMMENTED, added.text)).toEqual({ removed: [], added: ['        - three'] });
  });
});

describe('a value with a trailing comment', () => {
  it('becomes a block scalar with the comment on its header', () => {
    const r = setIn(COMMENTED, ['spec', 'steps', 1, 'title'], 'x\ny');
    expect(r.minimal).toBe(true);
    expect(changed(COMMENTED, r.text)).toEqual({
      removed: ['      title: short # note'],
      added: ['      title: |- # note', '        x', '        y'],
    });
  });
});

describe('missing or empty collections', () => {
  for (const [name, text] of [
    ['spec:', 'metadata:\n  id: demo\nspec:\n'],
    ['spec: null', 'metadata:\n  id: demo\nspec: null\n'],
    ['spec: ~', 'metadata:\n  id: demo\nspec: ~\n'],
    ['no spec', 'metadata:\n  id: demo\n'],
  ] as const) {
    it(`adds a step under ${name}`, () => {
      const r = insertItem(text, ['spec', 'steps'], 0, { id: 'n' });
      expect(js(r.text)).toEqual({ metadata: { id: 'demo' }, spec: { steps: [{ id: 'n' }] } });
    });
  }

  it('sets a field under an empty metadata', () => {
    const r = setIn('metadata:\nspec:\n  steps: []\n', ['metadata', 'title'], 'T');
    expect(js(r.text)).toEqual({ metadata: { title: 'T' }, spec: { steps: [] } });
  });

  it('moves a step into a list under an empty map', () => {
    const text = 'metadata:\n  id: demo\nspec:\n  steps:\n    - id: a\n    - id: b\n  finally:\n';
    const r = moveItem(
      text,
      { seq: ['spec', 'steps'], index: 0 },
      { seq: ['spec', 'finally'], index: 0 },
    );
    expect(js(r.text)).toEqual({
      metadata: { id: 'demo' },
      spec: { steps: [{ id: 'b' }], finally: [{ id: 'a' }] },
    });
  });
});

describe('YAML the compiler forbids', () => {
  it('refuses to edit anchors, aliases and tags with a named error', () => {
    for (const text of [
      'metadata:\n  id: demo\nspec:\n  defaults: &d\n    actor: human\n  steps:\n    - id: a\n',
      'metadata:\n  id: demo\nspec:\n  steps:\n    - id: a\n      with: *d\n',
      'metadata:\n  id: demo\nspec:\n  steps:\n    - id: a\n      title: !!str 123\n',
    ]) {
      expect(() => setIn(text, ['metadata', 'title'], 'T')).toThrow(/anchors, aliases or tags/);
    }
  });
});

describe('line endings', () => {
  const CRLF = SOURCE.replace(/\n/g, '\r\n');
  const lf = (text: string) => text.replace(/\r\n/g, '\n');
  const loneLf = (text: string) => /(^|[^\r])\n/.test(text);

  it('keeps CRLF on every edit', () => {
    const edits = [
      insertItem(CRLF, ['spec', 'steps'], 1, { id: 'n', instructions: 'a\nb\n' }),
      setIn(CRLF, ['spec', 'steps', 0, 'instructions'], 'x\ny\n'),
      setIn(CRLF, ['metadata', 'description'], 'D'),
      removeItem(CRLF, ['spec', 'steps'], 1),
      moveItem(CRLF, { seq: ['spec', 'steps'], index: 0 }, { seq: ['spec', 'steps'], index: 3 }),
      deleteIn(CRLF, ['spec', 'steps', 0, 'check']),
      renameKey(CRLF, ['spec', 'inputs'], 'service', 'svc'),
    ];
    const plain = [
      insertItem(SOURCE, ['spec', 'steps'], 1, { id: 'n', instructions: 'a\nb\n' }),
      setIn(SOURCE, ['spec', 'steps', 0, 'instructions'], 'x\ny\n'),
      setIn(SOURCE, ['metadata', 'description'], 'D'),
      removeItem(SOURCE, ['spec', 'steps'], 1),
      moveItem(SOURCE, { seq: ['spec', 'steps'], index: 0 }, { seq: ['spec', 'steps'], index: 3 }),
      deleteIn(SOURCE, ['spec', 'steps', 0, 'check']),
      renameKey(SOURCE, ['spec', 'inputs'], 'service', 'svc'),
    ];
    edits.forEach((r, i) => {
      expect(loneLf(r.text)).toBe(false);
      expect(lf(r.text)).toBe(plain[i]?.text);
    });
  });
});

describe('a tab after a list dash (the compiler accepts it)', () => {
  const TAB = 'metadata:\n  id: demo\nspec:\n  steps:\n    -\tid: a\n      title: A\n    - id: b\n';
  it('edits it without rewriting the file', () => {
    const r = setIn(TAB, ['spec', 'steps', 0, 'title'], 'B');
    expect(r.minimal).toBe(true);
    expect(r.text).toBe(TAB.replace('title: A', 'title: B'));
  });
});

// UI-4b item 3: a comment on an empty or null value's line, and a scalar
// replaced by a list.
describe('the line of a value that a list replaces', () => {
  const H = 'apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata:\n  id: a\n';
  const step = { id: 'task' };

  it.each([
    [
      'an empty value',
      `${H}spec:\n  steps: # todo\n  finally: []\n`,
      '  steps: # todo\n    - id: task\n  finally: []\n',
    ],
    ['null', `${H}spec:\n  steps: null # why\n`, '  steps: # why\n    - id: task\n'],
    ['a tilde', `${H}spec:\n  steps: ~ # tilde\n`, '  steps: # tilde\n    - id: task\n'],
    [
      'an empty flow list',
      `${H}spec:\n  steps: [] # empty\n`,
      '  steps: # empty\n    - id: task\n',
    ],
  ])('keeps the comment on %s’s line', (_, text, tail) => {
    const result = insertItem(text, ['spec', 'steps'], 0, step);
    expect(result).toEqual({ text: `${H}spec:\n${tail}`, minimal: true });
  });

  it('keeps the comment when the parent itself is empty or null', () => {
    for (const text of [`${H}spec: null # why\n`, `${H}spec: # why\n`]) {
      expect(insertItem(text, ['spec', 'steps'], 0, step)).toEqual({
        text: `${H}spec: # why\n  steps:\n    - id: task\n`,
        minimal: true,
      });
    }
  });

  it('says so when a scalar that is not empty is replaced', () => {
    const result = insertItem(`${H}spec:\n  steps: 5 # five\n`, ['spec', 'steps'], 0, step);
    expect(result).toEqual({
      text: `${H}spec:\n  steps: # five\n    - id: task\n`,
      minimal: true,
      replaced: { path: ['spec', 'steps'], was: '5' },
    });
  });

  it('keeps a comment inside quotes as the value, not as a comment', () => {
    const text = `${H}spec:\n  steps: "a # b"\n`;
    const result = insertItem(text, ['spec', 'steps'], 0, step);
    expect(result.text).toBe(`${H}spec:\n  steps:\n    - id: task\n`);
    expect(result.replaced).toEqual({ path: ['spec', 'steps'], was: '"a # b"' });
  });
});
