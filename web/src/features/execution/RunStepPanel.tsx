// UI-3: the graph's side panel during a run: the selected step's run
// (state, who did it, when, summary, outputs, evidence, log, checklist,
// approval votes, a strict assignee, a take-over) and what this person may
// do with it: the inline actions the Steps view offers (M2's StepActions,
// with LANG7's take-over and strict-assignee gating).
// The step's definition folds below, under "Definition".
import type { ReactNode } from 'react';
import { Link } from '@tanstack/react-router';
import { ExternalLink, UserCheck } from 'lucide-react';
import type { MeView } from '../../api/hooks';
import { NodeDetails, RunStateChip, type NodeOverlay } from '../../ui/graph';
import type { OutlineNode } from '../../ui/types';
import { LogBlock, isUserId, stepHeadingId } from '../../ui/execution';
import { ActorBadge, StatusPill, StepLabels, WaitBadge } from '../../ui/status';
import { isEligibleToDecide, type Capabilities } from './gating';
import { useLogView } from './logs';
import type { RunRecord } from './overlay';
import { StepActions } from './StepActions';
import { strictAssigneeLabel, takenOverText } from './takeOver';
import type { DirectoryNames } from '../context/names';

export interface RunStepPanelProps {
  executionId: string;
  node: OutlineNode;
  record: RunRecord | undefined;
  run: NodeOverlay | undefined;
  me: MeView | undefined;
  caps: Capabilities;
  documentAt: number;
  names: DirectoryNames;
  onClose: () => void;
  callHref?: (slug: string) => string | undefined;
  /** A call step's child run status, when read. */
  childStatus?: string;
  /** The run is paused: no claim or take-over is offered (LANG7). */
  runPaused?: boolean;
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[88px_1fr] gap-x-3 py-1.5 text-sm">
      <dt className="text-fg-muted">{label}</dt>
      <dd className="min-w-0 break-words text-fg">{children}</dd>
    </div>
  );
}

function Heading({ children }: { children: ReactNode }) {
  return (
    <h3 className="mb-1.5 mt-4 text-2xs font-semibold uppercase tracking-wider text-fg-muted">
      {children}
    </h3>
  );
}

function clock(ms: number): string {
  return new Date(ms).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

export function isHttps(value: string): boolean {
  try {
    return new URL(value).protocol === 'https:';
  } catch {
    return false;
  }
}

/** One output value as text: JSON for objects, a person's name for a user id. */
export function outputText(value: unknown, nameOf: (id: string) => string): string {
  if (typeof value === 'object' && value !== null) return JSON.stringify(value);
  if (isUserId(value)) return nameOf(value);
  return String(value);
}

export function OutputsTable({
  outputs,
  nameOf,
}: {
  outputs: Record<string, unknown>;
  nameOf: (id: string) => string;
}) {
  const entries = Object.entries(outputs);
  if (entries.length === 0) return null;
  return (
    <table data-slot="outputs-table" className="w-full border-collapse text-xs">
      <tbody>
        {entries.map(([key, value]) => (
          <tr key={key} className="border-b border-border last:border-b-0">
            <th
              scope="row"
              className="w-2/5 py-1 pr-3 text-left align-top font-mono font-normal text-fg-muted"
            >
              {key}
            </th>
            <td className="break-all py-1 align-top font-mono text-fg">
              {outputText(value, nameOf)}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export { EvidenceList } from './evidence/EvidenceViewer';
import { EvidenceList } from './evidence/EvidenceViewer';

/** LANG9: a failure's type, as `retry.on` and `catch_on` name it. */
const FAILURE_TYPES: Record<string, string> = {
  timeout: 'Timed out',
  failed: 'Reported as failed',
  call_failed: 'Child runbook failed',
};

function RunDetails({
  executionId,
  record,
  run,
  me,
  caps,
  documentAt,
  names,
  childStatus,
  runPaused = false,
}: Omit<RunStepPanelProps, 'node' | 'onClose' | 'callHref' | 'record'> & { record: RunRecord }) {
  const { section, step } = record;
  const logs = useLogView(section, documentAt);
  const checklist = step?.checklist ?? [];
  const approval = section.approval;
  const claimant = step?.claim && section.status === 'claimed' ? section.actor : null;
  const taken = takenOverText(section, names.nameOf);
  const strict = section.assignee?.strict ? section.assignee : null;
  return (
    <div data-slot="run-details" data-step-run={section.step_run_id}>
      {record.scope.length > 0 ? (
        <p className="mb-2 text-xs text-fg-muted">{record.scopeNames.join(' · ')}</p>
      ) : null}
      {taken ? (
        <p
          data-slot="panel-takeover"
          className="mb-2 flex items-center gap-2 rounded-md bg-accent-subtle px-3 py-2 text-xs text-accent-strong"
        >
          <UserCheck className="size-3.5 shrink-0" aria-hidden="true" />
          {taken}
        </p>
      ) : null}
      <dl className="divide-y divide-border">
        <Fact label="Status">
          <StatusPill status={section.status} />
          {run?.note ? <span className="ml-2 text-xs text-fg-muted">{run.note}</span> : null}
        </Fact>
        {section.failure_type ? (
          <Fact label="Failure">
            <span data-slot="failure-type" className="text-sm text-fg">
              {FAILURE_TYPES[section.failure_type] ?? section.failure_type}
            </span>
          </Fact>
        ) : null}
        {section.labels && Object.keys(section.labels).length > 0 ? (
          <Fact label="Labels">
            <StepLabels labels={section.labels} />
          </Fact>
        ) : null}
        {run?.badge ? (
          <Fact label="Waiting on">
            <WaitBadge kind={run.badge.kind} label={run.badge.label} />
          </Fact>
        ) : null}
        {section.actor && section.actor.kind !== 'system' ? (
          <Fact label={claimant ? 'Claimed by' : 'Done by'}>
            <ActorBadge
              actor={section.actor}
              {...(section.actor.kind === 'human'
                ? { name: names.nameOf(section.actor.user_id) }
                : {})}
            />
          </Fact>
        ) : null}
        {strict ? (
          <Fact label="Assigned to">
            {strictAssigneeLabel(section, names)} (strict)
            {strict.routing_error ? (
              <span className="block text-xs text-fg-muted">
                Nobody eligible yet: the project admins were told.
              </span>
            ) : null}
          </Fact>
        ) : null}
        {step && step.attempt > 1 ? <Fact label="Attempt">{step.attempt}</Fact> : null}
        {section.followup?.reminders_sent ? (
          <Fact label="Reminded">
            {section.followup.reminders_sent}{' '}
            {section.followup.reminders_sent === 1 ? 'time' : 'times'}
          </Fact>
        ) : null}
        {section.followup?.escalated ? (
          <Fact label="Escalated to">
            {section.followup.escalated_to && section.followup.escalated_to.length > 0
              ? section.followup.escalated_to
                  .map((recipient) =>
                    recipient.kind === 'user'
                      ? names.nameOf(recipient.reference)
                      : `team ${names.teamName(recipient.reference)}`,
                  )
                  .join(', ')
              : 'Yes'}
          </Fact>
        ) : null}
        {section.completed_at_ms ? (
          <Fact label="Finished">{clock(section.completed_at_ms)}</Fact>
        ) : null}
        {step?.wait?.until_ms ? (
          <Fact label={step.wait.signal ? 'Gives up' : 'Fires'}>{clock(step.wait.until_ms)}</Fact>
        ) : null}
        {step?.call ? (
          <Fact label="Child run">
            {step.call.started ? (
              <Link
                to="/x/$exe"
                params={{ exe: step.call.child_execution_id }}
                className="inline-flex items-center gap-1 font-medium text-accent underline-offset-2 hover:underline"
              >
                {childStatus ? `Open (${childStatus})` : 'Open'}
                <ExternalLink className="size-3.5" aria-hidden="true" />
              </Link>
            ) : (
              <span className="text-fg-muted">
                Not started{step.call.error ? ` (${step.call.error})` : ''}
              </span>
            )}
          </Fact>
        ) : null}
      </dl>

      {section.summary ? (
        <>
          <Heading>Summary</Heading>
          <p data-slot="run-summary" className="whitespace-pre-wrap text-sm leading-relaxed">
            {section.summary}
          </p>
        </>
      ) : null}
      {Object.keys(section.outputs).length > 0 ? (
        <>
          <Heading>Outputs</Heading>
          <OutputsTable outputs={section.outputs} nameOf={names.nameOf} />
        </>
      ) : null}
      {section.evidence.length > 0 ? (
        <>
          <Heading>Evidence</Heading>
          <EvidenceList evidence={section.evidence} executionId={executionId} />
        </>
      ) : null}
      {checklist.length > 0 ? (
        <>
          <Heading>Checklist</Heading>
          <ul className="grid gap-1 text-sm">
            {checklist.map((item) => (
              <li key={item.title} className="flex items-center gap-2">
                <span
                  aria-hidden="true"
                  className={
                    item.completed
                      ? 'size-3 rounded-sm bg-status-succeeded-fg'
                      : 'size-3 rounded-sm border border-border-strong'
                  }
                />
                <span className={item.completed ? 'text-fg-muted line-through' : ''}>
                  {item.title}
                </span>
                <span className="sr-only">{item.completed ? '(done)' : '(not done)'}</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {approval ? (
        <>
          <Heading>Approval</Heading>
          {/* An eligible voter reads the prompt in the decision panel below. */}
          {isEligibleToDecide(approval, me) ? null : <p className="text-sm">{approval.prompt}</p>}
          {approval.decisions.length > 0 ? (
            <ul data-slot="approval-votes" className="mt-1 grid gap-1 text-xs">
              {approval.decisions.map((d, i) => (
                <li key={i}>
                  {d.by === 'human'
                    ? names.nameOf(d.user_id)
                    : d.by === 'agent'
                      ? `Agent ${d.agent_name}`
                      : `System policy (${d.policy})`}{' '}
                  chose <strong className="font-semibold">{d.option}</strong>
                  {'comment' in d && d.comment ? `: ${d.comment}` : ''}
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-1 text-xs text-fg-muted">
              No votes yet{approval.quorum > 1 ? ` (${approval.quorum} needed)` : ''}.
            </p>
          )}
        </>
      ) : null}

      {/* Only tasks write logs (their actor appends them). */}
      {(section.kind ?? 'task') === 'task' && section.status !== 'pending' ? (
        <div className="mt-4">
          <LogBlock
            lines={logs.lines}
            loading={logs.loading}
            error={logs.error}
            hasMore={logs.hasMore}
            onLoadMore={logs.loadMore}
            onOpen={logs.open}
            onCopyAll={logs.copyAll}
          />
        </div>
      ) : null}

      <StepActions
        executionId={executionId}
        section={section}
        step={step}
        me={me}
        capabilities={caps}
        documentAt={documentAt}
        names={names}
        runPaused={runPaused}
        strictNote={false}
      />
    </div>
  );
}

export function RunStepPanel(props: RunStepPanelProps) {
  const { node, record, run, onClose, callHref } = props;
  return (
    <NodeDetails
      node={node}
      onClose={onClose}
      {...(callHref ? { callHref } : {})}
      {...(record
        ? { headingId: stepHeadingId(record.section.step_run_id), title: record.section.title }
        : {})}
      badge={
        run ? <RunStateChip state={run.state} label={run.stateLabel} verdict={run.verdict} /> : null
      }
      className="absolute inset-y-0 right-0 z-10 sm:w-[380px] md:static md:z-auto"
    >
      {record ? (
        <RunDetails {...props} record={record} />
      ) : (
        <p className="text-sm text-fg-muted">
          {run?.dimmed ? (run.note ?? 'This step did not run.') : 'This step has not started yet.'}
        </p>
      )}
    </NodeDetails>
  );
}
