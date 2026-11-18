// UI-4: the builder's view of a draft, read straight from the YAML.
//
// The compiler's outline disappears on any normalization error (an unknown
// key, a duplicate ID, a call with no runbook yet), which is exactly when
// someone building a runbook needs the graph. So the builder reads the YAML
// itself, leniently, into the same `Outline` shape the graph draws (paths are
// the same JSON pointers the compiler uses), and the compiler only validates.
// Steps are addressed by pointer, never by ID: IDs may be missing or repeated
// while someone is typing.
import { isMap, isSeq, LineCounter, parseDocument, type Document } from 'yaml';
import type {
  Outline,
  OutlineActor,
  OutlineApproval,
  OutlineBranch,
  OutlineNode,
  OutlineNodeKind,
  OutlineOutput,
  OutlineWait,
} from '../../ui/types';
import { pointerSegments } from '../editor/document';
import { branchBody, childBody, TOP_FINALLY, TOP_STEPS } from '../../ui/graph/bodies';
import { parseSource, usesForbiddenYaml, type YamlPath } from './yamlEdit';

export type Raw = Record<string, unknown>;

export { derivedTitle, idFromTitle, RESERVED_IDS, RUNBOOK_ID, STEP_ID, uniqueId } from './ids';
import { derivedTitle } from './ids';

export function toPointer(path: YamlPath): string {
  return path.length === 0
    ? ''
    : `/${path.map((s) => String(s).replace(/~/g, '~0').replace(/\//g, '~1')).join('/')}`;
}

export function toPath(pointer: string): (string | number)[] {
  return pointerSegments(pointer);
}

/** The control key that makes a step a container or a control node; tasks have none. */
const KIND_KEYS: [string, OutlineNodeKind][] = [
  ['steps', 'group'],
  ['parallel', 'parallel'],
  ['foreach', 'foreach'],
  ['loop', 'loop'],
  ['switch', 'switch'],
  ['wait', 'wait'],
  ['approval', 'approval'],
  ['set', 'set'],
  ['end', 'end'],
  ['call', 'call'],
];

export function kindOf(step: Raw): OutlineNodeKind {
  for (const [key, kind] of KIND_KEYS) if (key in step) return kind;
  return 'task';
}

export type BodyKind =
  | 'steps'
  | 'finally'
  | 'group'
  | 'catch'
  | 'group_finally'
  | 'parallel'
  | 'foreach'
  | 'loop'
  | 'case'
  | 'default';

/** A list steps can be inserted into. */
export interface BodyInfo {
  pointer: string;
  kind: BodyKind;
  /** "Top level", "Preflight › on failure", "Risk gate › case inputs.risk == 'high'". */
  label: string;
  /** The step that holds this list (absent for the runbook's own lists). */
  owner?: string;
  /** How many steps are in it now. */
  size: number;
  depth: number;
}

export interface StepInfo {
  pointer: string;
  /** The graph's node ID: the step ID, or a stand-in when it is missing or repeated. */
  graphId: string;
  id: string | null;
  kind: OutlineNodeKind;
  title: string;
  body: string;
  index: number;
  /** Pointers of the bodies this step holds (containers only). */
  bodies: string[];
  node: OutlineNode;
}

export interface ParseProblem {
  message: string;
  line: number;
  column: number;
}

export interface BuilderModel {
  outline: Outline;
  steps: StepInfo[];
  byGraphId: Map<string, StepInfo>;
  byPointer: Map<string, StepInfo>;
  bodies: Map<string, BodyInfo>;
  /** Every step ID written, for uniqueness. */
  ids: Set<string>;
  /** The runbook's `defaults.actor` (what a step without `actor` gets). */
  defaultActor: OutlineActor;
  /** `metadata` and `spec` as written (for the settings panel). */
  raw: { metadata: Raw; spec: Raw };
}

export type ReadResult =
  | {
      ok: true;
      model: BuilderModel;
      document: Document;
      /**
       * Review round 1: why the builder may only show this YAML, not edit it
       * (anchors, aliases or tags, a byte order mark: the compiler refuses
       * them, so the builder does not edit around them).
       */
      locked?: string;
    }
  | { ok: false; problems: ParseProblem[] };

const FORBIDDEN_YAML =
  'This YAML uses anchors, aliases or tags (&name, *name, !tag), which runbooks do not allow, so the builder only shows it. Remove them in the YAML tab to edit here.';
const BYTE_ORDER_MARK =
  'The file starts with a byte order mark (an invisible U+FEFF), which runbooks do not allow: the compiler reads it as part of the first key. Remove it in the YAML tab to edit here.';

function str(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function num(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function obj(value: unknown): Raw | undefined {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Raw) : undefined;
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function strings(value: unknown): string[] {
  return list(value).filter((v): v is string => typeof v === 'string');
}

function actorOf(value: unknown): OutlineActor | undefined {
  return value === 'human' || value === 'agent' || value === 'any' ? value : undefined;
}

/** `${{ inputs.risk == 'high' }}` → `inputs.risk == 'high'` for edge labels. */
export function bareExpression(text: string): string {
  const match = /^\s*\$\{\{\s*([\s\S]*?)\s*\}\}\s*$/.exec(text);
  return match ? (match[1] ?? text) : text;
}

function outputsOf(value: unknown): OutlineOutput[] | undefined {
  const map = obj(value);
  if (!map) return undefined;
  return Object.entries(map)
    .filter(([name]) => !name.startsWith('x-'))
    .map(([name, decl]) => {
      const d = obj(decl);
      return {
        name,
        type: str(d?.type) ?? 'string',
        ...(d?.required === true ? { required: true } : {}),
      };
    });
}

function approvalOf(value: unknown): OutlineApproval | undefined {
  const a = obj(value);
  if (!a) return undefined;
  const approvers = obj(a.approvers);
  const onTimeout = a.on_timeout;
  const decide = str(obj(onTimeout)?.decide);
  return {
    prompt: str(a.prompt) ?? '',
    options: list(a.options).flatMap((option) => {
      if (typeof option === 'string') return [{ id: option, title: option }];
      const o = obj(option);
      const id = str(o?.id);
      if (!o || !id) return [];
      return [{ id, title: str(o.title) ?? id, ...(o.auto === true ? { auto: true } : {}) }];
    }),
    approvers: [
      ...strings(approvers?.users).map((u) => `user:${u}`),
      ...strings(approvers?.teams).map((t) => `team:${t}`),
    ],
    min: num(a.min) ?? 1,
    ...(str(a.timeout) ? { timeout: str(a.timeout) } : {}),
    ...(onTimeout === 'escalate' ? { onTimeout: 'escalate' as const } : {}),
    ...(decide ? { onTimeout: 'decide' as const, timeoutDecision: decide } : {}),
    ...(list(a.escalate_to).length ? { escalateTo: strings(a.escalate_to) } : {}),
    ...(str(a.escalation_timeout) ? { escalationTimeout: str(a.escalation_timeout) } : {}),
    ...(a.allow_agents === true ? { allowAgents: true } : {}),
  };
}

function waitOf(value: unknown): OutlineWait | undefined {
  const w = obj(value);
  if (!w) return undefined;
  const timeout = str(w.timeout) ? { timeout: str(w.timeout) } : {};
  if (str(w.signal) !== undefined)
    return { kind: 'signal', signal: str(w.signal) ?? '', ...timeout };
  if (str(w.until) !== undefined) return { kind: 'until', until: str(w.until) ?? '', ...timeout };
  return { kind: 'for', duration: str(w.for) ?? '', ...timeout };
}

interface ReadContext {
  defaultActor: OutlineActor;
  seen: Set<string>;
  steps: StepInfo[];
  bodies: Map<string, BodyInfo>;
}

function readBody(
  value: unknown,
  pointer: string,
  kind: BodyKind,
  label: string,
  owner: string | undefined,
  depth: number,
  ctx: ReadContext,
): OutlineNode[] {
  const items = list(value);
  ctx.bodies.set(pointer, {
    pointer,
    kind,
    label,
    ...(owner ? { owner } : {}),
    size: items.length,
    depth,
  });
  return items.map((item, index) =>
    readStep(obj(item) ?? {}, `${pointer}/${index}`, pointer, index, depth, ctx),
  );
}

function readStep(
  step: Raw,
  pointer: string,
  body: string,
  index: number,
  depth: number,
  ctx: ReadContext,
): OutlineNode {
  const kind = kindOf(step);
  const id = str(step.id) ?? null;
  const graphId = id && !ctx.seen.has(id) ? id : `${id ?? 'step'}@${pointer}`;
  if (id) ctx.seen.add(id);
  const explicitTitle = str(step.title);
  const title = explicitTitle ?? (kind === 'task' && id ? derivedTitle(id) : undefined);
  const node: OutlineNode = {
    id: graphId,
    path: pointer,
    kind,
    ...(title ? { title } : {}),
  };
  if (str(step.if)) node.guard = str(step.if);
  if (step.on_failure === 'continue' || step.on_failure === 'handoff')
    node.onFailure = step.on_failure;
  if (str(step.timeout)) node.timeout = str(step.timeout);
  if (step.destructive === true) node.destructive = true;
  const info: StepInfo = {
    pointer,
    graphId,
    id,
    kind,
    title: title ?? id ?? '(no ID)',
    body,
    index,
    bodies: [],
    node,
  };
  ctx.steps.push(info);
  const name = title ?? id ?? 'step';
  const inner = depth + 1;

  switch (kind) {
    case 'task': {
      node.actor = actorOf(step.actor) ?? ctx.defaultActor;
      if (step.check === true) node.check = true;
      const instructions = step.instructions;
      const text =
        str(instructions) ??
        str(obj(instructions)?.default) ??
        str(obj(instructions)?.human) ??
        str(obj(instructions)?.agent);
      if (text) node.instructions = text;
      if (str(step.assignee)) node.assignee = str(step.assignee);
      if (str(step.skill)) node.skill = str(step.skill);
      const skills = strings(step.skills);
      if (skills.length) {
        node.skills = skills;
        if (!node.skill) node.skill = skills[0];
      } else if (node.skill) {
        node.skills = [node.skill];
      }
      if (strings(step.checklist).length) node.checklist = strings(step.checklist);
      const outputs = outputsOf(step.outputs);
      if (outputs?.length) node.outputs = outputs;
      const who = { agent: 'Agent', human: 'Human', any: 'Anyone' }[node.actor];
      node.hint = node.check ? `${who} · check` : who;
      break;
    }
    case 'group': {
      const own = `${pointer}/steps`;
      node.children = readBody(step.steps, own, 'group', name, pointer, inner, ctx);
      info.bodies.push(own);
      const branches: OutlineBranch[] = [];
      if ('catch' in step) {
        const lane = `${pointer}/catch`;
        branches.push({
          label: 'catch',
          kind: 'catch',
          children: readBody(
            step.catch,
            lane,
            'catch',
            `${name} › on failure`,
            pointer,
            inner,
            ctx,
          ),
        });
        info.bodies.push(lane);
      }
      if ('finally' in step) {
        const lane = `${pointer}/finally`;
        branches.push({
          label: 'finally',
          kind: 'finally',
          children: readBody(
            step.finally,
            lane,
            'group_finally',
            `${name} › finally`,
            pointer,
            inner,
            ctx,
          ),
        });
        info.bodies.push(lane);
      }
      if (branches.length) node.branches = branches;
      break;
    }
    case 'parallel': {
      const own = `${pointer}/parallel`;
      const join = step.join;
      const atLeast = num(obj(join)?.at_least);
      node.join =
        join === 'any'
          ? { kind: 'any' }
          : atLeast !== undefined
            ? { kind: 'at_least', count: atLeast }
            : { kind: 'all' };
      node.children = readBody(step.parallel, own, 'parallel', name, pointer, inner, ctx);
      info.bodies.push(own);
      break;
    }
    case 'foreach': {
      const f = obj(step.foreach) ?? {};
      node.foreach = {
        items: str(f.items) ?? '',
        as: str(f.as) ?? 'item',
        concurrency: num(f.concurrency) ?? 1,
        maxItems: num(f.max_items) ?? 0,
        ...(str(f.key) ? { key: str(f.key) } : {}),
      };
      const own = `${pointer}/foreach/steps`;
      node.children = readBody(f.steps, own, 'foreach', name, pointer, inner, ctx);
      info.bodies.push(own);
      break;
    }
    case 'loop': {
      const l = obj(step.loop) ?? {};
      node.loop = {
        ...(str(l.until) !== undefined ? { until: str(l.until) } : {}),
        ...(str(l.while) !== undefined ? { while: str(l.while) } : {}),
        maxIterations: num(l.max_iterations) ?? 0,
        ...(str(l.delay) ? { delay: str(l.delay) } : {}),
      };
      const own = `${pointer}/loop/steps`;
      node.children = readBody(l.steps, own, 'loop', name, pointer, inner, ctx);
      info.bodies.push(own);
      break;
    }
    case 'switch': {
      const s = obj(step.switch) ?? {};
      const branches: OutlineBranch[] = list(s.cases).map((c, i) => {
        const when = str(obj(c)?.when) ?? '';
        const own = `${pointer}/switch/cases/${i}/steps`;
        info.bodies.push(own);
        return {
          label: bareExpression(when) || `case ${i + 1}`,
          kind: 'case' as const,
          when,
          children: readBody(
            obj(c)?.steps,
            own,
            'case',
            `${name} › case ${bareExpression(when) || i + 1}`,
            pointer,
            inner,
            ctx,
          ),
        };
      });
      const own = `${pointer}/switch/default/steps`;
      info.bodies.push(own);
      branches.push({
        label: 'default',
        kind: 'default',
        children: readBody(
          obj(s.default)?.steps,
          own,
          'default',
          `${name} › default`,
          pointer,
          inner,
          ctx,
        ),
      });
      node.branches = branches;
      break;
    }
    case 'wait': {
      const wait = waitOf(step.wait);
      if (wait) node.wait = wait;
      break;
    }
    case 'approval': {
      const approval = approvalOf(step.approval);
      if (approval) node.approval = approval;
      // People decide; with allow_agents, agents may too (as the compiler says).
      node.actor = approval?.allowAgents ? 'any' : 'human';
      break;
    }
    case 'set': {
      const s = obj(step.set) ?? {};
      node.set = {
        ...(obj(s.fields) ? { fields: Object.keys(obj(s.fields) ?? {}) } : {}),
        ...(obj(s.vars) ? { vars: Object.keys(obj(s.vars) ?? {}) } : {}),
      };
      break;
    }
    case 'end': {
      const e = obj(step.end) ?? {};
      const status =
        e.status === 'failed' || e.status === 'cancelled' ? e.status : ('succeeded' as const);
      node.end = { status, ...(str(e.message) ? { message: str(e.message) } : {}) };
      break;
    }
    case 'call': {
      const c = obj(step.call) ?? {};
      node.call = {
        runbook: str(c.runbook) ?? '',
        ...(num(c.version) !== undefined ? { version: num(c.version) } : {}),
        ...(obj(c.with) ? { with: Object.keys(obj(c.with) ?? {}) } : {}),
      };
      break;
    }
  }
  return node;
}

/** Read the draft. Text that does not parse as YAML has no model (only its errors). */
export function readBuilder(text: string): ReadResult {
  const lines = new LineCounter();
  // A tab after a list dash is read as the compiler reads it (see parseSource).
  const document = parseDocument(parseSource(text), { lineCounter: lines });
  if (document.errors.length > 0) {
    return {
      ok: false,
      problems: document.errors.map((error) => {
        const at = error.linePos?.[0] ?? lines.linePos(error.pos[0]);
        return {
          message: error.message.split('\n')[0] ?? error.message,
          line: at.line,
          column: at.col,
        };
      }),
    };
  }
  const root = obj(document.toJS()) ?? {};
  const metadata = obj(root.metadata) ?? {};
  const spec = obj(root.spec) ?? {};
  const defaultActor = actorOf(obj(spec.defaults)?.actor) ?? 'any';
  const ctx: ReadContext = { defaultActor, seen: new Set(), steps: [], bodies: new Map() };
  const steps = readBody(spec.steps, TOP_STEPS, 'steps', 'Top level', undefined, 0, ctx);
  const finallySteps = readBody(spec.finally, TOP_FINALLY, 'finally', 'Finally', undefined, 0, ctx);
  const id = str(metadata.id) ?? '';
  const outline: Outline = {
    id,
    title: str(metadata.title) ?? (id ? derivedTitle(id) : 'Untitled runbook'),
    ...(str(metadata.description) ? { description: str(metadata.description) } : {}),
    steps,
    finally: finallySteps,
    stepBudget: null,
  };
  const ids = new Set(ctx.steps.flatMap((s) => (s.id ? [s.id] : [])));
  const locked = usesForbiddenYaml(document)
    ? FORBIDDEN_YAML
    : text.startsWith('\uFEFF')
      ? BYTE_ORDER_MARK
      : undefined;
  return {
    ok: true,
    ...(locked ? { locked } : {}),
    document,
    model: {
      outline,
      steps: ctx.steps,
      byGraphId: new Map(ctx.steps.map((s) => [s.graphId, s])),
      byPointer: new Map(ctx.steps.map((s) => [s.pointer, s])),
      bodies: ctx.bodies,
      ids,
      defaultActor,
      raw: { metadata, spec },
    },
  };
}

/** Whether `pointer` is `ancestor` or inside it. */
export function within(pointer: string, ancestor: string): boolean {
  return pointer === ancestor || pointer.startsWith(`${ancestor}/`);
}

/** The step a diagnostic's path points into (the deepest one). */
export function stepForPath(model: BuilderModel, path: string): StepInfo | undefined {
  let best: StepInfo | undefined;
  for (const step of model.steps) {
    if (within(path, step.pointer) && (!best || step.pointer.length > best.pointer.length))
      best = step;
  }
  return best;
}

/** The raw YAML value of a step (for its form). */
export function rawStep(document: Document, pointer: string): Raw {
  const value = document.getIn(toPath(pointer));
  if (isMap(value) || isSeq(value)) return (value.toJSON() as Raw) ?? {};
  return (value && typeof value === 'object' ? (value as Raw) : {}) ?? {};
}

export { childBody, branchBody };
