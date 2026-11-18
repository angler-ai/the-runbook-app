// What tells two runs of one runbook apart (review A): a short summary
// of the run's scalar inputs (or indexed fields), e.g.
// `customer=Northwind Goods`. Identity, never instructions or secrets:
// scalars only, at most two, each clipped.
import { useQueries } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';
import { queryKeys } from '../../api/query-keys';

export interface RunIdentity {
  /** `key=value` of the first scalar inputs/fields, comma-joined. */
  summary?: string;
  startedAtMs?: number;
  /** Pre-formatted assignee of one step ("team oncall"). */
  assigned?: string;
}

type Data = Record<string, unknown>;

function scalar(value: unknown): string | undefined {
  if (typeof value === 'string') return value.length > 32 ? `${value.slice(0, 31)}…` : value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return undefined;
}

/** Up to two `key=value` scalar entries; undefined when there are none. */
export function dataSummary(data: Data | undefined | null): string | undefined {
  if (!data) return undefined;
  const parts: string[] = [];
  for (const [key, value] of Object.entries(data)) {
    const shown = scalar(value);
    if (shown !== undefined && shown !== '') parts.push(`${key}=${shown}`);
    if (parts.length === 2) break;
  }
  return parts.length > 0 ? parts.join(' · ') : undefined;
}

type ExecutionView = Schemas['ExecutionView'];
type RecipientRef = Schemas['RecipientRef'];

export function assignedLabel(
  view: ExecutionView | undefined,
  stepRunId: string,
  names: { nameOf(id: string): string; teamName(reference: string): string },
): string | undefined {
  const step = view?.steps.find((s) => s.id === stepRunId);
  if (!step) return undefined;
  const refs: RecipientRef[] =
    (step.assignees?.length ?? 0) > 0 ? (step.assignees ?? []) : step.eligible_recipients;
  if (refs.length === 0) return undefined;
  const labels = refs
    .slice(0, 2)
    .map((ref) =>
      ref.kind === 'user' ? names.nameOf(ref.reference) : `team ${names.teamName(ref.reference)}`,
    );
  return `assigned to ${labels.join(', ')}${refs.length > 2 ? '…' : ''}`;
}

/** Runs whose identity a list fetches: one request each, so the first only. */
export const RUN_IDENTITY_LIMIT = 50;

/**
 * The identity of each run in a list, fetched once per run for the first
 * RUN_IDENTITY_LIMIT runs (an inbox can hold a thousand tasks; later rows
 * show without it). Steps add their assignee when a step id is given per
 * run via `stepOf`.
 */
export function useRunIdentities(
  executionIds: string[],
  names: { nameOf(id: string): string; teamName(reference: string): string },
  stepOf?: (executionId: string) => string | undefined,
): Map<string, RunIdentity> {
  const unique = [...new Set(executionIds)].slice(0, RUN_IDENTITY_LIMIT);
  const results = useQueries({
    queries: unique.map((id) => ({
      queryKey: queryKeys.execution(id),
      queryFn: async () => {
        const { data } = await api.GET('/v1/executions/{exe}', {
          params: { path: { exe: id } },
        });
        return data as ExecutionView;
      },
    })),
  });
  const out = new Map<string, RunIdentity>();
  unique.forEach((id, index) => {
    const view = results[index]?.data;
    if (!view) return;
    const stepId = stepOf?.(id);
    out.set(id, {
      summary: dataSummary(view.inputs) ?? dataSummary(view.fields),
      startedAtMs: view.started_at_ms ?? view.created_at_ms ?? undefined,
      assigned: stepId ? assignedLabel(view, stepId, names) : undefined,
    });
  });
  return out;
}
