// UI-4: the runbook's own settings, in the side panel while no step is
// selected: title, description, tags, owners, inputs, fields, defaults,
// triggers, the run's title, timeout and concurrency, and the `finally` steps.
import { AlertCircle, Plus, Settings2 } from 'lucide-react';
import { useId } from 'react';
import { useBuilder } from './context';
import {
  NumberField,
  Section,
  SelectField,
  SmallButton,
  StringListField,
  TextAreaField,
  TextField,
} from './fields';
import { RUNBOOK_ID, stepForPath, type Raw } from './model';
import { insertStep, setRunbookField } from './ops';
import { DeclarationsEditor, useStringList } from './StepPanel';
import { TriggersEditor } from './TriggersEditor';
import type { YamlPath } from './yamlEdit';

function str(value: unknown): string {
  return typeof value === 'string' ? value : value == null ? '' : String(value);
}

function num(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function obj(value: unknown): Raw {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Raw) : {};
}

const DURATION_HINT = 'A duration such as 30m, 6h or 1d.';

export function SettingsPanel({ stepBudget }: { stepBudget: number | null | undefined }) {
  const api = useBuilder();
  const headingId = useId();
  const { metadata, spec } = api.model.raw;
  const execution = obj(spec.execution);
  const concurrency = obj(execution.concurrency);
  const defaults = obj(spec.defaults);
  const finallySteps = api.model.steps.filter((s) => s.body === '/spec/finally');
  const loose = api.diagnostics.filter((d) => !stepForPath(api.model, d.path));

  const set = (path: YamlPath, value: unknown, coalesce = true) =>
    api.edit(() => setRunbookField(api.source, path, value === '' ? undefined : value), {
      ...(coalesce ? { coalesce: `settings:${path.join('.')}` } : {}),
    });
  const at = (pointer: string) => api.problemsAt(pointer);
  const tags = useStringList(['metadata', 'tags'], metadata.tags);
  const owners = useStringList(['metadata', 'owners'], metadata.owners);

  const id = str(metadata.id);
  const idProblem =
    api.isNew && id && !RUNBOOK_ID.test(id)
      ? ['Use lowercase letters, digits and single hyphens (my-runbook).']
      : api.isNew && id === 'new'
        ? ['“new” is reserved for the new-runbook page; pick another ID.']
        : [];

  return (
    <aside
      aria-labelledby={headingId}
      data-slot="settings-panel"
      className="absolute inset-y-0 right-0 z-10 hidden h-full w-full flex-col overflow-hidden border-l border-border bg-surface text-fg sm:w-[400px] md:static md:z-auto md:flex"
    >
      <header className="flex items-start gap-2 border-b border-border px-4 py-3">
        <Settings2 className="mt-0.5 size-4 shrink-0 text-fg-muted" aria-hidden="true" />
        <div className="min-w-0 flex-1">
          <p className="text-2xs font-medium uppercase tracking-wider text-fg-muted">Runbook</p>
          <h2
            id={headingId}
            tabIndex={-1}
            className="rounded-sm text-md font-semibold leading-snug focus-visible:outline-2 focus-visible:outline-focus"
          >
            Runbook settings
          </h2>
          <p className="text-xs text-fg-muted">Select a step to edit it.</p>
        </div>
      </header>
      <div className="grid flex-1 content-start gap-4 overflow-y-auto px-4 py-3">
        {loose.length > 0 ? (
          <div
            role="group"
            aria-label="Problems outside the steps"
            className="grid gap-1 rounded-md border border-verdict-fail-border bg-verdict-fail-bg p-2.5 text-sm text-verdict-fail-fg"
          >
            {loose.map((d, i) => (
              <p key={i} className="flex gap-1.5">
                <AlertCircle className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
                <span>
                  {d.message} <span className="font-mono text-xs opacity-80">{d.path || '/'}</span>
                </span>
              </p>
            ))}
          </div>
        ) : null}
        <Section title="About">
          <TextField
            label="Title"
            value={str(metadata.title)}
            problems={at('/metadata/title')}
            onChange={(v) => set(['metadata', 'title'], v)}
          />
          {api.isNew ? (
            <TextField
              label="ID"
              value={id}
              mono
              hint="Fixed once saved. Lowercase letters, digits and hyphens."
              problems={[...idProblem, ...at('/metadata/id')]}
              onChange={(v) => set(['metadata', 'id'], v)}
            />
          ) : (
            <p className="text-sm">
              <span className="text-fg-muted">ID </span>
              <code className="font-mono">{id}</code>
              <span className="block text-xs text-fg-muted">
                An ID is fixed once the runbook exists.
              </span>
            </p>
          )}
          <TextAreaField
            label="Description"
            value={str(metadata.description)}
            rows={3}
            problems={at('/metadata/description')}
            onChange={(v) => set(['metadata', 'description'], v)}
          />
          <StringListField
            label="Tags"
            {...tags}
            itemLabel={(n) => `Tag ${n}`}
            addLabel="Add a tag"
            problems={at('/metadata/tags')}
          />
          <StringListField
            label="Owners"
            {...owners}
            itemLabel={(n) => `Owner ${n}`}
            addLabel="Add an owner"
            mono
            suggestions={[
              ...api.directory.teams.map((t) => `team:${t}`),
              ...api.directory.users.map((u) => `user:${u}`),
            ]}
            hint="team:<slug> or user:<name>. Owners get handoffs nobody else takes."
            problems={at('/metadata/owners')}
          />
        </Section>
        <Section title="Inputs">
          <DeclarationsEditor
            title="Inputs"
            mapPath={['spec', 'inputs']}
            value={obj(spec.inputs)}
            addLabel="Add an input"
            withDefault
            base="input"
            problemsAt={(name) => at(`/spec/inputs/${name}`)}
          />
          <p className="text-xs text-fg-muted">
            Asked for when a run starts; steps read them as {'${{ inputs.<name> }}'}.
          </p>
        </Section>
        <Section title="Triggers">
          <TriggersEditor />
        </Section>
        <Section title="Fields" folded>
          <DeclarationsEditor
            title="Fields"
            mapPath={['spec', 'fields']}
            value={obj(spec.fields)}
            addLabel="Add a field"
            withDefault
            base="field"
            problemsAt={(name) => at(`/spec/fields/${name}`)}
          />
          <p className="text-xs text-fg-muted">Values a run keeps and steps may set.</p>
        </Section>
        <Section title="Defaults for steps">
          <SelectField
            label="Who does a step"
            value={str(defaults.actor)}
            options={[
              { value: '', label: 'Anyone (the default)' },
              { value: 'agent', label: 'Agent' },
              { value: 'human', label: 'Person' },
              { value: 'any', label: 'Anyone' },
            ]}
            problems={at('/spec/defaults/actor')}
            onChange={(v) => set(['spec', 'defaults', 'actor'], v, false)}
          />
          <TextField
            label="Step timeout"
            value={str(defaults.timeout)}
            mono
            hint={DURATION_HINT}
            problems={at('/spec/defaults/timeout')}
            onChange={(v) => set(['spec', 'defaults', 'timeout'], v)}
          />
          <SelectField
            label="When a step fails"
            value={str(defaults.on_failure)}
            options={[
              { value: '', label: 'Fail the run (the default)' },
              { value: 'continue', label: 'Continue with the next step' },
              { value: 'handoff', label: 'Hand off to a person' },
            ]}
            problems={at('/spec/defaults/on_failure')}
            onChange={(v) => set(['spec', 'defaults', 'on_failure'], v, false)}
          />
        </Section>
        <Section title="Runs">
          <TextField
            label="Run title"
            value={str(execution.title)}
            hint={'Each run’s name, such as Release ${{ inputs.version }}.'}
            problems={at('/spec/execution/title')}
            onChange={(v) => set(['spec', 'execution', 'title'], v)}
          />
          <TextField
            label="Run timeout"
            value={str(execution.timeout)}
            mono
            hint={DURATION_HINT}
            problems={at('/spec/execution/timeout')}
            onChange={(v) => set(['spec', 'execution', 'timeout'], v)}
          />
          <TextField
            label="Concurrency key"
            value={str(concurrency.key)}
            mono
            hint="Runs with the same key share the limit below (${{ inputs.service }}, or a fixed word)."
            problems={at('/spec/execution/concurrency/key')}
            onChange={(v) => {
              if (v === '') set(['spec', 'execution', 'concurrency'], undefined, false);
              else if (concurrency.key === undefined)
                set(['spec', 'execution', 'concurrency'], { key: v, limit: 1 }, false);
              else set(['spec', 'execution', 'concurrency', 'key'], v);
            }}
          />
          {concurrency.key !== undefined ? (
            <>
              <NumberField
                label="At most at a time"
                value={num(concurrency.limit)}
                min={1}
                problems={at('/spec/execution/concurrency/limit')}
                onChange={(v) => set(['spec', 'execution', 'concurrency', 'limit'], v)}
              />
              <SelectField
                label="When the limit is reached"
                value={str(concurrency.on_conflict)}
                options={[
                  { value: '', label: 'Queue the new run (the default)' },
                  { value: 'queue', label: 'Queue the new run' },
                  { value: 'reject', label: 'Refuse the new run' },
                  { value: 'cancel_previous', label: 'Cancel the earlier runs' },
                ]}
                problems={at('/spec/execution/concurrency/on_conflict')}
                onChange={(v) => set(['spec', 'execution', 'concurrency', 'on_conflict'], v, false)}
              />
            </>
          ) : null}
          <p className="text-sm" data-testid="settings-step-budget">
            <span className="text-fg-muted">Step budget: </span>
            {stepBudget != null
              ? `at most ${stepBudget} step runs (provisional)`
              : 'known once the runbook compiles'}
          </p>
        </Section>
        <Section title="Finally">
          <p className="text-xs text-fg-muted">Steps that run at the end of every run.</p>
          {finallySteps.length > 0 ? (
            <ul className="grid gap-1 text-sm">
              {finallySteps.map((step) => (
                <li key={step.pointer}>
                  <button
                    type="button"
                    className="text-left font-medium text-accent hover:underline"
                    onClick={() => api.select(step.pointer)}
                  >
                    {step.title}
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
          <div>
            <SmallButton
              onClick={() => {
                const size = api.model.bodies.get('/spec/finally')?.size ?? 0;
                api.edit(
                  () =>
                    insertStep(
                      api.source,
                      api.model,
                      { body: '/spec/finally', index: size },
                      'task',
                    ),
                  { selectResult: true, announce: 'Added a finally step.' },
                );
              }}
            >
              <Plus className="size-3.5" aria-hidden="true" />
              Add a finally step
            </SmallButton>
          </div>
        </Section>
      </div>
    </aside>
  );
}
