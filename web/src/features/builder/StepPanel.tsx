// UI-4: the selected step's form. It shows only the fields the step's kind
// has, grouped, with what the runbook would do when a field is empty. Every
// field is optional except the ID, which follows the title for a step made
// in this session until it is edited by hand, and must stay unique. Every
// change is written to the YAML at once (typing in one field is one undo).
import { AlertCircle, ExternalLink, Plus, Trash2, X } from 'lucide-react';
import { useId, useMemo, useState, type ReactNode } from 'react';
import { parseDocument } from 'yaml';
import { useVersionSource } from '../runbooks/hooks';
import { KindIcon, kindWord } from '../../ui/graph/RunbookNodeComponent';
import { TemplateText } from '../../ui/graph/TemplateText';
import { Input } from '../../ui/primitives/input';
import type { OutlineNode } from '../../ui/types';
import { useBuilder, type BuilderApi } from './context';
import {
  CheckField,
  FieldShell,
  IconButton,
  NumberField,
  Section,
  SegmentedField,
  SelectField,
  SmallButton,
  StringListField,
  TextAreaField,
  TextField,
} from './fields';
import { derivedTitle, idFromTitle, rawStep, toPath, uniqueId, type Raw } from './model';
import { idProblem, insertStep, listEdit, setStepField } from './ops';
import { deleteIn, insertItem, removeItem, renameKey, setIn, type YamlPath } from './yamlEdit';

// ------------------------------------------------------------------ helpers

function str(value: unknown): string {
  return typeof value === 'string' ? value : value == null ? '' : String(value);
}

function num(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function obj(value: unknown): Raw {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Raw) : {};
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string') : [];
}

/** Keys of a step the form shows; problems on other keys are listed at the top. */
const FORM_KEYS = new Set([
  'id',
  'title',
  'if',
  'actor',
  'assignee',
  'assignment',
  'check',
  'destructive',
  'skill',
  'skills',
  'instructions',
  'with',
  'checklist',
  'outputs',
  'evidence',
  'retry',
  'timeout',
  'on_failure',
  'join',
  'fail_fast',
  'foreach',
  'loop',
  'switch',
  'wait',
  'approval',
  'set',
  'end',
  'call',
]);

const DURATION_HINT = 'A duration such as 30s, 15m, 2h or 1d.';
const EXPRESSION_HINT = 'An expression such as ${{ inputs.risk == ’high’ }}.';

/** Field writer bound to one step. */
function useStepFields(pointer: string) {
  const api = useBuilder();
  const at = (key: YamlPath) => toPointerIn(pointer, key);
  return {
    api,
    /** Set a field; empty text and `undefined` remove it. */
    set(key: YamlPath, value: unknown, coalesce = true) {
      const cleared = value === '' || value === undefined ? undefined : value;
      api.edit(() => setStepField(api.source, pointer, key, cleared), {
        ...(coalesce ? { coalesce: `${pointer}:${key.join('.')}` } : {}),
      });
    },
    problems(key: YamlPath) {
      return api.problemsAt(at(key));
    },
  };
}

/**
 * A StringListField's values and change handler over the list at `path`,
 * edited in place (see `listEdit`): typing in one item is one undo step.
 */
export function useStringList(path: YamlPath, raw: unknown) {
  const api = useBuilder();
  const values = strings(raw);
  return {
    values,
    onChange(next: string[]) {
      const changed = next.flatMap((v, i) => (v === values[i] ? [] : [i]));
      const typing = next.length === values.length && changed.length === 1;
      api.edit(() => listEdit(api.source, path, raw, next), {
        ...(typing ? { coalesce: `list:${path.join('.')}:${changed[0]}` } : {}),
      });
    },
  };
}

function toPointerIn(pointer: string, key: YamlPath): string {
  return `${pointer}/${key.map((k) => String(k).replace(/~/g, '~0').replace(/\//g, '~1')).join('/')}`;
}

// -------------------------------------------------------------------- panel

export function StepPanel({ node, onClose }: { node: OutlineNode; onClose(): void }) {
  const api = useBuilder();
  const step = api.model.byGraphId.get(node.id);
  const headingId = useId();
  if (!step) return null;
  const raw = rawStep(api.document, step.pointer);
  const own = api.diagnostics.filter((d) => {
    if (d.path === step.pointer) return true;
    if (!d.path.startsWith(`${step.pointer}/`)) return false;
    const key = d.path.slice(step.pointer.length + 1).split('/')[0] ?? '';
    // A nested step's problem belongs to that step's panel.
    if (['steps', 'parallel', 'catch', 'finally'].includes(key)) return false;
    return !FORM_KEYS.has(key);
  });
  return (
    <aside
      aria-labelledby={headingId}
      data-slot="step-panel"
      className="absolute inset-y-0 right-0 z-10 flex h-full w-full flex-col overflow-hidden border-l border-border bg-surface text-fg shadow-overlay sm:w-[400px] md:static md:z-auto md:shadow-none"
    >
      <header className="flex items-start gap-2 border-b border-border px-4 py-3">
        <KindIcon node={node} className="mt-0.5 size-4 shrink-0 text-fg-muted" />
        <div className="min-w-0 flex-1">
          <p className="text-2xs font-medium uppercase tracking-wider text-fg-muted">
            {kindWord(node)}
          </p>
          <h2
            id={headingId}
            data-slot="node-details-heading"
            tabIndex={-1}
            className="rounded-sm text-md font-semibold leading-snug focus-visible:outline-2 focus-visible:outline-focus"
          >
            <TemplateText text={step.title} />
          </h2>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close step details"
          className="-mr-1 rounded-md p-1 text-fg-muted hover:bg-hover hover:text-fg"
        >
          <X className="size-4" aria-hidden="true" />
        </button>
      </header>
      <div className="flex-1 overflow-y-auto px-4 py-3">
        {own.length > 0 ? (
          <div
            role="group"
            aria-label="Problems in this step"
            className="mb-3 grid gap-1 rounded-md border border-verdict-fail-border bg-verdict-fail-bg p-2.5 text-sm text-verdict-fail-fg"
          >
            {own.map((d, i) => (
              <p key={i} className="flex gap-1.5">
                <AlertCircle className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
                <span>{d.message}</span>
              </p>
            ))}
          </div>
        ) : null}
        <div className="grid gap-4" key={step.pointer}>
          <IdentityFields pointer={step.pointer} raw={raw} kind={step.kind} />
          <KindFields pointer={step.pointer} raw={raw} kind={step.kind} />
          <BehaviourFields pointer={step.pointer} raw={raw} kind={step.kind} />
        </div>
      </div>
    </aside>
  );
}

// --------------------------------------------------------------- identity

function IdentityFields({ pointer, raw, kind }: { pointer: string; raw: Raw; kind: string }) {
  const { api, problems } = useStepFields(pointer);
  const id = str(raw.id);
  const [idDraft, setIdDraft] = useState(id);
  // The draft follows the YAML when the ID changes elsewhere (undo, the YAML tab).
  const [shownId, setShownId] = useState(id);
  if (shownId !== id) {
    setShownId(id);
    setIdDraft(id);
  }
  const idError = idDraft === id ? null : idProblem(api.model, pointer, idDraft);
  const fresh = api.freshIds.has(id);

  function commitId() {
    if (idDraft === id) return;
    if (idProblem(api.model, pointer, idDraft)) return;
    api.forgetFresh(id);
    api.edit(() => setStepField(api.source, pointer, ['id'], idDraft), {
      announce: `Step ID is now ${idDraft}.`,
    });
  }

  function onTitle(title: string) {
    api.edit(
      () => {
        let result = setStepField(api.source, pointer, ['title'], title === '' ? undefined : title);
        if (fresh && title.trim() !== '') {
          const taken = new Set([...api.model.ids].filter((other) => other !== id));
          const next = uniqueId(idFromTitle(title), taken);
          if (next !== id) {
            result = setStepField(result.text, pointer, ['id'], next);
            api.renameFresh(id, next);
          }
        }
        return result;
      },
      { coalesce: `${pointer}:title` },
    );
  }

  return (
    <Section title="Step">
      <TextField
        label="Title"
        value={str(raw.title)}
        placeholder={id ? (kind === 'task' ? derivedTitle(id) : id) : undefined}
        hint={
          raw.title === undefined && kind === 'task' && id
            ? `Empty: the title is “${derivedTitle(id)}”, from the ID.`
            : undefined
        }
        problems={problems(['title'])}
        onChange={onTitle}
      />
      <FieldShell
        id={`${pointer}-id`}
        label="ID"
        hint={
          fresh
            ? 'Follows the title until you change it. Other steps refer to it as steps.<ID>.'
            : 'Other steps refer to it as steps.<ID>; renaming does not update them.'
        }
        problems={[...(idError ? [idError] : []), ...problems(['id'])]}
      >
        {(describedBy, invalid) => (
          <Input
            id={`${pointer}-id`}
            value={idDraft}
            autoComplete="off"
            spellCheck={false}
            aria-describedby={describedBy}
            aria-invalid={invalid || undefined}
            className="h-9 font-mono text-sm"
            onChange={(e) => setIdDraft(e.target.value)}
            onBlur={commitId}
            onKeyDown={(e) => {
              if (e.key === 'Enter') commitId();
              if (e.key === 'Escape') setIdDraft(id);
            }}
          />
        )}
      </FieldShell>
    </Section>
  );
}

// -------------------------------------------------------------- by kind

function KindFields({ pointer, raw, kind }: { pointer: string; raw: Raw; kind: string }) {
  switch (kind) {
    case 'task':
      return <TaskFields pointer={pointer} raw={raw} />;
    case 'approval':
      return <ApprovalFields pointer={pointer} raw={raw} />;
    case 'wait':
      return <WaitFields pointer={pointer} raw={raw} />;
    case 'group':
      return <GroupFields pointer={pointer} raw={raw} />;
    case 'parallel':
      return <ParallelFields pointer={pointer} raw={raw} />;
    case 'switch':
      return <SwitchFields pointer={pointer} raw={raw} />;
    case 'foreach':
      return <ForeachFields pointer={pointer} raw={raw} />;
    case 'loop':
      return <LoopFields pointer={pointer} raw={raw} />;
    case 'call':
      return <CallFields pointer={pointer} raw={raw} />;
    case 'set':
      return <SetFields pointer={pointer} raw={raw} />;
    case 'end':
      return <EndFields pointer={pointer} raw={raw} />;
    default:
      return null;
  }
}

const ACTORS = [
  { value: 'any', label: 'Anyone' },
  { value: 'agent', label: 'Agent' },
  { value: 'human', label: 'Person' },
] as const;

function actorWord(actor: string): string {
  return ACTORS.find((a) => a.value === actor)?.label ?? actor;
}

function recipientSuggestions(api: BuilderApi): string[] {
  return [
    ...api.directory.teams.map((t) => `team:${t}`),
    ...api.directory.users.map((u) => `user:${u}`),
  ];
}

function TaskFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api, set, problems } = useStepFields(pointer);
  const checklist = useStringList([...toPath(pointer), 'checklist'], raw.checklist);
  const defaultActor = api.model.defaultActor;
  const written = typeof raw.actor === 'string' ? raw.actor : undefined;
  const actor = written ?? defaultActor;
  const instructions = raw.instructions;
  const instructionsObject = instructions && typeof instructions === 'object';
  const assignee = str(raw.assignee);
  const strict = raw.assignment === 'strict';
  const suggestions = recipientSuggestions(api);
  const listId = useId();

  return (
    <>
      <Section title="What to do">
        {instructionsObject ? (
          <>
            {(['default', 'human', 'agent'] as const).map((audience) => (
              <TextAreaField
                key={audience}
                label={`Instructions for ${audience === 'default' ? 'everyone' : audience === 'human' ? 'people' : 'agents'}`}
                value={str(obj(instructions)[audience])}
                problems={problems(['instructions', audience])}
                onChange={(v) => set(['instructions', audience], v)}
              />
            ))}
          </>
        ) : (
          <TextAreaField
            label="Instructions"
            value={str(instructions)}
            rows={6}
            hint="Markdown. ${{ … }} expressions are filled in when the step runs."
            problems={problems(['instructions'])}
            onChange={(v) => set(['instructions'], v)}
          />
        )}
      </Section>
      <Section title="Who does it">
        <SegmentedField
          label="Who does it"
          value={actor}
          options={ACTORS}
          hint={
            written === undefined && defaultActor !== 'any'
              ? `From the runbook’s defaults (${actorWord(defaultActor)}).`
              : actor === 'agent'
                ? 'People can still take over an agent’s step while it is ready.'
                : actor === 'any'
                  ? 'A person or an agent may claim it.'
                  : 'Only people claim it; agents never do.'
          }
          problems={problems(['actor'])}
          onChange={(value) => {
            // "Anyone" is the default: leave it unwritten unless the runbook says otherwise.
            set(
              ['actor'],
              value === defaultActor && written === undefined ? undefined : value,
              false,
            );
          }}
        />
        <FieldShell
          id={`${pointer}-assignee`}
          label="Assignee"
          hint={
            strict
              ? 'Strict: only this person or team may claim and complete it.'
              : 'Guidance: routes the work to them (Inbox, notifications); others may still do it. A user, a team or an expression.'
          }
          problems={problems(['assignee'])}
        >
          {(describedBy, invalid) => (
            <>
              <Input
                id={`${pointer}-assignee`}
                value={assignee}
                list={listId}
                autoComplete="off"
                placeholder="team:oncall"
                aria-describedby={describedBy}
                aria-invalid={invalid || undefined}
                className="h-9 font-mono text-sm"
                onChange={(e) => {
                  const next = e.target.value;
                  api.edit(
                    () => {
                      const result = setStepField(
                        api.source,
                        pointer,
                        ['assignee'],
                        next || undefined,
                      );
                      // Strict needs an assignee: clearing one clears strict too.
                      return !next && strict
                        ? setStepField(result.text, pointer, ['assignment'], undefined)
                        : result;
                    },
                    { coalesce: `${pointer}:assignee` },
                  );
                }}
              />
              <datalist id={listId}>
                {suggestions.map((s) => (
                  <option key={s} value={s} />
                ))}
              </datalist>
            </>
          )}
        </FieldShell>
        <CheckField
          label="Strict"
          checked={strict}
          disabled={!assignee}
          hint={assignee ? 'Only the assignee may claim it.' : 'Needs an assignee.'}
          problems={problems(['assignment'])}
          onChange={(on) => set(['assignment'], on ? 'strict' : undefined, false)}
        />
      </Section>
      <Section title="Result">
        <CheckField
          label="Check: ends with a verdict"
          checked={raw.check === true}
          hint="Pass, warn or fail, reported by whoever does it."
          problems={problems(['check'])}
          onChange={(on) => set(['check'], on ? true : undefined, false)}
        />
        <DeclarationsEditor
          title="Outputs"
          mapPath={[...toPath(pointer), 'outputs']}
          value={obj(raw.outputs)}
          addLabel="Add an output"
          problemsAt={(name) => api.problemsAt(toPointerIn(pointer, ['outputs', name]))}
        />
        <EvidenceFields pointer={pointer} raw={raw} />
        <StringListField
          label="Checklist"
          {...checklist}
          itemLabel={(n) => `Checklist item ${n}`}
          addLabel="Add an item"
          problems={problems(['checklist'])}
        />
      </Section>
    </>
  );
}

const EVIDENCE = ['link', 'log', 'file', 'screenshot'] as const;

function EvidenceFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { set, problems } = useStepFields(pointer);
  const evidence = obj(raw.evidence);
  return (
    <fieldset className="grid gap-1.5">
      <legend className="mb-1 text-sm font-medium text-fg">Evidence</legend>
      {(['required', 'accepted'] as const).map((which) => {
        const chosen = strings(evidence[which]);
        return (
          <div key={which} className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <span className="w-16 text-xs text-fg-muted">
              {which === 'required' ? 'Required' : 'Accepted'}
            </span>
            {EVIDENCE.map((kind) => (
              <label key={kind} className="flex items-center gap-1 text-sm">
                <input
                  type="checkbox"
                  checked={chosen.includes(kind)}
                  className="accent-[var(--color-accent)]"
                  aria-label={`${which === 'required' ? 'Required' : 'Accepted'} evidence: ${kind}`}
                  onChange={(e) => {
                    const next = e.target.checked
                      ? [...chosen, kind]
                      : chosen.filter((k) => k !== kind);
                    set(['evidence', which], next.length ? next : undefined, false);
                  }}
                />
                {kind}
              </label>
            ))}
          </div>
        );
      })}
      {problems(['evidence']).map((m, i) => (
        <p key={i} className="text-xs text-danger-text">
          {m}
        </p>
      ))}
    </fieldset>
  );
}

function ApprovalFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api, set, problems } = useStepFields(pointer);
  const approval = obj(raw.approval);
  const options = Array.isArray(approval.options) ? approval.options : [];
  const optionIds = options.map((o) => (typeof o === 'string' ? o : str(obj(o).id)));
  const approvers = obj(approval.approvers);
  const teams = useStringList(
    [...toPath(pointer), 'approval', 'approvers', 'teams'],
    approvers.teams,
  );
  const users = useStringList(
    [...toPath(pointer), 'approval', 'approvers', 'users'],
    approvers.users,
  );
  const escalateTo = useStringList(
    [...toPath(pointer), 'approval', 'escalate_to'],
    approval.escalate_to,
  );
  const onTimeout = approval.on_timeout;
  const decide = str(obj(onTimeout).decide);
  const timeoutMode = onTimeout === 'escalate' ? 'escalate' : decide ? 'decide' : 'none';

  return (
    <>
      <Section title="Decision">
        <TextAreaField
          label="Prompt"
          value={str(approval.prompt)}
          rows={3}
          problems={problems(['approval', 'prompt'])}
          onChange={(v) => set(['approval', 'prompt'], v)}
        />
        <OptionsEditor pointer={pointer} options={options} />
        <StringListField
          label="Approving teams"
          {...teams}
          itemLabel={(n) => `Approving team ${n}`}
          addLabel="Add a team"
          mono
          suggestions={api.directory.teams}
          problems={[
            ...problems(['approval', 'approvers', 'teams']),
            ...problems(['approval', 'approvers']).filter(
              (m) =>
                !problems(['approval', 'approvers', 'teams']).includes(m) &&
                !problems(['approval', 'approvers', 'users']).includes(m),
            ),
          ]}
        />
        <StringListField
          label="Approving people"
          {...users}
          itemLabel={(n) => `Approving person ${n}`}
          addLabel="Add a person"
          suggestions={api.directory.users}
          problems={problems(['approval', 'approvers', 'users'])}
        />
        <NumberField
          label="Approvals needed"
          value={num(approval.min)}
          min={1}
          placeholder="1"
          problems={problems(['approval', 'min'])}
          onChange={(v) => set(['approval', 'min'], v)}
        />
        <CheckField
          label="Agents may decide too"
          checked={approval.allow_agents === true}
          problems={problems(['approval', 'allow_agents'])}
          onChange={(on) => set(['approval', 'allow_agents'], on ? true : undefined, false)}
        />
      </Section>
      <Section title="Timeouts and escalation">
        <TextField
          label="Timeout"
          value={str(approval.timeout)}
          mono
          hint={DURATION_HINT}
          problems={problems(['approval', 'timeout'])}
          onChange={(v) => set(['approval', 'timeout'], v)}
        />
        <SelectField
          label="When it times out"
          value={timeoutMode}
          options={[
            { value: 'none', label: 'Expire (the default)' },
            { value: 'escalate', label: 'Escalate' },
            { value: 'decide', label: 'Decide an option' },
          ]}
          problems={problems(['approval', 'on_timeout'])}
          onChange={(mode) =>
            set(
              ['approval', 'on_timeout'],
              mode === 'escalate'
                ? 'escalate'
                : mode === 'decide'
                  ? { decide: optionIds[optionIds.length - 1] ?? '' }
                  : undefined,
              false,
            )
          }
        />
        {timeoutMode === 'decide' ? (
          <SelectField
            label="Option to decide"
            value={decide}
            options={optionIds.map((id) => ({ value: id, label: id }))}
            onChange={(v) => set(['approval', 'on_timeout', 'decide'], v, false)}
          />
        ) : null}
        <StringListField
          label="Escalate to"
          {...escalateTo}
          itemLabel={(n) => `Escalation recipient ${n}`}
          addLabel="Add a recipient"
          mono
          suggestions={recipientSuggestions(api)}
          problems={problems(['approval', 'escalate_to'])}
        />
        <TextField
          label="Escalation timeout"
          value={str(approval.escalation_timeout)}
          mono
          hint={DURATION_HINT}
          problems={problems(['approval', 'escalation_timeout'])}
          onChange={(v) => set(['approval', 'escalation_timeout'], v)}
        />
        <SelectField
          label="When it expires"
          value={str(approval.on_expired)}
          options={[
            { value: '', label: 'Pause the run (the default)' },
            { value: 'pause', label: 'Pause the run' },
            { value: 'fail', label: 'Fail the run' },
          ]}
          problems={problems(['approval', 'on_expired'])}
          onChange={(v) => set(['approval', 'on_expired'], v || undefined, false)}
        />
      </Section>
    </>
  );
}

function OptionsEditor({ pointer, options }: { pointer: string; options: unknown[] }) {
  const { set, problems } = useStepFields(pointer);
  const rows = options.map((o) =>
    typeof o === 'string' ? { id: o, title: '' } : { id: str(obj(o).id), title: str(obj(o).title) },
  );
  function write(next: { id: string; title: string }[]) {
    set(
      ['approval', 'options'],
      next.map((row) => (row.title ? { id: row.id, title: row.title } : row.id)),
      false,
    );
  }
  return (
    <fieldset className="grid gap-1.5">
      <legend className="mb-1 text-sm font-medium text-fg">Options</legend>
      <ul className="grid gap-1.5">
        {rows.map((row, i) => (
          <li key={i} className="flex items-center gap-1.5">
            <Input
              aria-label={`Option ${i + 1} ID`}
              value={row.id}
              className="h-8 font-mono text-sm"
              onChange={(e) =>
                write(rows.map((r, j) => (j === i ? { ...r, id: e.target.value } : r)))
              }
            />
            <Input
              aria-label={`Option ${i + 1} title`}
              placeholder="Title (optional)"
              value={row.title}
              className="h-8 text-sm"
              onChange={(e) =>
                write(rows.map((r, j) => (j === i ? { ...r, title: e.target.value } : r)))
              }
            />
            <IconButton
              label={`Remove option ${i + 1}`}
              disabled={rows.length <= 1}
              onClick={() => write(rows.filter((_, j) => j !== i))}
            >
              <X className="size-3.5" aria-hidden="true" />
            </IconButton>
          </li>
        ))}
      </ul>
      <div>
        <SmallButton
          onClick={() => write([...rows, { id: `option_${rows.length + 1}`, title: '' }])}
        >
          <Plus className="size-3.5" aria-hidden="true" />
          Add an option
        </SmallButton>
      </div>
      {problems(['approval', 'options']).map((m, i) => (
        <p key={i} className="text-xs text-danger-text">
          {m}
        </p>
      ))}
    </fieldset>
  );
}

function WaitFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api, set, problems } = useStepFields(pointer);
  const wait = obj(raw.wait);
  const mode = 'signal' in wait ? 'signal' : 'until' in wait ? 'until' : 'for';
  function switchMode(next: string) {
    if (next === mode) return;
    const value = next === 'signal' ? 'ready' : next === 'for' ? '1h' : '${{ inputs.deadline }}';
    api.edit(() =>
      setStepField(
        setStepField(api.source, pointer, ['wait', mode], undefined).text,
        pointer,
        ['wait', next],
        value,
      ),
    );
  }
  return (
    <Section title="Wait">
      <SegmentedField
        label="Waits for"
        value={mode}
        options={[
          { value: 'signal', label: 'A signal' },
          { value: 'for', label: 'A duration' },
          { value: 'until', label: 'A time' },
        ]}
        onChange={switchMode}
      />
      {mode === 'signal' ? (
        <TextField
          label="Signal name"
          value={str(wait.signal)}
          mono
          hint="A customer, an API or a person sends it: POST …/signals/<name> or the run page’s Send button."
          problems={problems(['wait', 'signal'])}
          onChange={(v) => set(['wait', 'signal'], v)}
        />
      ) : mode === 'for' ? (
        <TextField
          label="Duration"
          value={str(wait.for)}
          mono
          hint={`${DURATION_HINT} At most 7 days.`}
          problems={problems(['wait', 'for'])}
          onChange={(v) => set(['wait', 'for'], v)}
        />
      ) : (
        <TextField
          label="Until"
          value={str(wait.until)}
          mono
          hint="A date and time (2030-01-01T09:00:00Z) or an expression; at most 7 days ahead."
          problems={problems(['wait', 'until'])}
          onChange={(v) => set(['wait', 'until'], v)}
        />
      )}
      <TextField
        label="Give up after"
        value={str(wait.timeout)}
        mono
        hint={DURATION_HINT}
        problems={problems(['wait', 'timeout'])}
        onChange={(v) => set(['wait', 'timeout'], v)}
      />
    </Section>
  );
}

function GroupFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api, set } = useStepFields(pointer);
  const lanes = [
    { key: 'catch', label: 'On-failure steps', hint: 'Run when a step in the group fails.' },
    {
      key: 'finally',
      label: 'Finally steps',
      hint: 'Run at the end of the group, whatever happened.',
    },
  ] as const;
  return (
    <Section title="Group">
      {lanes.map((lane) => {
        const has = lane.key in raw;
        return (
          <div key={lane.key} className="flex items-center justify-between gap-2">
            <div>
              <p className="text-sm font-medium">{lane.label}</p>
              <p className="text-xs text-fg-muted">
                {has
                  ? `${Array.isArray(raw[lane.key]) ? (raw[lane.key] as unknown[]).length : 0} steps.`
                  : lane.hint}
              </p>
            </div>
            {has ? (
              <SmallButton tone="danger" onClick={() => set([lane.key], undefined, false)}>
                <Trash2 className="size-3.5" aria-hidden="true" />
                Remove
              </SmallButton>
            ) : (
              <SmallButton
                onClick={() => {
                  api.edit(
                    () =>
                      insertStep(
                        api.source,
                        api.model,
                        { body: `${pointer}/${lane.key}`, index: 0 },
                        'task',
                      ),
                    { selectResult: true, announce: `Added ${lane.label.toLowerCase()}.` },
                  );
                }}
              >
                <Plus className="size-3.5" aria-hidden="true" />
                Add
              </SmallButton>
            )}
          </div>
        );
      })}
    </Section>
  );
}

function ParallelFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { set, problems } = useStepFields(pointer);
  const join = raw.join;
  const atLeast = num(obj(join).at_least);
  const mode = join === 'any' ? 'any' : atLeast !== undefined ? 'at_least' : 'all';
  return (
    <Section title="Parallel">
      <SelectField
        label="Continues when"
        value={mode}
        options={[
          { value: 'all', label: 'Every step is done (join: all)' },
          { value: 'any', label: 'The first step succeeds (join: any)' },
          { value: 'at_least', label: 'Some steps succeed (join: at least)' },
        ]}
        hint={
          mode === 'all'
            ? 'The steps run in any order, at the same time.'
            : 'The others are cancelled once enough have succeeded.'
        }
        problems={problems(['join'])}
        onChange={(next) =>
          set(
            ['join'],
            next === 'all' ? undefined : next === 'any' ? 'any' : { at_least: 2 },
            false,
          )
        }
      />
      {mode === 'at_least' ? (
        <NumberField
          label="How many must succeed"
          value={atLeast}
          min={1}
          problems={problems(['join', 'at_least'])}
          onChange={(v) => set(['join', 'at_least'], v ?? 1)}
        />
      ) : null}
      {mode === 'all' ? (
        <CheckField
          label="Stop the others on the first failure"
          checked={raw.fail_fast === true}
          problems={problems(['fail_fast'])}
          onChange={(on) => set(['fail_fast'], on ? true : undefined, false)}
        />
      ) : null}
    </Section>
  );
}

function SwitchFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api, set, problems } = useStepFields(pointer);
  const sw = obj(raw.switch);
  const cases = Array.isArray(sw.cases) ? sw.cases : [];
  const casesPath = [...toPath(pointer), 'switch', 'cases'];
  return (
    <Section title="Branches">
      <p className="text-xs text-fg-muted">
        The first case whose condition holds runs; otherwise the default does.
      </p>
      <ol className="grid gap-3">
        {cases.map((c, i) => {
          const steps = Array.isArray(obj(c).steps) ? (obj(c).steps as unknown[]).length : 0;
          return (
            <li key={i} className="grid gap-1.5 rounded-md border border-border p-2.5">
              <TextField
                label={`Case ${i + 1}: when`}
                value={str(obj(c).when)}
                mono
                hint={`${steps} ${steps === 1 ? 'step' : 'steps'}. ${EXPRESSION_HINT}`}
                problems={problems(['switch', 'cases', i, 'when'])}
                onChange={(v) => set(['switch', 'cases', i, 'when'], v)}
              />
              <div>
                <SmallButton
                  tone="danger"
                  onClick={() =>
                    api.edit(() => removeItem(api.source, casesPath, i), {
                      announce: `Removed case ${i + 1} and its steps.`,
                    })
                  }
                >
                  <Trash2 className="size-3.5" aria-hidden="true" />
                  Remove case {i + 1}
                </SmallButton>
              </div>
            </li>
          );
        })}
      </ol>
      <div>
        <SmallButton
          onClick={() => {
            const taken = new Set(api.model.ids);
            const first = uniqueId('task', taken);
            api.edit(
              () =>
                insertItem(api.source, casesPath, cases.length, {
                  when: '${{ true }}',
                  steps: [{ id: first }],
                }),
              { announce: `Added case ${cases.length + 1}.` },
            );
          }}
        >
          <Plus className="size-3.5" aria-hidden="true" />
          Add a case
        </SmallButton>
      </div>
      {problems(['switch', 'cases']).length > 0 && cases.length === 0 ? (
        <p className="text-xs text-danger-text">{problems(['switch', 'cases'])[0]}</p>
      ) : null}
      <CheckField
        label="Default branch"
        checked={'default' in sw}
        hint="Runs when no case holds. Without one, the run goes on to the next step."
        onChange={(on) => set(['switch', 'default'], on ? { steps: [] } : undefined, false)}
      />
    </Section>
  );
}

function ForeachFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { set, problems } = useStepFields(pointer);
  const f = obj(raw.foreach);
  return (
    <Section title="For each">
      <TextField
        label="Items"
        value={str(f.items)}
        mono
        hint="A list: ${{ inputs.regions }}."
        problems={problems(['foreach', 'items'])}
        onChange={(v) => set(['foreach', 'items'], v)}
      />
      <TextField
        label="Each item is called"
        value={str(f.as)}
        mono
        placeholder="item"
        problems={problems(['foreach', 'as'])}
        onChange={(v) => set(['foreach', 'as'], v)}
      />
      <TextField
        label="Key"
        value={str(f.key)}
        mono
        hint="Names each iteration (${{ item.name }}); empty uses the item’s position."
        problems={problems(['foreach', 'key'])}
        onChange={(v) => set(['foreach', 'key'], v)}
      />
      <NumberField
        label="At a time"
        value={num(f.concurrency)}
        min={1}
        placeholder="1"
        problems={problems(['foreach', 'concurrency'])}
        onChange={(v) => set(['foreach', 'concurrency'], v)}
      />
      <NumberField
        label="Most items"
        value={num(f.max_items)}
        min={1}
        max={100}
        hint="Required, 1 to 100: a longer list fails the step."
        problems={problems(['foreach', 'max_items'])}
        onChange={(v) => set(['foreach', 'max_items'], v)}
      />
    </Section>
  );
}

function LoopFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api, set, problems } = useStepFields(pointer);
  const l = obj(raw.loop);
  const mode = 'while' in l ? 'while' : 'until';
  return (
    <Section title="Loop">
      <SegmentedField
        label="Repeats"
        value={mode}
        options={[
          { value: 'until', label: 'Until' },
          { value: 'while', label: 'While' },
        ]}
        hint="Checked after each pass, so the steps run at least once."
        onChange={(next) => {
          if (next === mode) return;
          const condition = str(l[mode]);
          api.edit(() =>
            setStepField(
              setStepField(api.source, pointer, ['loop', mode], undefined).text,
              pointer,
              ['loop', next],
              condition || '${{ true }}',
            ),
          );
        }}
      />
      <TextField
        label="Condition"
        value={str(l[mode])}
        mono
        hint={`${mode === 'until' ? 'Stops once it holds' : 'Stops once it no longer holds'}. ${EXPRESSION_HINT}`}
        problems={problems(['loop', mode])}
        onChange={(v) => set(['loop', mode], v)}
      />
      <NumberField
        label="At most"
        value={num(l.max_iterations)}
        min={1}
        max={50}
        hint="Required, 1 to 50 passes."
        problems={problems(['loop', 'max_iterations'])}
        onChange={(v) => set(['loop', 'max_iterations'], v)}
      />
      <TextField
        label="Pause between passes"
        value={str(l.delay)}
        mono
        hint={DURATION_HINT}
        problems={problems(['loop', 'delay'])}
        onChange={(v) => set(['loop', 'delay'], v)}
      />
    </Section>
  );
}

/** The inputs a published runbook declares (from its latest version's YAML). */
function useCalledInputs(runbookId: string | undefined, version: number | undefined) {
  const source = useVersionSource(runbookId ?? '', runbookId && version ? String(version) : null);
  return useMemo(() => {
    const text = (source.data as { source?: string } | undefined)?.source;
    if (!text) return null;
    const inputs = obj(obj(obj(parseDocument(text).toJS()).spec).inputs);
    return Object.entries(inputs).map(([name, decl]) => ({
      name,
      required: obj(decl).required === true,
    }));
  }, [source.data]);
}

function CallFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api, set, problems } = useStepFields(pointer);
  const call = obj(raw.call);
  const slug = str(call.runbook);
  const listId = useId();
  const target = api.callTargets.find((t) => t.slug === slug);
  const inputs = useCalledInputs(target?.runbookId, num(call.version) ?? target?.latestVersion);
  const given = obj(call.with);
  const missing = (inputs ?? []).filter((i) => !(i.name in given));
  const href = slug ? api.callHref?.(slug) : undefined;
  return (
    <Section title="Call">
      <FieldShell
        id={`${pointer}-call`}
        label="Runbook"
        hint={
          api.callTargets.length > 0
            ? 'A published runbook of this project. Calls are checked by the server when you save.'
            : 'No published runbooks in this project yet; type a runbook’s ID.'
        }
        problems={problems(['call', 'runbook'])}
      >
        {(describedBy, invalid) => (
          <>
            <Input
              id={`${pointer}-call`}
              value={slug}
              list={listId}
              autoComplete="off"
              spellCheck={false}
              placeholder="post-deploy-checks"
              aria-describedby={describedBy}
              aria-invalid={invalid || undefined}
              className="h-9 font-mono text-sm"
              onChange={(e) => set(['call', 'runbook'], e.target.value)}
            />
            <datalist id={listId}>
              {api.callTargets.map((t) => (
                <option key={t.slug} value={t.slug}>
                  {t.title}
                </option>
              ))}
            </datalist>
          </>
        )}
      </FieldShell>
      {href && target ? (
        <a
          href={href}
          target="_blank"
          rel="noreferrer"
          className="inline-flex items-center gap-1 text-sm font-medium text-accent hover:underline"
        >
          {target.title} (v{target.latestVersion})
          <ExternalLink className="size-3.5" aria-hidden="true" />
          <span className="sr-only">(opens in a new tab)</span>
        </a>
      ) : null}
      <NumberField
        label="Version"
        value={num(call.version)}
        min={1}
        hint="Empty: the latest version, pinned when this runbook is published."
        problems={problems(['call', 'version'])}
        onChange={(v) => set(['call', 'version'], v)}
      />
      <KeyValueEditor
        title="Inputs to pass (with)"
        mapPath={[...toPath(pointer), 'call', 'with']}
        value={given}
        addLabel="Add an input"
        valueHint="A value or an expression."
        problemsAt={(name) => api.problemsAt(toPointerIn(pointer, ['call', 'with', name]))}
      />
      {missing.length > 0 ? (
        <div className="grid gap-1 rounded-md bg-surface-raised p-2.5 text-sm">
          <p>
            {target?.title ?? slug} also takes:{' '}
            {missing.map((i) => `${i.name}${i.required ? ' (required)' : ''}`).join(', ')}.
          </p>
          <div>
            <SmallButton
              onClick={() => {
                api.edit(() => {
                  let result = { text: api.source, minimal: true };
                  for (const input of missing) {
                    const next = setStepField(
                      result.text,
                      pointer,
                      ['call', 'with', input.name],
                      '',
                    );
                    result = { text: next.text, minimal: result.minimal && next.minimal };
                  }
                  return result;
                });
              }}
            >
              Add them
            </SmallButton>
          </div>
        </div>
      ) : null}
    </Section>
  );
}

function SetFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { api } = useStepFields(pointer);
  const s = obj(raw.set);
  return (
    <Section title="Set">
      <KeyValueEditor
        title="Fields"
        mapPath={[...toPath(pointer), 'set', 'fields']}
        value={obj(s.fields)}
        addLabel="Set a field"
        valueHint="Fields are declared in the runbook settings."
        problemsAt={(name) => api.problemsAt(toPointerIn(pointer, ['set', 'fields', name]))}
      />
      <KeyValueEditor
        title="Variables"
        mapPath={[...toPath(pointer), 'set', 'vars']}
        value={obj(s.vars)}
        addLabel="Set a variable"
        valueHint="Variables are declared in the runbook’s spec.vars."
        problemsAt={(name) => api.problemsAt(toPointerIn(pointer, ['set', 'vars', name]))}
      />
    </Section>
  );
}

function EndFields({ pointer, raw }: { pointer: string; raw: Raw }) {
  const { set, problems } = useStepFields(pointer);
  const end = obj(raw.end);
  return (
    <Section title="End">
      <SelectField
        label="The run ends as"
        value={str(end.status) || 'succeeded'}
        options={[
          { value: 'succeeded', label: 'Succeeded' },
          { value: 'failed', label: 'Failed' },
          { value: 'cancelled', label: 'Cancelled' },
        ]}
        problems={problems(['end', 'status'])}
        onChange={(v) => set(['end', 'status'], v, false)}
      />
      <TextField
        label="Message"
        value={str(end.message)}
        problems={problems(['end', 'message'])}
        onChange={(v) => set(['end', 'message'], v)}
      />
    </Section>
  );
}

// ---------------------------------------------------------- behaviour

function BehaviourFields({ pointer, raw, kind }: { pointer: string; raw: Raw; kind: string }) {
  const { api, set, problems } = useStepFields(pointer);
  const task = kind === 'task';
  const retry = obj(raw.retry);
  return (
    <Section title="Conditions, time limits and failure" folded>
      <TextField
        label="Runs only if"
        value={str(raw.if)}
        mono
        hint={`Skipped when false. ${EXPRESSION_HINT}`}
        problems={problems(['if'])}
        onChange={(v) => set(['if'], v)}
      />
      <TextField
        label="Timeout"
        value={str(raw.timeout)}
        mono
        hint={DURATION_HINT}
        problems={problems(['timeout'])}
        onChange={(v) => set(['timeout'], v)}
      />
      <SelectField
        label="On failure"
        value={str(raw.on_failure)}
        options={[
          { value: '', label: 'Fail the run (the default)' },
          { value: 'continue', label: 'Continue with the next step' },
          { value: 'handoff', label: 'Hand off to a person' },
        ]}
        problems={problems(['on_failure'])}
        onChange={(v) => set(['on_failure'], v || undefined, false)}
      />
      {task ? (
        <>
          <NumberField
            label="Attempts"
            value={num(retry.max_attempts)}
            min={1}
            placeholder="1"
            hint="How many times it is tried before it fails."
            problems={problems(['retry', 'max_attempts'])}
            onChange={(v) => set(['retry', 'max_attempts'], v)}
          />
          <TextField
            label="Wait between attempts"
            value={str(retry.backoff)}
            mono
            hint={DURATION_HINT}
            problems={problems(['retry', 'backoff'])}
            onChange={(v) => set(['retry', 'backoff'], v)}
          />
          <CheckField
            label="Destructive"
            checked={raw.destructive === true}
            hint="Changes production state; approval policies treat it with care."
            problems={problems(['destructive'])}
            onChange={(on) => set(['destructive'], on ? true : undefined, false)}
          />
          <SkillField
            pointer={pointer}
            rawSkill={raw.skill}
            rawSkills={raw.skills}
            suggestions={api.directory.skills}
          />
        </>
      ) : null}
    </Section>
  );
}

function SkillField({
  pointer,
  rawSkill,
  rawSkills,
  suggestions,
}: {
  pointer: string;
  rawSkill: unknown;
  rawSkills: unknown;
  suggestions: string[];
}) {
  const api = useBuilder();
  const { problems } = useStepFields(pointer);
  const listId = useId();
  const [draft, setDraft] = useState('');

  const selectedSkills: string[] = useMemo(() => {
    const list: string[] = [];
    if (Array.isArray(rawSkills)) {
      for (const s of rawSkills) {
        if (typeof s === 'string' && s.trim() !== '' && !list.includes(s.trim())) {
          list.push(s.trim());
        }
      }
    } else if (typeof rawSkill === 'string' && rawSkill.trim() !== '') {
      list.push(rawSkill.trim());
    }
    return list;
  }, [rawSkill, rawSkills]);

  const updateSkills = (next: string[]) => {
    const deduped: string[] = [];
    for (const s of next) {
      const trimmed = s.trim();
      if (trimmed && !deduped.includes(trimmed)) {
        deduped.push(trimmed);
      }
    }
    api.edit(() => {
      if (deduped.length === 0) {
        const r1 = setStepField(api.source, pointer, ['skill'], undefined);
        return setStepField(r1.text, pointer, ['skills'], undefined);
      } else if (deduped.length === 1) {
        const r1 = setStepField(api.source, pointer, ['skills'], undefined);
        return setStepField(r1.text, pointer, ['skill'], deduped[0]);
      } else {
        const r1 = setStepField(api.source, pointer, ['skill'], undefined);
        return setStepField(r1.text, pointer, ['skills'], deduped);
      }
    });
  };

  const addSkill = (val: string) => {
    const trimmed = val.trim();
    if (!trimmed) return;
    if (!selectedSkills.includes(trimmed)) {
      updateSkills([...selectedSkills, trimmed]);
    }
    setDraft('');
  };

  const allProblems = [...problems(['skill']), ...problems(['skills'])];

  return (
    <FieldShell
      id={`${pointer}-skill`}
      label={selectedSkills.length > 1 ? 'Skills' : 'Skill'}
      hint="Skills from the project’s library the agent loads for this step."
      problems={allProblems}
    >
      {(describedBy, invalid) => (
        <div className="grid gap-2">
          {selectedSkills.length > 0 && (
            <div className="flex flex-wrap gap-1.5" data-testid="selected-skills">
              {selectedSkills.map((s) => (
                <span
                  key={s}
                  className="inline-flex items-center gap-1 px-2 py-0.5 rounded bg-muted font-mono text-xs text-fg"
                >
                  <span>{s}</span>
                  <button
                    type="button"
                    aria-label={`Remove skill ${s}`}
                    className="text-fg-muted hover:text-fg focus:outline-none"
                    onClick={() => updateSkills(selectedSkills.filter((item) => item !== s))}
                  >
                    <X className="size-3" aria-hidden="true" />
                  </button>
                </span>
              ))}
            </div>
          )}
          <div className="flex items-center gap-1.5">
            <Input
              id={`${pointer}-skill`}
              value={draft}
              list={listId}
              placeholder="Add a skill…"
              autoComplete="off"
              aria-describedby={describedBy}
              aria-invalid={invalid || undefined}
              className="h-9 font-mono text-sm"
              onChange={(e) => {
                const val = e.target.value;
                if (suggestions.includes(val)) {
                  addSkill(val);
                } else {
                  setDraft(val);
                }
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  addSkill(draft);
                }
              }}
            />
            <datalist id={listId}>
              {suggestions
                .filter((s) => !selectedSkills.includes(s))
                .map((s) => (
                  <option key={s} value={s} />
                ))}
            </datalist>
            <SmallButton
              disabled={!draft.trim()}
              onClick={() => addSkill(draft)}
            >
              <Plus className="size-3.5" aria-hidden="true" />
              Add
            </SmallButton>
          </div>
        </div>
      )}
    </FieldShell>
  );
}

// ------------------------------------------------- maps of declarations

export const DECLARATION_TYPES = [
  'string',
  'text',
  'number',
  'integer',
  'boolean',
  'enum',
  'multi_enum',
  'date',
  'datetime',
  'duration',
  'url',
  'email',
  'user',
  'list',
  'object',
] as const;

/** A name that is not taken yet in `map`. */
function freshName(base: string, map: Raw): string {
  return uniqueId(base, new Set(Object.keys(map)));
}

/** Rename a key on blur or Enter; the field keeps what is typed until then. */
function KeyInput({
  value,
  label,
  taken,
  onRename,
}: {
  value: string;
  label: string;
  taken: Raw;
  onRename(next: string): void;
}) {
  const [draft, setDraft] = useState(value);
  const [shown, setShown] = useState(value);
  if (shown !== value) {
    setShown(value);
    setDraft(value);
  }
  const problem =
    draft === value
      ? null
      : draft.trim() === ''
        ? 'A name is needed.'
        : draft in taken
          ? 'That name is taken.'
          : null;
  const commit = () => {
    if (draft !== value && !problem) onRename(draft.trim());
    else setDraft(value);
  };
  return (
    <span className="grid min-w-0 flex-1 gap-0.5">
      <Input
        value={draft}
        aria-label={label}
        aria-invalid={problem ? true : undefined}
        autoComplete="off"
        spellCheck={false}
        className="h-8 font-mono text-sm"
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit();
          if (e.key === 'Escape') setDraft(value);
        }}
      />
      {problem ? <span className="text-xs text-danger-text">{problem}</span> : null}
    </span>
  );
}

/** Outputs, inputs and fields: a name, a type, required (and a default). */
export function DeclarationsEditor({
  title,
  mapPath,
  value,
  addLabel,
  problemsAt,
  withDefault = false,
  base = 'output',
}: {
  title: string;
  mapPath: YamlPath;
  value: Raw;
  addLabel: string;
  problemsAt(name: string): string[];
  withDefault?: boolean;
  base?: string;
}) {
  const api = useBuilder();
  const entries = Object.entries(value).filter(([name]) => !name.startsWith('x-'));
  const write = (path: YamlPath, v: unknown, coalesce?: string) =>
    api.edit(
      () =>
        v === undefined || v === ''
          ? deleteIn(api.source, path, { prune: true })
          : setIn(api.source, path, v),
      coalesce ? { coalesce } : {},
    );
  return (
    <fieldset className="grid gap-1.5">
      <legend className="mb-1 text-sm font-medium text-fg">{title}</legend>
      {entries.length > 0 ? (
        <ul className="grid gap-2">
          {entries.map(([name, decl]) => {
            const d = obj(decl);
            const type = str(d.type) || 'string';
            const problems = problemsAt(name);
            return (
              <li key={name} className="grid gap-1.5 rounded-md border border-border p-2">
                <div className="flex items-start gap-1.5">
                  <KeyInput
                    value={name}
                    label={`${title}: name of ${name}`}
                    taken={value}
                    onRename={(next) => api.edit(() => renameKey(api.source, mapPath, name, next))}
                  />
                  <select
                    aria-label={`${title}: type of ${name}`}
                    value={type}
                    className="h-8 rounded-md border border-border-input bg-surface px-2 text-sm text-fg focus-visible:outline-2 focus-visible:outline-accent"
                    onChange={(e) => {
                      const type = e.target.value;
                      api.edit(() => {
                        let result = setIn(api.source, [...mapPath, name, 'type'], type);
                        const needsOptions = type === 'enum' || type === 'multi_enum';
                        if (needsOptions && !Array.isArray(d.options))
                          result = setIn(
                            result.text,
                            [...mapPath, name, 'options'],
                            ['one', 'two'],
                          );
                        if (type === 'list' && !d.items)
                          result = setIn(result.text, [...mapPath, name, 'items'], {
                            type: 'string',
                          });
                        return result;
                      });
                    }}
                  >
                    {DECLARATION_TYPES.map((t) => (
                      <option key={t} value={t}>
                        {t}
                      </option>
                    ))}
                  </select>
                  <IconButton
                    label={`Remove ${name}`}
                    onClick={() => api.edit(() => deleteIn(api.source, [...mapPath, name]))}
                  >
                    <X className="size-3.5" aria-hidden="true" />
                  </IconButton>
                </div>
                <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
                  <label className="flex items-center gap-1.5 text-sm">
                    <input
                      type="checkbox"
                      checked={d.required === true}
                      className="accent-[var(--color-accent)]"
                      onChange={(e) =>
                        write([...mapPath, name, 'required'], e.target.checked ? true : undefined)
                      }
                    />
                    Required
                  </label>
                  {type === 'enum' || type === 'multi_enum' ? (
                    <span className="flex min-w-0 flex-1 items-center gap-1.5 text-sm">
                      <span className="shrink-0 text-fg-muted" aria-hidden="true">
                        Options
                      </span>
                      <Input
                        aria-label={`${title}: options of ${name} (comma-separated)`}
                        value={strings(d.options).join(', ')}
                        className="h-8 text-sm"
                        onChange={(e) =>
                          write(
                            [...mapPath, name, 'options'],
                            e.target.value
                              .split(',')
                              .map((o) => o.trim())
                              .filter(Boolean),
                            `${mapPath.join('.')}.${name}.options`,
                          )
                        }
                      />
                    </span>
                  ) : null}
                  {withDefault && type !== 'list' && type !== 'object' && type !== 'multi_enum' ? (
                    <span className="flex min-w-0 flex-1 items-center gap-1.5 text-sm">
                      <span className="shrink-0 text-fg-muted" aria-hidden="true">
                        Default
                      </span>
                      <Input
                        aria-label={`${title}: default of ${name}`}
                        value={d.default === undefined ? '' : str(d.default)}
                        className="h-8 text-sm"
                        onChange={(e) => {
                          const text = e.target.value;
                          const typed =
                            type === 'boolean'
                              ? text === 'true'
                                ? true
                                : text === 'false'
                                  ? false
                                  : text
                              : (type === 'number' || type === 'integer') &&
                                  text.trim() !== '' &&
                                  Number.isFinite(Number(text))
                                ? Number(text)
                                : text;
                          write(
                            [...mapPath, name, 'default'],
                            typed === '' ? undefined : typed,
                            `${mapPath.join('.')}.${name}.default`,
                          );
                        }}
                      />
                    </span>
                  ) : null}
                </div>
                {problems.map((m, i) => (
                  <p key={i} className="text-xs text-danger-text">
                    {m}
                  </p>
                ))}
              </li>
            );
          })}
        </ul>
      ) : null}
      <div>
        <SmallButton
          onClick={() =>
            api.edit(() =>
              setIn(
                api.source,
                [...mapPath, freshName(base, value)],
                { type: 'string' },
                { flow: true },
              ),
            )
          }
        >
          <Plus className="size-3.5" aria-hidden="true" />
          {addLabel}
        </SmallButton>
      </div>
    </fieldset>
  );
}

/** `with`, `set.fields`, `set.vars`: a name and a value (often an expression). */
export function KeyValueEditor({
  title,
  mapPath,
  value,
  addLabel,
  valueHint,
  problemsAt,
}: {
  title: string;
  mapPath: YamlPath;
  value: Raw;
  addLabel: string;
  valueHint?: ReactNode;
  problemsAt(name: string): string[];
}) {
  const api = useBuilder();
  const entries = Object.entries(value).filter(([name]) => !name.startsWith('x-'));
  return (
    <fieldset className="grid gap-1.5">
      <legend className="mb-1 text-sm font-medium text-fg">{title}</legend>
      {entries.length > 0 ? (
        <ul className="grid gap-1.5">
          {entries.map(([name, v]) => {
            const problems = problemsAt(name);
            return (
              <li key={name} className="grid gap-0.5">
                <div className="flex items-start gap-1.5">
                  <KeyInput
                    value={name}
                    label={`${title}: name of ${name}`}
                    taken={value}
                    onRename={(next) => api.edit(() => renameKey(api.source, mapPath, name, next))}
                  />
                  <Input
                    aria-label={`${title}: value of ${name}`}
                    value={typeof v === 'object' && v !== null ? JSON.stringify(v) : str(v)}
                    aria-invalid={problems.length > 0 || undefined}
                    className="h-8 flex-1 font-mono text-sm"
                    onChange={(e) => {
                      const next = e.target.value;
                      api.edit(() => setIn(api.source, [...mapPath, name], next), {
                        coalesce: `${mapPath.join('.')}.${name}`,
                      });
                    }}
                  />
                  <IconButton
                    label={`Remove ${name}`}
                    onClick={() =>
                      api.edit(() => deleteIn(api.source, [...mapPath, name], { prune: true }))
                    }
                  >
                    <X className="size-3.5" aria-hidden="true" />
                  </IconButton>
                </div>
                {problems.map((m, i) => (
                  <p key={i} className="text-xs text-danger-text">
                    {m}
                  </p>
                ))}
              </li>
            );
          })}
        </ul>
      ) : null}
      {valueHint ? <p className="text-xs text-fg-muted">{valueHint}</p> : null}
      <div>
        <SmallButton
          onClick={() =>
            api.edit(() => setIn(api.source, [...mapPath, freshName('name', value)], ''))
          }
        >
          <Plus className="size-3.5" aria-hidden="true" />
          {addLabel}
        </SmallButton>
      </div>
    </fieldset>
  );
}
