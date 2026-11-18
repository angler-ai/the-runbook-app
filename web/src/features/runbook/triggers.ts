import { parse } from 'yaml';

export interface ScheduleTriggerView {
  id: string;
  cron: string;
  timezone: string;
  catchUp?: string;
  inputNames: string[];
}

export interface WebhookTriggerView {
  id: string;
  hasPayloadSchema: boolean;
  conditional: boolean;
  deduped: boolean;
  inputNames: string[];
}

export interface RunbookTriggersView {
  schedule: ScheduleTriggerView[];
  webhook: WebhookTriggerView[];
}

const EMPTY: RunbookTriggersView = { schedule: [], webhook: [] };

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}

function durationLabel(value: unknown): string | undefined {
  if (typeof value === 'string' && value.length > 0) return value;
  if (typeof value === 'number' && Number.isFinite(value)) return String(value);
  return undefined;
}

function inputNames(value: unknown): string[] {
  const inputs = record(value);
  return inputs ? Object.keys(inputs) : [];
}

function entries(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    const parsed = record(entry);
    return parsed ? [parsed] : [];
  });
}

/** `spec.triggers` of a source, as the runbook page shows them. Invalid or partial entries are omitted. */
export function triggersOf(source: string | undefined): RunbookTriggersView {
  if (!source) return EMPTY;
  let root: Record<string, unknown> | undefined;
  try {
    root = record(parse(source));
  } catch {
    return EMPTY;
  }
  const triggers = record(record(root?.spec)?.triggers);
  if (!triggers) return EMPTY;

  const schedule = entries(triggers.schedule).flatMap((entry) => {
    const id = stringValue(entry.id);
    const cron = stringValue(entry.cron);
    if (!id || !cron) return [];
    const catchUp = durationLabel(entry.catch_up);
    return [
      {
        id,
        cron,
        timezone: stringValue(entry.timezone) ?? 'UTC',
        ...(catchUp ? { catchUp } : {}),
        inputNames: inputNames(entry.inputs),
      },
    ];
  });
  const webhook = entries(triggers.webhook).flatMap((entry) => {
    const id = stringValue(entry.id);
    if (!id) return [];
    return [
      {
        id,
        hasPayloadSchema: record(entry.payload) !== undefined,
        conditional: stringValue(entry.when) !== undefined,
        deduped: stringValue(entry.dedupe) !== undefined,
        inputNames: inputNames(entry.inputs),
      },
    ];
  });

  return { schedule, webhook };
}

export function webhookTriggerUrl({
  origin,
  project,
  runbook,
  trigger,
}: {
  origin: string;
  project: string;
  runbook: string;
  trigger: string;
}): string {
  const base = origin.replace(/\/+$/, '');
  return `${base}/v1/projects/${encodeURIComponent(project)}/runbooks/${encodeURIComponent(runbook)}/triggers/${encodeURIComponent(trigger)}`;
}
