// UI-4: the builder's step operations, as text in → text out. Each one is
// a yamlEdit primitive (Document API + minimal splice) addressed by the
// step's pointer, plus the pointer the step ends up at, so the builder can
// keep it selected and move focus to it.
import { parseDocument } from 'yaml';
import { parentBody } from '../../ui/graph/bodies';
import { paletteEntry, type PaletteKind } from './kinds';
import {
  RESERVED_IDS,
  STEP_ID,
  toPath,
  uniqueId,
  within,
  type BuilderModel,
  type Raw,
  type StepInfo,
} from './model';
import {
  deleteIn,
  insertItem,
  moveItem,
  removeItem,
  setIn,
  type EditResult,
  type YamlPath,
} from './yamlEdit';

export interface InsertTarget {
  /** Pointer of the list (`/spec/steps`, `/spec/steps/2/parallel`). */
  body: string;
  /** Position among the list's current steps. */
  index: number;
}

export interface OpResult extends EditResult {
  /** Where the affected step is now (absent after a delete). */
  pointer?: string;
}

export function insertStep(
  text: string,
  model: BuilderModel,
  target: InsertTarget,
  kind: PaletteKind,
  options: { callTarget?: string } = {},
): OpResult {
  const value = paletteEntry(kind).create({
    taken: new Set(model.ids),
    ...(options.callTarget ? { callTarget: options.callTarget } : {}),
  });
  const result = insertItem(text, toPath(target.body), target.index, value);
  return { ...result, pointer: `${target.body}/${target.index}` };
}

export function deleteStep(text: string, pointer: string): OpResult {
  const at = parentBody(pointer);
  if (!at) return { text, minimal: true };
  return removeItem(text, toPath(at.body), at.index);
}

/** Pointer `p` after the item `index` of `body` was removed. */
function afterRemoval(p: string, body: string, index: number): string {
  if (!p.startsWith(`${body}/`)) return p;
  const rest = p.slice(body.length + 1);
  const cut = rest.indexOf('/');
  const head = Number(cut < 0 ? rest : rest.slice(0, cut));
  if (!Number.isInteger(head) || head <= index) return p;
  return `${body}/${head - 1}${cut < 0 ? '' : rest.slice(cut)}`;
}

/** Why a step cannot go to `to`, or null when it can. */
export function moveProblem(model: BuilderModel, pointer: string, to: InsertTarget): string | null {
  if (within(to.body, pointer)) return 'A step cannot move inside itself.';
  if (!model.bodies.has(to.body)) return 'That place no longer exists.';
  return null;
}

export function moveStep(
  text: string,
  model: BuilderModel,
  pointer: string,
  to: InsertTarget,
): OpResult | null {
  const from = parentBody(pointer);
  if (!from || moveProblem(model, pointer, to)) return null;
  const result = moveItem(
    text,
    { seq: toPath(from.body), index: from.index },
    { seq: toPath(to.body), index: to.index },
  );
  const sameBody = from.body === to.body;
  const index = sameBody && from.index < to.index ? to.index - 1 : to.index;
  const body = sameBody ? to.body : afterRemoval(to.body, from.body, from.index);
  return { ...result, pointer: `${body}/${index}` };
}

/** The moves a step's menu offers, each with its target (null when not possible). */
export interface MoveTargets {
  up: InsertTarget | null;
  down: InsertTarget | null;
  /** Out of the container it is in, to just after that container. */
  out: { target: InsertTarget; owner: StepInfo } | null;
  /** Into the container just above it, at the end of its first list. */
  into: { target: InsertTarget; owner: StepInfo } | null;
}

export function moveTargets(model: BuilderModel, step: StepInfo): MoveTargets {
  const body = model.bodies.get(step.body);
  const size = body?.size ?? 0;
  const up = step.index > 0 ? { body: step.body, index: step.index - 1 } : null;
  const down = step.index < size - 1 ? { body: step.body, index: step.index + 2 } : null;
  let out: MoveTargets['out'] = null;
  const owner = body?.owner ? model.byPointer.get(body.owner) : undefined;
  if (owner) out = { target: { body: owner.body, index: owner.index + 1 }, owner };
  let into: MoveTargets['into'] = null;
  const previous = model.byPointer.get(`${step.body}/${step.index - 1}`);
  const firstBody = previous?.bodies[0];
  if (previous && firstBody) {
    into = {
      target: { body: firstBody, index: model.bodies.get(firstBody)?.size ?? 0 },
      owner: previous,
    };
  }
  return { up, down, out, into };
}

const STEP_BODIES = ['steps', 'parallel', 'catch', 'finally'] as const;

/** Give a copied step (and every step inside it) a new, unique ID. */
function renameCopy(step: Raw, taken: Set<string>, top: boolean): Raw {
  const copy: Raw = { ...step };
  if (typeof copy.id === 'string') {
    copy.id = uniqueId(`${copy.id}_copy`, taken);
    taken.add(copy.id as string);
  }
  if (top && typeof copy.title === 'string') copy.title = `${copy.title} (copy)`;
  const body = (value: unknown) =>
    Array.isArray(value)
      ? value.map((child) =>
          child && typeof child === 'object' ? renameCopy(child as Raw, taken, false) : child,
        )
      : value;
  for (const key of STEP_BODIES) if (key in copy) copy[key] = body(copy[key]);
  for (const key of ['foreach', 'loop'] as const) {
    const inner = copy[key];
    if (inner && typeof inner === 'object')
      copy[key] = { ...(inner as Raw), steps: body((inner as Raw).steps) };
  }
  const sw = copy.switch;
  if (sw && typeof sw === 'object') {
    const s = sw as Raw;
    copy.switch = {
      ...s,
      ...(Array.isArray(s.cases)
        ? {
            cases: s.cases.map((c) =>
              c && typeof c === 'object' ? { ...(c as Raw), steps: body((c as Raw).steps) } : c,
            ),
          }
        : {}),
      ...(s.default && typeof s.default === 'object'
        ? { default: { ...(s.default as Raw), steps: body((s.default as Raw).steps) } }
        : {}),
    };
  }
  return copy;
}

export function duplicateStep(text: string, model: BuilderModel, pointer: string): OpResult {
  const at = parentBody(pointer);
  const value = parseDocument(text).getIn(toPath(pointer));
  if (!at || !value || typeof value !== 'object') return { text, minimal: true };
  const json = (value as { toJSON(): unknown }).toJSON() as Raw;
  const copy = renameCopy(json, new Set(model.ids), true);
  const result = insertItem(text, toPath(at.body), at.index + 1, copy);
  return { ...result, pointer: `${at.body}/${at.index + 1}` };
}

/**
 * Set (or with `undefined`, remove) a field of a step. Removing prunes maps
 * left empty (`retry: {}` disappears with its last member).
 */
export function setStepField(
  text: string,
  pointer: string,
  key: YamlPath,
  value: unknown,
): OpResult {
  const path = [...toPath(pointer), ...key];
  const result =
    value === undefined ? deleteIn(text, path, { prune: true }) : setIn(text, path, value);
  return { ...result, pointer };
}

/** Set (or remove) a runbook-level field (`metadata.title`, `spec.defaults.actor`…). */
export function setRunbookField(text: string, path: YamlPath, value: unknown): EditResult {
  return value === undefined ? deleteIn(text, path, { prune: true }) : setIn(text, path, value);
}

/** Why `id` cannot be this step's ID, or null. */
export function idProblem(model: BuilderModel, pointer: string, id: string): string | null {
  if (id === '') return 'A step needs an ID.';
  if (!STEP_ID.test(id))
    return 'Use a lowercase letter first, then lowercase letters, digits and _ (at most 63).';
  if (RESERVED_IDS.has(id)) return `“${id}” is a reserved word in expressions; pick another ID.`;
  const owner = model.steps.find((s) => s.id === id && s.pointer !== pointer);
  if (owner) return `Another step (${owner.title}) already uses this ID.`;
  return null;
}

/**
 * Review round 1: a list of strings edited in a form (a checklist, tags,
 * approvers), changed in place. `raw` is the list as written (non-string
 * items included, which the form does not show); `next` is the form's new
 * list of its strings. One item changed, added at the end or removed is
 * that one edit at its index in the YAML, so comments on the other items
 * and the items the form does not show stay. Anything else rewrites the
 * strings where they stand. A list left with nothing goes, key and all.
 */
export function listEdit(
  text: string,
  path: YamlPath,
  raw: unknown,
  next: readonly string[],
): EditResult & { item?: number } {
  const items = Array.isArray(raw) ? raw : [];
  /** YAML index of each string the form shows. */
  const at = items.flatMap((item, i) => (typeof item === 'string' ? [i] : []));
  const before = at.map((i) => items[i] as string);
  if (next.length === 0 && items.length === before.length) {
    return before.length === 0 ? { text, minimal: true } : deleteIn(text, path, { prune: true });
  }
  if (!Array.isArray(raw)) return setIn(text, path, [...next]);
  if (next.length === before.length) {
    const changed = next.flatMap((value, i) => (value === before[i] ? [] : [i]));
    if (changed.length === 0) return { text, minimal: true };
    if (changed.length === 1) {
      const index = at[changed[0] as number] as number;
      return { ...setIn(text, [...path, index], next[changed[0] as number]), item: index };
    }
  } else if (next.length === before.length + 1 && before.every((v, i) => v === next[i])) {
    return insertItem(text, path, items.length, next[next.length - 1]);
  } else if (next.length === before.length - 1) {
    const gone = before.findIndex((v, i) => v !== next[i]);
    const index = gone < 0 ? before.length - 1 : gone;
    if (before.filter((_, i) => i !== index).every((v, i) => v === next[i])) {
      return removeItem(text, path, at[index] as number);
    }
  }
  // Several changes at once: the strings, in their new order, where strings stand.
  let k = 0;
  const merged = items.flatMap((item) =>
    typeof item === 'string' ? (k < next.length ? [next[k++]] : []) : [item],
  );
  merged.push(...next.slice(k));
  return setIn(text, path, merged);
}
