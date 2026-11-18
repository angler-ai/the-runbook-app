// Triggers in the builder: schedules and webhooks that start runs on their
// own (LANG9b `spec.triggers`). Every change is written to the YAML like the
// rest of the settings panel; the compiler's problems show under each field.
import { CalendarClock, Check, Copy, Plus, Trash2, Webhook } from 'lucide-react';
import { useId, useState, type ReactNode } from 'react';
import { Input } from '../../ui/primitives/input';
import { cn } from '../../ui/primitives/cn';
import {
  browserTimeZone,
  cronFor,
  describeCron,
  parseTimeValue,
  scheduleShape,
  shapeTime,
  timeValue,
  timeZones,
  WEEKDAY_NAMES,
  type ScheduleKind,
  type ScheduleShape,
} from '../runbook/schedule';
import { useBuilder } from './context';
import { FieldShell, NumberField, SelectField, SmallButton, TextField } from './fields';
import { uniqueId, type Raw } from './model';
import { DeclarationsEditor } from './StepPanel';
import { deleteIn, insertItem, removeItem, setIn, type YamlPath } from './yamlEdit';

type TriggerKind = 'schedule' | 'webhook';

function obj(value: unknown): Raw {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Raw) : {};
}

function str(value: unknown): string {
  return typeof value === 'string' ? value : value == null ? '' : String(value);
}

function list(value: unknown): Raw[] {
  return Array.isArray(value) ? value.map(obj) : [];
}

const KIND_CHOICES: { value: ScheduleKind; label: string }[] = [
  { value: 'hourly', label: 'Every hour' },
  { value: 'daily', label: 'Every day' },
  { value: 'weekdays', label: 'Every weekday (Mon to Fri)' },
  { value: 'weekly', label: 'Every week' },
  { value: 'monthly', label: 'Every month' },
  { value: 'custom', label: 'Custom (cron expression)' },
];

const CATCH_UP: { value: string; label: string }[] = [
  { value: '', label: 'Skip it (the default)' },
  { value: '15m', label: 'Run it if up to 15 minutes late' },
  { value: '1h', label: 'Run it if up to 1 hour late' },
  { value: '6h', label: 'Run it if up to 6 hours late' },
  { value: '24h', label: 'Run it if up to 24 hours late' },
];

/** A typed value as its input's type wants it (`"5"` → 5 for a number). */
function typed(text: string, type: string): unknown {
  if ((type === 'integer' || type === 'number') && /^-?\d+(\.\d+)?$/.test(text.trim())) {
    return Number(text.trim());
  }
  if (type === 'boolean' && (text === 'true' || text === 'false')) return text === 'true';
  return text;
}

/** The values a trigger gives the runbook's inputs. */
function TriggerInputs({
  path,
  values,
  expressions,
}: {
  path: YamlPath;
  values: Raw;
  /** Webhooks may use `${{ payload.* }}`; schedules take fixed values. */
  expressions: boolean;
}) {
  const api = useBuilder();
  const declared = Object.entries(obj(api.model.raw.spec.inputs)).filter(
    ([name]) => !name.startsWith('x-'),
  );
  const pointer = `/${path.join('/')}`;
  if (declared.length === 0) {
    return (
      <p className="text-xs text-fg-muted">
        This runbook has no inputs, so there is nothing to fill in.
      </p>
    );
  }
  const write = (name: string, v: unknown) =>
    api.edit(
      () =>
        v === '' || v === undefined
          ? deleteIn(api.source, [...path, name], { prune: true })
          : setIn(api.source, [...path, name], v),
      { coalesce: `trigger-input:${path.join('.')}.${name}` },
    );
  return (
    <fieldset className="grid gap-2">
      <legend className="mb-1 text-sm font-medium text-fg">Inputs for each run</legend>
      {declared.map(([name, raw]) => {
        const decl = obj(raw);
        const type = str(decl.type) || 'string';
        const needed = decl.required === true && decl.default === undefined;
        const label = `${str(decl.title) || name}${needed ? ' (required)' : ''}`;
        const problems = api.problemsAt(`${pointer}/${name}`);
        const current = values[name];
        const fallback =
          decl.default !== undefined ? `Empty: the default, ${JSON.stringify(decl.default)}.` : '';
        if (type === 'object' || type === 'array') {
          return (
            <p key={name} className="text-xs text-fg-muted">
              <span className="font-medium text-fg">{str(decl.title) || name}</span>: a {type}; set
              it in the YAML tab.
            </p>
          );
        }
        const options = Array.isArray(decl.options)
          ? decl.options.filter((o): o is string => typeof o === 'string')
          : [];
        if (!expressions && (type === 'boolean' || options.length > 0)) {
          const choices = type === 'boolean' ? ['true', 'false'] : options;
          return (
            <SelectField
              key={name}
              label={label}
              value={str(current)}
              options={[
                { value: '', label: needed ? 'Choose…' : 'Not set' },
                ...choices.map((c) => ({ value: c, label: c })),
              ]}
              hint={fallback || undefined}
              problems={problems}
              onChange={(v) => write(name, typed(v, type))}
            />
          );
        }
        return (
          <TextField
            key={name}
            label={label}
            value={str(current)}
            mono={expressions}
            placeholder={expressions ? '${{ payload.field }} or a fixed value' : undefined}
            hint={fallback || undefined}
            problems={problems}
            onChange={(v) => write(name, typed(v, type))}
          />
        );
      })}
    </fieldset>
  );
}

function TimeField({
  label,
  hour,
  minute,
  onChange,
}: {
  label: string;
  hour: number;
  minute: number;
  onChange(time: { hour: number; minute: number }): void;
}) {
  const id = useId();
  return (
    <FieldShell id={id} label={label}>
      {(describedBy) => (
        <Input
          id={id}
          type="time"
          value={timeValue(hour, minute)}
          aria-describedby={describedBy}
          className="h-9 text-sm"
          onChange={(e) => {
            const time = parseTimeValue(e.target.value);
            if (time) onChange(time);
          }}
        />
      )}
    </FieldShell>
  );
}

function ScheduleCard({ index, raw }: { index: number; raw: Raw }) {
  const api = useBuilder();
  const path: YamlPath = ['spec', 'triggers', 'schedule', index];
  const pointer = `/spec/triggers/schedule/${index}`;
  const cron = str(raw.cron);
  const timezone = str(raw.timezone) || 'UTC';
  const shape = scheduleShape(cron);
  // "Custom" stays chosen even while the cron happens to match a shape.
  const [custom, setCustom] = useState(shape.kind === 'custom');
  const kind: ScheduleKind = custom ? 'custom' : shape.kind;
  const time = shapeTime(shape);
  const said = describeCron(cron);
  const zones = timeZones();
  if (!zones.includes(timezone)) zones.unshift(timezone);

  const set = (key: string, v: unknown, coalesce = true) =>
    api.edit(
      () =>
        v === '' || v === undefined
          ? deleteIn(api.source, [...path, key])
          : setIn(api.source, [...path, key], v),
      coalesce ? { coalesce: `trigger:${pointer}/${key}` } : {},
    );
  const setShape = (next: Exclude<ScheduleShape, { kind: 'custom' }>) =>
    set('cron', cronFor(next), false);
  const at = (key: string) => api.problemsAt(`${pointer}/${key}`);

  function chooseKind(next: ScheduleKind) {
    if (next === 'custom') {
      setCustom(true);
      return;
    }
    setCustom(false);
    const { hour, minute } = time;
    const day = shape.kind === 'monthly' ? shape.day : 1;
    const weekday = shape.kind === 'weekly' ? shape.weekday : 1;
    if (next === 'hourly') setShape({ kind: 'hourly', minute });
    else if (next === 'daily') setShape({ kind: 'daily', hour, minute });
    else if (next === 'weekdays') setShape({ kind: 'weekdays', hour, minute });
    else if (next === 'weekly') setShape({ kind: 'weekly', weekday, hour, minute });
    else setShape({ kind: 'monthly', day, hour, minute });
  }

  return (
    <li
      className="grid gap-3 rounded-md border border-border p-3"
      data-testid={`schedule-trigger-${index}`}
    >
      <TriggerHeader
        icon={<CalendarClock className="size-4 shrink-0 text-fg-muted" aria-hidden="true" />}
        title={str(raw.id) || 'Schedule'}
        kind="schedule"
        index={index}
      />
      <TextField
        label="Name"
        value={str(raw.id)}
        mono
        hint={`Runs show who started them as “Schedule ${str(raw.id) || 'name'}”. Lowercase letters, digits and _.`}
        problems={at('id')}
        onChange={(v) => set('id', v)}
      />
      <SelectField
        label="Repeats"
        value={kind}
        options={KIND_CHOICES}
        onChange={(v) => chooseKind(v as ScheduleKind)}
      />
      {kind === 'hourly' && shape.kind === 'hourly' ? (
        <NumberField
          label="Minutes past the hour"
          value={shape.minute}
          min={0}
          max={59}
          onChange={(v) => v !== undefined && setShape({ kind: 'hourly', minute: v })}
        />
      ) : null}
      {kind === 'weekly' && shape.kind === 'weekly' ? (
        <SelectField
          label="Day"
          value={String(shape.weekday)}
          options={WEEKDAY_NAMES.map((name, i) => ({ value: String(i), label: name }))}
          onChange={(v) => setShape({ ...shape, weekday: Number(v) })}
        />
      ) : null}
      {kind === 'monthly' && shape.kind === 'monthly' ? (
        <NumberField
          label="Day of the month"
          value={shape.day}
          min={1}
          max={31}
          hint="Months without this day are skipped (the 31st skips 30-day months)."
          onChange={(v) => v !== undefined && setShape({ ...shape, day: v })}
        />
      ) : null}
      {(kind === 'daily' || kind === 'weekdays' || kind === 'weekly' || kind === 'monthly') &&
      shape.kind === kind ? (
        <TimeField
          label="Time"
          hour={shape.hour}
          minute={shape.minute}
          onChange={(t) => setShape({ ...shape, ...t })}
        />
      ) : null}
      {kind === 'custom' ? (
        <TextField
          label="Cron expression"
          value={cron}
          mono
          placeholder="0 2 * * *"
          hint="Minute, hour, day of month, month, day of week. For example */15 * * * * is every 15 minutes."
          problems={at('cron')}
          onChange={(v) => set('cron', v)}
        />
      ) : at('cron').length > 0 ? (
        <p className="text-xs text-danger-text">{at('cron').join(' ')}</p>
      ) : null}
      <SelectField
        label="Time zone"
        value={timezone}
        options={zones.map((z) => ({ value: z, label: z.replace(/_/g, ' ') }))}
        problems={at('timezone')}
        onChange={(v) => set('timezone', v, false)}
      />
      <p
        className="rounded-md bg-surface-raised px-2.5 py-2 text-sm"
        data-testid="schedule-summary"
      >
        {said ? (
          <>
            {said} <span className="text-fg-muted">({timezone.replace(/_/g, ' ')})</span>
          </>
        ) : cron ? (
          <span className="text-fg-muted">
            Runs on cron <code className="font-mono">{cron}</code> in {timezone}.
          </span>
        ) : (
          <span className="text-fg-muted">Enter a cron expression.</span>
        )}
      </p>
      <SelectField
        label="If a run is missed"
        value={str(raw.catch_up)}
        options={
          CATCH_UP.some((c) => c.value === str(raw.catch_up))
            ? CATCH_UP
            : [
                ...CATCH_UP,
                { value: str(raw.catch_up), label: `Run it if up to ${str(raw.catch_up)} late` },
              ]
        }
        hint="For when the service was down at the scheduled time."
        problems={at('catch_up')}
        onChange={(v) => set('catch_up', v, false)}
      />
      <TriggerInputs path={[...path, 'inputs']} values={obj(raw.inputs)} expressions={false} />
    </li>
  );
}

function CopyUrl({ url }: { url: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="grid gap-1">
      <span className="text-sm font-medium text-fg">URL</span>
      <div className="flex min-w-0 items-start gap-2">
        <code className="min-w-0 flex-1 break-all rounded bg-surface-raised px-2 py-1 font-mono text-xs ring-1 ring-border ring-inset">
          POST {url}
        </code>
        <SmallButton
          onClick={() => {
            void navigator.clipboard?.writeText(url).then(
              () => setCopied(true),
              () => setCopied(false),
            );
          }}
        >
          {copied ? (
            <Check className="size-3.5" aria-hidden="true" />
          ) : (
            <Copy className="size-3.5" aria-hidden="true" />
          )}
          {copied ? 'Copied' : 'Copy'}
        </SmallButton>
      </div>
    </div>
  );
}

function WebhookCard({ index, raw }: { index: number; raw: Raw }) {
  const api = useBuilder();
  const path: YamlPath = ['spec', 'triggers', 'webhook', index];
  const pointer = `/spec/triggers/webhook/${index}`;
  const id = str(raw.id);
  const set = (key: string, v: string) =>
    api.edit(
      () =>
        v === '' ? deleteIn(api.source, [...path, key]) : setIn(api.source, [...path, key], v),
      { coalesce: `trigger:${pointer}/${key}` },
    );
  const at = (key: string) => api.problemsAt(`${pointer}/${key}`);
  const url = id && api.triggerUrl ? api.triggerUrl(id) : undefined;

  return (
    <li
      className="grid gap-3 rounded-md border border-border p-3"
      data-testid={`webhook-trigger-${index}`}
    >
      <TriggerHeader
        icon={<Webhook className="size-4 shrink-0 text-fg-muted" aria-hidden="true" />}
        title={id || 'Webhook'}
        kind="webhook"
        index={index}
      />
      <TextField
        label="Name"
        value={id}
        mono
        hint={`Part of the URL; runs show “Webhook ${id || 'name'}”. Lowercase letters, digits and _.`}
        problems={at('id')}
        onChange={(v) => set('id', v)}
      />
      {url ? (
        <CopyUrl url={url} />
      ) : (
        <p className="text-xs text-fg-muted">The URL appears here once the runbook is saved.</p>
      )}
      <p className="text-xs text-fg-muted">
        The caller sends its JSON as the body, with a project service token that may start runs (
        <code className="font-mono">executions:start</code>) in an{' '}
        <code className="font-mono">Authorization: Bearer</code> header, or as{' '}
        <code className="font-mono">?token=</code> on the URL. Calls start runs once you publish.
      </p>
      <DeclarationsEditor
        title="Payload fields"
        mapPath={[...path, 'payload']}
        value={obj(raw.payload)}
        addLabel="Add a payload field"
        base="field"
        problemsAt={(name) => at(`payload/${name}`)}
      />
      <p className="-mt-2 text-xs text-fg-muted">
        The fields of the body that the settings below may read, as{' '}
        <code className="font-mono">{'${{ payload.<name> }}'}</code>. Others are ignored.
      </p>
      <TextField
        label="Only start a run when"
        value={str(raw.when)}
        mono
        placeholder="${{ payload.status == 'success' }}"
        hint="Empty: every call starts a run."
        problems={at('when')}
        onChange={(v) => set('when', v)}
      />
      <TriggerInputs path={[...path, 'inputs']} values={obj(raw.inputs)} expressions />
      <TextField
        label="Treat calls as repeats when this matches"
        value={str(raw.dedupe)}
        mono
        placeholder="${{ payload.delivery_id }}"
        hint="A call with the same value as an earlier one returns that run instead of starting another."
        problems={at('dedupe')}
        onChange={(v) => set('dedupe', v)}
      />
    </li>
  );
}

function TriggerHeader({
  icon,
  title,
  kind,
  index,
}: {
  icon: ReactNode;
  title: string;
  kind: TriggerKind;
  index: number;
}) {
  const api = useBuilder();
  const triggers = obj(api.model.raw.spec.triggers);
  const remove = () =>
    api.edit(
      () => {
        const mine = list(triggers[kind]);
        const other = list(triggers[kind === 'schedule' ? 'webhook' : 'schedule']);
        if (mine.length > 1) return removeItem(api.source, ['spec', 'triggers', kind], index);
        if (other.length > 0) return deleteIn(api.source, ['spec', 'triggers', kind]);
        return deleteIn(api.source, ['spec', 'triggers']);
      },
      { announce: `Removed the ${kind} ${title}.` },
    );
  return (
    <div className="flex items-center gap-2">
      {icon}
      <strong className="min-w-0 flex-1 truncate font-mono text-sm">{title}</strong>
      <button
        type="button"
        onClick={remove}
        aria-label={`Remove ${kind} ${title}`}
        title={`Remove ${kind}`}
        className={cn(
          'inline-flex size-8 shrink-0 items-center justify-center rounded-md text-fg-muted hover:bg-hover hover:text-danger-text focus-visible:outline-2 focus-visible:outline-focus',
        )}
      >
        <Trash2 className="size-4" aria-hidden="true" />
      </button>
    </div>
  );
}

/** The required inputs a new schedule must give: their defaults, else nothing. */
function startingInputs(spec: Raw): Raw {
  const inputs: Raw = {};
  for (const [name, raw] of Object.entries(obj(spec.inputs))) {
    const decl = obj(raw);
    if (decl.required === true && decl.default === undefined) {
      const options = Array.isArray(decl.options) ? decl.options : [];
      if (typeof options[0] === 'string') inputs[name] = options[0];
      else if (decl.type === 'boolean') inputs[name] = false;
    }
  }
  return inputs;
}

export function TriggersEditor() {
  const api = useBuilder();
  const spec = api.model.raw.spec;
  const triggers = obj(spec.triggers);
  const schedules = list(triggers.schedule);
  const webhooks = list(triggers.webhook);
  const taken = new Set([...schedules, ...webhooks].map((t) => str(t.id)));

  function add(kind: TriggerKind) {
    const at = kind === 'schedule' ? schedules.length : webhooks.length;
    const id = uniqueId(kind === 'schedule' ? 'daily' : 'incoming', taken);
    const inputs = kind === 'schedule' ? startingInputs(spec) : {};
    const value: Raw =
      kind === 'schedule'
        ? {
            id,
            cron: '0 9 * * *',
            timezone: browserTimeZone(),
            ...(Object.keys(inputs).length > 0 ? { inputs } : {}),
          }
        : { id };
    api.edit(() => insertItem(api.source, ['spec', 'triggers', kind], at, value), {
      announce: kind === 'schedule' ? `Added the schedule ${id}.` : `Added the webhook ${id}.`,
    });
  }

  return (
    <div className="grid gap-3" data-testid="triggers-editor">
      <p className="text-xs text-fg-muted">
        Start runs on a schedule or when another system calls a URL. Only the latest published
        version’s triggers act.
      </p>
      {schedules.length > 0 ? (
        <ul className="grid gap-2" aria-label="Schedules">
          {schedules.map((raw, i) => (
            <ScheduleCard key={i} index={i} raw={raw} />
          ))}
        </ul>
      ) : null}
      {webhooks.length > 0 ? (
        <ul className="grid gap-2" aria-label="Webhooks">
          {webhooks.map((raw, i) => (
            <WebhookCard key={i} index={i} raw={raw} />
          ))}
        </ul>
      ) : null}
      <div className="flex flex-wrap gap-2">
        <SmallButton onClick={() => add('schedule')}>
          <Plus className="size-3.5" aria-hidden="true" />
          Add a schedule
        </SmallButton>
        <SmallButton onClick={() => add('webhook')}>
          <Plus className="size-3.5" aria-hidden="true" />
          Add a webhook
        </SmallButton>
      </div>
    </div>
  );
}
