// UI-4: "Reload and reapply" after a draft conflict merges the author's
// changes (base → mine) onto the newer draft (base → theirs), line by line,
// and refuses when both touched the same lines.
import { describe, expect, it } from 'vitest';
import { PALETTE } from './kinds';
import { merge3, reapply } from './merge';
import { readBuilder } from './model';
import { deleteStep, duplicateStep, insertStep, moveStep, moveTargets, setStepField } from './ops';

const BASE = ['a: 1', 'b: 2', 'c: 3', 'd: 4', 'e: 5', ''].join('\n');

describe('merge3', () => {
  it('applies both sides’ changes when they are apart', () => {
    const mine = BASE.replace('b: 2', 'b: two');
    const theirs = BASE.replace('e: 5', 'e: five').replace('c: 3\n', 'c: 3\nc2: 3.5\n');
    expect(merge3(BASE, mine, theirs)).toEqual({
      ok: true,
      text: ['a: 1', 'b: two', 'c: 3', 'c2: 3.5', 'd: 4', 'e: five', ''].join('\n'),
    });
  });

  it('takes an identical change once', () => {
    const both = BASE.replace('c: 3', 'c: three');
    expect(merge3(BASE, both, both)).toEqual({ ok: true, text: both });
  });

  it('refuses overlapping or adjacent edits', () => {
    expect(
      merge3(BASE, BASE.replace('c: 3', 'c: mine'), BASE.replace('c: 3', 'c: theirs')).ok,
    ).toBe(false);
    expect(
      merge3(BASE, BASE.replace('c: 3', 'c: mine'), BASE.replace('d: 4', 'd: theirs')).ok,
    ).toBe(false);
  });

  it('refuses a step one side deleted and the other moved (both delete its lines)', () => {
    const base = 'steps:\n  - id: d\n  - id: e\n    actor: agent\nend: 1\n';
    const deleted = 'steps:\n  - id: d\nend: 1\n';
    const moved = 'steps:\n  - id: d\n    steps:\n      - id: e\n        actor: agent\nend: 1\n';
    expect(merge3(base, deleted, moved)).toEqual({ ok: false, reason: 'overlap' });
  });

  it('refuses the same line added by both sides in different places, at any indentation', () => {
    const base = ['a: 1', 'b: 2', 'c: 3', 'd: 4', 'e: 5', 'f: 6', ''].join('\n');
    const mine = base.replace('a: 1\n', 'a: 1\n- id: call\n');
    const theirs = base.replace('e: 5\n', 'e: 5\n    - id: call\n');
    expect(merge3(base, mine, theirs)).toEqual({ ok: false, reason: 'overlap' });
  });

  it('keeps theirs when I changed nothing, and mine when they changed nothing', () => {
    const theirs = BASE.replace('a: 1', 'a: one');
    expect(merge3(BASE, BASE, theirs)).toEqual({ ok: true, text: theirs });
    expect(merge3(BASE, theirs, BASE)).toEqual({ ok: true, text: theirs });
  });

  it('handles insertions at the end and deletions', () => {
    const mine = `${BASE}f: 6\n`;
    const theirs = BASE.replace('a: 1\n', '');
    expect(merge3(BASE, mine, theirs)).toEqual({
      ok: true,
      text: ['b: 2', 'c: 3', 'd: 4', 'e: 5', 'f: 6', ''].join('\n'),
    });
  });
});

// UI-4 review round 1: "Reload and reapply" must never apply a change twice.
// The fuzz drives real builder edits (insert, duplicate, delete, move, set
// a field) as the review's probe did.
describe('merge3 fuzz', () => {
  const START = `apiVersion: runbook/v1\nkind: Runbook\nmetadata:\n  id: demo\nspec:\n  steps:\n    - id: a\n      actor: agent\n\n    - id: b\n      actor: agent\n\n    # gate\n    - id: g\n      steps:\n        - id: c\n          actor: agent\n        - id: d\n          actor: agent\n\n    - id: e\n      actor: agent\n  finally:\n    - id: z\n      actor: agent\n`;
  const KINDS = PALETTE.map((p) => p.kind);

  function fuzzer(seed: number) {
    let s = seed;
    const rnd = () => (s = (s * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
    const pick = <T>(items: readonly T[]): T => items[Math.floor(rnd() * items.length)] as T;
    const edit = (text: string): string => {
      const read = readBuilder(text);
      if (!read.ok) return text;
      const m = read.model;
      const bodies = [...m.bodies.values()];
      const which = rnd();
      if (which < 0.4) {
        const b = pick(bodies);
        return insertStep(
          text,
          m,
          { body: b.pointer, index: Math.floor(rnd() * (b.size + 1)) },
          pick(KINDS),
        ).text;
      }
      const step = pick(m.steps);
      if (which < 0.55) return duplicateStep(text, m, step.pointer).text;
      if (which < 0.7 && m.steps.length > 3) return deleteStep(text, step.pointer).text;
      if (which < 0.85) {
        const t = moveTargets(m, step);
        const choices = [t.up, t.down, t.out?.target, t.into?.target].filter(
          (x): x is NonNullable<typeof x> => Boolean(x),
        );
        if (choices.length === 0) return text;
        return moveStep(text, m, step.pointer, pick(choices))?.text ?? text;
      }
      return setStepField(text, step.pointer, ['actor'], pick(['human', 'agent', 'any'])).text;
    };
    return { rnd, edit };
  }

  const ids = (text: string) => {
    const read = readBuilder(text);
    return read.ok ? read.model.steps.map((step) => step.id) : null;
  };

  it('lost response: my earlier save is theirs, so reapply gives exactly mine', () => {
    const { rnd, edit } = fuzzer(777);
    let cases = 0;
    let duplicated = 0;
    for (let t = 0; t < 4000; t += 1) {
      let base = START;
      for (let i = Math.floor(rnd() * 4); i > 0; i -= 1) base = edit(base);
      const theirs = edit(base); // saved; the response was lost
      let mine = theirs;
      for (let i = 1 + Math.floor(rnd() * 2); i > 0; i -= 1) mine = edit(mine);
      if (theirs === base || mine === theirs) continue;
      cases += 1;
      expect(reapply({ base, unconfirmed: [theirs], mine, theirs })).toEqual({
        ok: true,
        text: mine,
      });
      // merge3 alone cannot know `theirs` is mine (an edit that undoes it looks
      // like "unchanged here"), but it never applies a step twice.
      const r = merge3(base, mine, theirs);
      if (r.ok) {
        const merged = ids(r.text) ?? [];
        if (new Set(merged).size !== merged.length) duplicated += 1;
      }
    }
    expect(cases).toBeGreaterThan(3000);
    expect(duplicated).toBe(0);
  }, 120_000);

  it('lost response, then someone else saved on top: reapply refuses', () => {
    const { rnd, edit } = fuzzer(99);
    for (let t = 0; t < 300; t += 1) {
      const sent = edit(START);
      const theirs = edit(sent);
      const mine = edit(sent);
      if (sent === START || theirs === sent) continue;
      expect(reapply({ base: START, unconfirmed: [sent], mine, theirs })).toEqual({
        ok: false,
        reason: 'unconfirmed',
      });
    }
    expect(rnd()).toBeGreaterThanOrEqual(0);
  });

  it('two people from one base: a merge parses, keeps every step once, and has no duplicate IDs', () => {
    const { rnd, edit } = fuzzer(4242);
    let bad = 0;
    let merged = 0;
    for (let t = 0; t < 4000; t += 1) {
      let base = START;
      for (let i = Math.floor(rnd() * 4); i > 0; i -= 1) base = edit(base);
      const a = edit(base);
      const b = edit(base);
      if (a === base || b === base || a === b) continue;
      const r = reapply({ base, unconfirmed: [], mine: a, theirs: b });
      expect(r).toEqual(merge3(base, a, b));
      if (!r.ok) continue;
      merged += 1;
      const m = readBuilder(r.text);
      const count = (text: string) => {
        const read = readBuilder(text);
        return read.ok ? read.model.steps.length : -1;
      };
      if (!m.ok) {
        bad += 1;
        continue;
      }
      const stepIds = m.model.steps.map((s) => s.id);
      if (new Set(stepIds).size !== stepIds.length) bad += 1;
      else if (m.model.steps.length !== count(a) + count(b) - count(base)) bad += 1;
    }
    expect(bad).toBe(0);
    // Strict, but still useful: edits in different steps combine.
    expect(merged).toBeGreaterThan(400);
  }, 120_000);
});
