// UI-4: the builder's model and step operations. Every palette kind is
// inserted, deleted and moved (up, down, into and out of a container); the
// reader matches the compiler's outline on real runbooks; and new steps
// compile (with the real browser compiler when it is built).
import { describe, expect, it } from 'vitest';
import { parseDocument } from 'yaml';
import type { Outline, OutlineNode } from '../../ui/types';
import { loadCompiler, type RunbookCompiler } from '../../wasm/runbook';
import { PALETTE, type PaletteKind } from './kinds';
import {
  derivedTitle,
  idFromTitle,
  readBuilder,
  stepForPath,
  uniqueId,
  type BuilderModel,
} from './model';
import {
  deleteStep,
  duplicateStep,
  idProblem,
  insertStep,
  listEdit,
  moveStep,
  moveTargets,
  setStepField,
} from './ops';

type FsPromises = {
  readFile: (path: string) => Promise<Uint8Array<ArrayBuffer>>;
  access: (path: string) => Promise<void>;
};
const fs = (await import(/* @vite-ignore */ 'node:fs/promises' as string)) as FsPromises;
const here = decodeURIComponent(new URL(import.meta.url).pathname);
const wasmPath = `${here.slice(0, here.indexOf('/src/'))}/src/wasm/runbook.wasm`;
const compiler: RunbookCompiler | null = await fs.access(wasmPath).then(
  async () => loadCompiler(await fs.readFile(wasmPath)),
  () => null,
);
if (!compiler) console.warn('SKIPPED compiler checks: src/wasm/runbook.wasm is not built');
// Real runbooks from outside web/ (Vite serves only web/): the review seed's
// release train and the compiler's every-kind fixture.
const repo = here.slice(0, here.indexOf('/web/src/'));
const decode = (bytes: Uint8Array) => new TextDecoder().decode(bytes);
const releaseTrain = decode(
  await fs.readFile(`${repo}/scripts/dev/review-runbooks/release-train.yaml`),
);
const everyKind = decode(
  await fs.readFile(`${repo}/crates/runbook-web-wasm/tests/fixtures/every-kind.yaml`),
);

function model(text: string): BuilderModel {
  const read = readBuilder(text);
  if (!read.ok) throw new Error(read.problems.map((p) => p.message).join('; '));
  return read.model;
}

const BASE = `metadata:
  id: every
  title: Every kind
spec:
  steps:
    - id: first
      title: First
    - id: holder
      steps:
        - id: inside
`;

function shape(nodes: OutlineNode[]): unknown[] {
  return nodes.map((n) => ({
    id: n.id,
    kind: n.kind,
    path: n.path,
    title: n.title,
    actor: n.actor,
    children: n.children ? shape(n.children) : undefined,
    branches: n.branches?.map((b) => ({
      kind: b.kind,
      label: b.label,
      children: shape(b.children),
    })),
  }));
}

describe('reading the YAML', () => {
  it.each([
    ['release-train', releaseTrain],
    ['every-kind', everyKind],
  ])('matches the compiler’s outline for %s', (_, source) => {
    const ours = model(source).outline;
    if (!compiler) return;
    const theirs = compiler.outline({ source }).outline as Outline;
    expect(shape(ours.steps)).toEqual(shape(theirs.steps));
    expect(shape(ours.finally)).toEqual(shape(theirs.finally));
    expect(ours.title).toBe(theirs.title);
  });

  it('keeps drawing when the compiler would refuse (unknown key, duplicate ID, no call target)', () => {
    const m = model(`metadata:
  id: odd
spec:
  steps:
    - id: a
      surprise: true
    - id: a
    - call: {}
`);
    expect(m.steps.map((s) => [s.graphId, s.kind])).toEqual([
      ['a', 'task'],
      ['a@/spec/steps/1', 'task'],
      ['step@/spec/steps/2', 'call'],
    ]);
  });

  it('reports YAML syntax errors with their line', () => {
    const read = readBuilder('metadata:\n  id: x\nspec:\n  steps: [\n');
    expect(read.ok).toBe(false);
    if (!read.ok) expect(read.problems[0]?.line).toBeGreaterThan(0);
  });

  // UI-4 review round 1: where the `yaml` package and the compiler disagree,
  // the builder follows the compiler.
  it('reads a tab after a list dash as the compiler does', () => {
    const source =
      'metadata:\n  id: tabs\nspec:\n  steps:\n    -\tid: a\n      title: A\n    - id: b\n';
    const read = readBuilder(source);
    expect(read.ok).toBe(true);
    if (!read.ok) return;
    expect(read.locked).toBeUndefined();
    expect(read.model.steps.map((s) => s.id)).toEqual(['a', 'b']);
    if (!compiler) return;
    const theirs = compiler.outline({ source }).outline as Outline;
    expect(shape(read.model.outline.steps)).toEqual(shape(theirs.steps));
  });

  it('draws but locks a file with a byte order mark (the compiler does not read its first key)', () => {
    const source = '\uFEFFmetadata:\n  id: bom\nspec:\n  steps:\n    - id: a\n';
    const read = readBuilder(source);
    expect(read.ok && read.locked).toMatch(/byte order mark/);
    if (compiler)
      expect(compiler.validate({ source }).diagnostics.map((d) => d.code)).toContain(
        'spec.source_shape',
      );
  });

  it.each([
    [
      'an anchor',
      'metadata:\n  id: x\nspec:\n  defaults: &d\n    actor: human\n  steps:\n    - id: a\n',
    ],
    [
      'an alias',
      'metadata:\n  id: x\nx-d: &d { a: 1 }\nspec:\n  steps:\n    - id: a\n      with: *d\n',
    ],
    ['a tag', 'metadata:\n  id: x\nspec:\n  steps:\n    - id: a\n      title: !!str 123\n'],
  ])('draws but locks a file with %s, which the compiler forbids', (_, source) => {
    const read = readBuilder(source);
    expect(read.ok && read.locked).toMatch(/anchors, aliases or tags/);
    if (compiler)
      expect(compiler.validate({ source }).diagnostics.map((d) => d.code)).toContain(
        'spec.yaml_feature_forbidden',
      );
  });

  it('places diagnostics on the deepest step', () => {
    const m = model(releaseTrain);
    expect(stepForPath(m, '/spec/steps/1/switch/cases/0/steps/0/approval/approvers')?.id).toBe(
      'cab_review',
    );
    expect(stepForPath(m, '/spec/inputs/risk')).toBeUndefined();
  });

  it('derives titles and IDs as LANG7 does', () => {
    expect(derivedTitle('check-health')).toBe('Check health');
    expect(derivedTitle('ping_api')).toBe('Ping api');
    expect(idFromTitle('Check the error rate (p95)')).toBe('check_the_error_rate_p95');
    expect(idFromTitle('5xx rate')).toBe('step_5xx_rate');
    expect(uniqueId('task', new Set(['task', 'task_2']))).toBe('task_3');
  });
});

describe('inserting', () => {
  it.each(PALETTE.map((p) => p.kind))('inserts a %s with only added lines', (kind) => {
    const before = model(BASE);
    const result = insertStep(BASE, before, { body: '/spec/steps', index: 1 }, kind as PaletteKind);
    expect(result.minimal).toBe(true);
    // Nothing that was there changed: the old text is the new text minus a block.
    const added = result.text.length - BASE.length;
    expect(added).toBeGreaterThan(0);
    const after = model(result.text);
    const step = after.byPointer.get(result.pointer ?? '');
    const entry = PALETTE.find((p) => p.kind === kind);
    expect(step?.kind).toBe(entry?.outlineKind);
    expect(after.steps.find((s) => s.id === 'first')?.index).toBe(0);
    expect(after.steps.find((s) => s.id === 'holder')?.index).toBe(2);
    if (compiler) {
      const codes = compiler.validate({ source: result.text }).diagnostics.map((d) => d.code);
      // Approvals need approvers, a foreach its list and a call its runbook:
      // the panel asks for them, and the compiler says so meanwhile.
      const expected: Partial<Record<PaletteKind, string[]>> = {
        approval: ['spec.approvers_required'],
        foreach: ['expression.unknown_reference'],
        call: ['spec.required'],
      };
      if (expected[kind as PaletteKind]) expect(codes.length).toBeGreaterThan(0);
      else expect(codes).toEqual([]);
    }
  });

  it('gives each new step a unique ID, inside containers too', () => {
    let text = BASE;
    for (let i = 0; i < 3; i += 1) {
      text = insertStep(
        text,
        model(text),
        { body: '/spec/steps/1/steps', index: 1 },
        'parallel',
      ).text;
    }
    const m = model(text);
    expect(new Set(m.steps.map((s) => s.id)).size).toBe(m.steps.length);
    expect(m.ids.has('task_6')).toBe(true);
  });

  it('inserts into an empty body and at the end of a switch’s default', () => {
    let text = insertStep(BASE, model(BASE), { body: '/spec/steps', index: 2 }, 'switch').text;
    const sw = model(text).steps.find((s) => s.kind === 'switch');
    expect(sw?.bodies).toEqual([
      '/spec/steps/2/switch/cases/0/steps',
      '/spec/steps/2/switch/default/steps',
    ]);
    text = insertStep(
      text,
      model(text),
      { body: '/spec/steps/2/switch/default/steps', index: 0 },
      'end',
    ).text;
    const m = model(text);
    expect(m.byPointer.get('/spec/steps/2/switch/default/steps/0')?.kind).toBe('end');
    expect(text).toContain('      default:\n          steps:\n            - id: end\n');
  });
});

/** A runbook holding one step of every palette kind, top level, in order. */
function everyPaletteKind(): string {
  let text = 'metadata:\n  id: kinds\nspec:\n  steps:\n    - id: anchor\n';
  for (const entry of PALETTE) {
    const m = model(text);
    text = insertStep(
      text,
      m,
      { body: '/spec/steps', index: m.bodies.get('/spec/steps')?.size ?? 0 },
      entry.kind,
    ).text;
  }
  return text;
}

describe('deleting and moving every kind', () => {
  const all = everyPaletteKind();
  const kinds = model(all).steps.filter((s) => s.body === '/spec/steps' && s.id !== 'anchor');

  it.each(kinds.map((s) => [s.id, s.pointer]))('deletes %s and only its lines', (id, pointer) => {
    const result = deleteStep(all, pointer as string);
    expect(result.minimal).toBe(true);
    const after = model(result.text);
    expect(after.ids.has(id as string)).toBe(false);
    expect(after.bodies.get('/spec/steps')?.size).toBe(kinds.length);
  });

  it.each(kinds.map((s) => [s.id, s.pointer]))(
    'moves %s up, down, into a group and back out',
    (id, pointer) => {
      let text = all;
      let at = pointer as string;
      const step = () => model(text).byPointer.get(at);
      // Up.
      const up = moveTargets(model(text), step()!).up!;
      let r = moveStep(text, model(text), at, up)!;
      expect(r.minimal).toBe(true);
      text = r.text;
      at = r.pointer!;
      expect(step()?.id).toBe(id);
      // Down again.
      const down = moveTargets(model(text), step()!).down!;
      r = moveStep(text, model(text), at, down)!;
      text = r.text;
      at = r.pointer!;
      expect(at).toBe(pointer);
      // Into the first other container at the top level (the group, or the
      // parallel when moving the group itself).
      const container = () =>
        model(text).steps.find(
          (s) => s.body === '/spec/steps' && s.bodies.length > 0 && s.id !== id,
        )!;
      const group = container();
      r = moveStep(text, model(text), at, { body: group.bodies[0]!, index: 0 })!;
      expect(r.minimal).toBe(true);
      text = r.text;
      at = r.pointer!;
      expect(step()?.id).toBe(id);
      expect(step()?.body).toBe(container().bodies[0]);
      // And out, to just after the group.
      const out = moveTargets(model(text), step()!).out!;
      r = moveStep(text, model(text), at, out.target)!;
      text = r.text;
      at = r.pointer!;
      expect(step()?.id).toBe(id);
      expect(step()?.body).toBe('/spec/steps');
      if (compiler) {
        const before = compiler.validate({ source: all }).diagnostics.length;
        expect(compiler.validate({ source: text }).diagnostics.length).toBe(before);
      }
    },
  );

  it('refuses to move a container inside itself', () => {
    const m = model(all);
    const group = m.steps.find((s) => s.kind === 'group')!;
    expect(moveStep(all, m, group.pointer, { body: group.bodies[0]!, index: 0 })).toBeNull();
  });

  it('offers "into" only below a container and "out" only inside one', () => {
    const m = model(BASE);
    const first = m.steps.find((s) => s.id === 'first')!;
    const inside = m.steps.find((s) => s.id === 'inside')!;
    expect(moveTargets(m, first)).toMatchObject({ up: null, out: null, into: null });
    expect(moveTargets(m, inside).out?.owner.id).toBe('holder');
  });
});

describe('duplicating and editing', () => {
  it('duplicates a container with new IDs throughout', () => {
    const m = model(releaseTrain);
    const canary = m.steps.find((s) => s.id === 'canary')!;
    const result = duplicateStep(releaseTrain, m, canary.pointer);
    const after = model(result.text);
    const copy = after.byPointer.get(result.pointer!)!;
    expect(copy.id).toBe('canary_copy');
    expect(copy.title).toBe('Canary (copy)');
    expect(after.ids.has('canary_us_copy')).toBe(true);
    expect(new Set(after.steps.map((s) => s.id)).size).toBe(after.steps.length);
  });

  it('sets and removes fields, pruning emptied maps', () => {
    let text = setStepField(BASE, '/spec/steps/0', ['retry', 'max_attempts'], 3).text;
    expect(parseDocument(text).getIn(['spec', 'steps', 0, 'retry'])).toBeTruthy();
    text = setStepField(text, '/spec/steps/0', ['retry', 'max_attempts'], undefined).text;
    expect(text).toBe(BASE);
  });

  it('edits a string list in place: one item set, added or removed, other items untouched', () => {
    const text = `metadata:\n  id: x\nspec:\n  steps:\n    - id: a\n      checklist:\n        - { note: kept } # c0\n        - one # c1\n        # between\n        - two\n`;
    const raw = [{ note: 'kept' }, 'one', 'two'];
    const path = ['spec', 'steps', 0, 'checklist'];
    const typed = listEdit(text, path, raw, ['one', 'twos']);
    expect(typed.item).toBe(2);
    expect(typed.text).toBe(text.replace('        - two\n', '        - twos\n'));
    const removed = listEdit(text, path, raw, ['two']);
    expect(removed.text).toBe(text.replace('        - one # c1\n', ''));
    const added = listEdit(text, path, raw, ['one', 'two', '']);
    expect(added.text).toBe(`${text}        - ""\n`);
    // Reordered or several changes at once: the strings are rewritten, the rest stays.
    const swapped = listEdit(text, path, raw, ['two', 'one']);
    expect(parseDocument(swapped.text).toJS().spec.steps[0].checklist).toEqual([
      { note: 'kept' },
      'two',
      'one',
    ]);
    // The last string removed, with nothing else in the list: the key goes.
    expect(listEdit('a:\n  - only\n', ['a'], ['only'], []).text).toBe('{}\n');
    // No list yet.
    expect(listEdit('a: 1\n', ['b'], undefined, ['']).text).toBe('a: 1\nb:\n  - ""\n');
  });

  it('checks IDs for shape and uniqueness', () => {
    const m = model(BASE);
    expect(idProblem(m, '/spec/steps/0', 'inside')).toMatch(/already uses/);
    expect(idProblem(m, '/spec/steps/0', 'Bad-ID')).toMatch(/lowercase/);
    expect(idProblem(m, '/spec/steps/0', 'first')).toBeNull();
    expect(idProblem(m, '/spec/steps/0', 'loop')).toMatch(/reserved/);
    expect(idFromTitle('Loop')).toBe('loop_step');
  });
});
