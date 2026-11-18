// UI-4: templates. Each one reads in the builder, carries the runbook's ID
// and title, and compiles with no problems (with the real browser compiler
// when it is built; an approval needs its approvers, which the creator is).
import { describe, expect, it } from 'vitest';
import { parseDocument } from 'yaml';
import { loadCompiler, type RunbookCompiler } from '../../wasm/runbook';
import { readBuilder } from './model';
import { TEMPLATES, templateById, templateSource } from './templates';

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

const params = { id: 'release-checks', title: 'Release checks: "prod"', me: 'Riley Chen' };
const directory = { users: [{ id: 'usr_01ARZ3NDEKTSV4RRFFQ69G5FAA', name: 'Riley Chen' }] };

describe('templates', () => {
  it.each(TEMPLATES.map((t) => [t.id, t]))('%s fills in the ID and title and compiles', (_, t) => {
    const source = templateSource(t, params);
    const doc = parseDocument(source);
    expect(doc.errors).toEqual([]);
    expect(doc.getIn(['metadata', 'id'])).toBe('release-checks');
    expect(doc.getIn(['metadata', 'title'])).toBe('Release checks: "prod"');
    const read = readBuilder(source);
    expect(read.ok).toBe(true);
    if (compiler) expect(compiler.validate({ source, context: directory }).diagnostics).toEqual([]);
  });

  it('starts blank with no steps', () => {
    const read = readBuilder(templateSource(templateById('blank'), params));
    expect(read.ok && read.model.steps).toEqual([]);
  });

  it('makes post-deploy checks agentic', () => {
    const read = readBuilder(templateSource(templateById('post-deploy'), params));
    if (!read.ok) throw new Error('no model');
    const tasks = read.model.steps.filter((s) => s.kind === 'task');
    expect(tasks.every((s) => s.node.actor === 'agent')).toBe(true);
    expect(tasks.filter((s) => s.node.check).length).toBeGreaterThanOrEqual(3);
  });

  it('mixes people, agents, the customer, any-order steps and an approval in onboarding', () => {
    const source = templateSource(templateById('customer-onboarding'), params);
    const read = readBuilder(source);
    if (!read.ok) throw new Error('no model');
    const kinds = new Set(read.model.steps.map((s) => s.kind));
    expect([...kinds]).toEqual(expect.arrayContaining(['task', 'parallel', 'wait', 'approval']));
    const actors = new Set(read.model.steps.map((s) => s.node.actor).filter(Boolean));
    expect([...actors]).toEqual(expect.arrayContaining(['human', 'agent']));
    expect(source).toContain('approvers: { users: [Riley Chen] }');
    // Without a known creator the approval asks for approvers in the builder.
    const anonymous = templateSource(templateById('customer-onboarding'), {
      id: 'x',
      title: 'X',
    });
    expect(anonymous).not.toContain('approvers');
    if (compiler) {
      expect(compiler.validate({ source: anonymous }).diagnostics.map((d) => d.code)).toEqual([
        'spec.approvers_required',
      ]);
    }
  });

  it('falls back to blank for an unknown template', () => {
    expect(templateById('nope').id).toBe('blank');
  });
});

// UI-4 review round 1: `String.replace` reads `$$`, `$&`, `$'` and `$1` in
// the replacement as patterns; a name or approver must come through as typed.
describe('names with $', () => {
  it.each(['Refund $$ review', 'Costs $& more', "Ops $' thing", 'Pay $` back', '$1 deal'])(
    '%s',
    (title) => {
      for (const template of TEMPLATES) {
        const src = templateSource(template, { id: 'x', title, me: 'Dana $$ Lee' });
        const doc = parseDocument(src);
        expect(doc.errors).toEqual([]);
        const js = doc.toJS() as {
          metadata: { title: string };
          spec: { steps: { approval?: { approvers?: { users: string[] } } }[] };
        };
        expect(js.metadata.title).toBe(title);
        const approvers = js.spec.steps.find((s) => s.approval?.approvers)?.approval?.approvers;
        if (approvers) expect(approvers.users).toEqual(['Dana $$ Lee']);
      }
    },
  );
});
