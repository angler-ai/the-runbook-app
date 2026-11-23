import { useQueries } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { formatDuration } from '../../lib/format';
import { dollars } from '../execution/RunnerStep';
import { panel, Rows } from '../runner/presentation';

type RunReport = Schemas['ExecutionDocumentView'];

export interface ActivityData {
  busy_ms: number;
  idle_ms: number;
  completed?: number;
  succeeded?: number;
  idle_stops?: number;
  running?: number;
  lease_losses?: number;
  p50_ms?: number | null;
  success_rate?: number | null;
  recent?: {
    step_run_id: string;
    execution_id?: string;
    title?: string;
    status?: string;
    cost_usd?: number;
  }[];
}

export function rate(fraction?: number | null) {
  return fraction == null ? '—' : `${Math.round(fraction * 100)}%`;
}

/** R1-D2: one approval a runner agent worked on, from the run's report. */
export interface AgentDecisionRow {
  stepRunId: string;
  executionId: string;
  runTitle: string;
  approval: string;
  /** `decided`, `abstained` or `reviewing`. */
  state: 'decided' | 'abstained' | 'reviewing';
  /** The option chosen (decided), or the agent's reason (abstained). */
  detail?: string;
  comment?: string;
  evidence: string[];
  model?: string;
}

/**
 * R1-D2: the agent's decisions, abstentions and reviews in progress, from
 * the reports of runs its recent claims belong to. Decision runs claim
 * the approval's step run, so a recent claim on an approval section is
 * one. Newest claims first, as the activity lists them.
 */
export function decisionRows(
  recent: NonNullable<ActivityData['recent']>,
  reports: ReadonlyMap<string, RunReport>,
): AgentDecisionRow[] {
  const rows: AgentDecisionRow[] = [];
  const seen = new Set<string>();
  for (const claim of recent) {
    if (!claim.execution_id || seen.has(claim.step_run_id)) continue;
    const report = reports.get(claim.execution_id);
    const section = report?.sections.find((s) => s.step_run_id === claim.step_run_id);
    const approval = section?.approval;
    if (!report || !section || !approval) continue;
    seen.add(claim.step_run_id);
    const base = {
      stepRunId: claim.step_run_id,
      executionId: claim.execution_id,
      runTitle: report.header.title,
      approval: section.title,
    };
    const optionTitle = (id: string) =>
      approval.options.find((option) => option.id === id)?.title ?? id;
    for (const decision of approval.decisions) {
      if (decision.by !== 'agent') continue;
      rows.push({
        ...base,
        state: 'decided',
        detail: optionTitle(decision.option),
        comment: decision.comment ?? undefined,
        evidence: decision.evidence ?? [],
        model: decision.model ?? undefined,
      });
    }
    for (const note of approval.agent_notes ?? []) {
      rows.push({
        ...base,
        state: 'abstained',
        detail: note.reason,
        evidence: [],
        model: note.model ?? undefined,
      });
    }
    if (approval.decision_lease) rows.push({ ...base, state: 'reviewing', evidence: [] });
  }
  return rows;
}

/** Runs whose reports the decisions list reads (one request each). */
export const DECISION_RUN_LIMIT = 10;

/** The reports of the runs the agent's recent claims belong to. */
export function useDecisionReports(
  recent: ActivityData['recent'],
  enabled = true,
): Map<string, RunReport> {
  const runs = [
    ...new Set((recent ?? []).flatMap((claim) => (claim.execution_id ? [claim.execution_id] : []))),
  ].slice(0, DECISION_RUN_LIMIT);
  const results = useQueries({
    queries: runs.map((exe) => ({
      queryKey: queryKeys.executionDocument(exe),
      enabled,
      queryFn: async () => {
        const { data } = await api.GET('/v1/executions/{exe}/report', {
          params: { path: { exe } },
        });
        return data as RunReport;
      },
    })),
  });
  const reports = new Map<string, RunReport>();
  runs.forEach((exe, index) => {
    const data = results[index]?.data;
    if (data) reports.set(exe, data);
  });
  return reports;
}

const stateWords: Record<AgentDecisionRow['state'], string> = {
  decided: 'Decided',
  abstained: 'Abstained',
  reviewing: 'Reviewing now',
};

export function AgentDecisions({ rows, project }: { rows: AgentDecisionRow[]; project?: string }) {
  return (
    <div className="grid gap-2">
      <h3 className="text-sm font-semibold">Recent approval decisions</h3>
      {rows.length === 0 ? (
        <p className="text-fg-muted">
          This agent has not decided or abstained on an approval lately.
        </p>
      ) : (
        <Rows
          label="Recent approval decisions"
          columns={['Approval', 'Outcome', 'Reason', 'Transcript']}
          rows={rows.map((row, index) => ({
            id: `${row.stepRunId}-${row.state}-${index}`,
            cells: [
              <a
                key="run"
                className="text-accent underline"
                href={`/x/${encodeURIComponent(row.executionId)}`}
              >
                {row.approval} · {row.runTitle}
              </a>,
              <span key="outcome">
                {stateWords[row.state]}
                {row.state === 'decided' && row.detail ? `: ${row.detail}` : ''}
                {row.model ? (
                  <span className="block text-xs text-fg-muted">{row.model}</span>
                ) : null}
              </span>,
              <span key="reason" className="grid gap-0.5">
                {row.state === 'abstained' ? row.detail : (row.comment ?? '—')}
                {row.evidence.length > 0 ? (
                  <span className="font-mono text-xs text-fg-muted">
                    Evidence: {row.evidence.join(', ')}
                  </span>
                ) : null}
              </span>,
              project ? (
                <a
                  key="transcript"
                  className="text-accent underline"
                  href={`/p/${encodeURIComponent(project)}/agents/transcripts/${encodeURIComponent(row.stepRunId)}`}
                >
                  Transcript
                </a>
              ) : (
                '—'
              ),
            ],
          }))}
        />
      )}
    </div>
  );
}

export function Activity({
  activity,
  cost,
  project,
  decisions,
}: {
  activity?: ActivityData;
  cost?: number;
  project?: string;
  /** R1-D2: the agent's recent approval decisions (see `decisionRows`). */
  decisions?: AgentDecisionRow[];
}) {
  const busy = activity ? formatDuration(activity.busy_ms) : '—';
  const idle = activity ? formatDuration(activity.idle_ms) : '—';
  const metrics = [
    ['7-day cost', cost == null ? '—' : dollars(cost)],
    ['Success rate', rate(activity?.success_rate)],
    ['Median step duration', activity?.p50_ms == null ? '—' : formatDuration(activity.p50_ms)],
    ['Running', activity?.running ?? '—'],
    ['Idle stops', activity?.idle_stops ?? '—'],
    ['Lease losses', activity?.lease_losses ?? '—'],
    ['Busy time (24 hours)', busy],
    ['Idle time (24 hours)', idle],
  ];
  return (
    <div className="grid gap-4">
      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {metrics.map(([label, value]) => (
          <div className={panel} key={label}>
            <dt className="text-sm text-fg-muted">{label}</dt>
            <dd className="mt-1 text-lg font-semibold">{value}</dd>
          </div>
        ))}
      </dl>
      <Rows
        label="Recent steps"
        columns={['Step', 'Status', 'Cost', 'Transcript']}
        rows={(activity?.recent ?? []).map((step) => ({
          id: step.step_run_id,
          cells: [
            step.execution_id ? (
              <a
                className="text-accent underline"
                href={`/x/${encodeURIComponent(step.execution_id)}`}
              >
                {step.title ?? step.step_run_id}
              </a>
            ) : (
              (step.title ?? step.step_run_id)
            ),
            step.status ?? '—',
            step.cost_usd == null ? '—' : dollars(step.cost_usd),
            project ? (
              <a
                className="text-accent underline"
                href={`/p/${encodeURIComponent(project)}/agents/transcripts/${encodeURIComponent(step.step_run_id)}`}
              >
                Transcript
              </a>
            ) : (
              '—'
            ),
          ],
        }))}
      />
      {decisions ? <AgentDecisions rows={decisions} project={project} /> : null}
    </div>
  );
}
