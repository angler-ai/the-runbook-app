import { useState } from 'react';
import { useQueries } from '@tanstack/react-query';
import { GitPullRequest } from 'lucide-react';
import { EmptyState } from '../../ui/layout';
import { StatusPill } from '../../ui/status/StatusPill';
import { Button } from '../../ui/primitives/button';
import {
  runnerError,
  runnerQueryOptions,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type Row,
  type RunnerStatus,
} from '../runner/api';
import { panel, resourceName, sentenceCase } from '../runner/presentation';
import { Diff } from './AgentVersions';

export interface Proposal {
  id: string;
  kind: string;
  status: string;
  diff: Row;
  target_id?: string | null;
  proposed_by?: { kind?: string; id?: string } | string;
  reason?: string;
  sponsor_id?: string;
  expires_at?: string;
}

function proposalChange(item: Proposal): Row {
  const change = item.kind === 'catalog.change' && item.diff.change;
  if (change && typeof change === 'object' && !Array.isArray(change)) return change as Row;
  return Object.fromEntries(Object.entries(item.diff).filter(([key]) => key !== 'project_id'));
}

function proposalResource(item: Proposal) {
  if (item.kind === 'catalog.change') {
    const resource = item.diff.resource;
    if (resource === 'policy') return '/policy';
    if (
      !item.target_id ||
      !['groups', 'providers', 'profiles', 'mcp-servers'].includes(String(resource))
    )
      return undefined;
    return `/${String(resource)}/${item.target_id}`;
  }
  if (!item.target_id) return undefined;
  if (item.kind === 'credential.change') return `/credentials/${item.target_id}`;
  if (item.kind === 'agent.change' && typeof item.diff.project_id === 'string')
    return `/projects/${item.diff.project_id}/agents/${item.target_id}`;
  return undefined;
}

export function ProposalList({
  items,
  canDecide,
  decide,
  before,
}: {
  items: Proposal[];
  canDecide: boolean;
  decide: (id: string, action: 'approve' | 'reject') => Promise<void>;
  before?: (item: Proposal) => Row | undefined;
}) {
  const [pending, setPending] = useState<string>();
  const [feedback, setFeedback] = useState<{ error: boolean; text: string }>();
  if (!items.length)
    return (
      <EmptyState
        icon={GitPullRequest}
        title="No proposals"
        description="Agent-proposed configuration changes will appear here for review."
      />
    );
  return (
    <div className="grid gap-3">
      {items.map((item) => (
        <section key={item.id} className={panel}>
          <div className="flex flex-wrap items-start justify-between gap-2">
            <div>
              <h3 className="font-semibold">
                {item.kind === 'agent.change'
                  ? item.target_id
                    ? 'Change agent'
                    : 'Create agent'
                  : item.kind === 'credential.change'
                    ? 'Change credential'
                    : 'Change runner catalog'}
              </h3>
              {item.kind === 'catalog.change' && typeof item.diff.resource === 'string' && (
                <p className="mt-1 text-xs text-fg-muted">{resourceName(item.diff.resource)}</p>
              )}
            </div>
            <StatusPill
              status={
                item.status === 'open'
                  ? 'waiting'
                  : item.status === 'approved'
                    ? 'succeeded'
                    : 'cancelled'
              }
              label={sentenceCase(item.status)}
            />
          </div>
          <p className="my-2 text-sm text-fg-muted">
            {typeof item.proposed_by === 'string' ? item.proposed_by : item.proposed_by?.id}
            {item.sponsor_id && ` · Sponsored by ${item.sponsor_id}`}
            {item.expires_at && ` · Expires ${new Date(item.expires_at).toLocaleString()}`}
          </p>
          {item.reason && <p className="mb-2 text-sm">{item.reason}</p>}
          <Diff before={before?.(item)} after={proposalChange(item)} />
          {canDecide && item.status === 'open' && (
            <div className="mt-3 flex gap-2">
              {(['approve', 'reject'] as const).map((action) => (
                <Button
                  key={action}
                  variant={action === 'approve' ? 'primary' : 'secondary'}
                  disabled={Boolean(pending)}
                  onClick={async () => {
                    setPending(item.id);
                    setFeedback(undefined);
                    try {
                      await decide(item.id, action);
                      setFeedback({
                        error: false,
                        text: action === 'approve' ? 'Proposal approved.' : 'Proposal rejected.',
                      });
                    } catch (error) {
                      setFeedback({ error: true, text: runnerError(error) });
                    } finally {
                      setPending(undefined);
                    }
                  }}
                >
                  {action === 'approve' ? 'Approve' : 'Reject'}
                </Button>
              ))}
            </div>
          )}
        </section>
      ))}
      {feedback && (
        <p
          role={feedback.error ? 'alert' : 'status'}
          className={feedback.error ? 'text-danger' : ''}
        >
          {feedback.text}
        </p>
      )}
    </div>
  );
}

export function Proposals({
  status,
  canDecide,
  project,
  before,
}: {
  status: RunnerStatus;
  canDecide: boolean;
  project?: string;
  before?: (item: Proposal) => Row | undefined;
}) {
  const query = useRunnerQuery<Items<Proposal>>(status, '/proposals');
  const mutation = useRunnerMutation(status);
  const items = (query.data?.items ?? []).filter(
    (item) => !project || (item.kind === 'agent.change' && item.diff.project_id === project),
  );
  const paths = [
    ...new Set(
      !before
        ? items.flatMap((item) => {
            const path = item.status === 'open' ? proposalResource(item) : undefined;
            return path ? [path] : [];
          })
        : [],
    ),
  ];
  const baseline = useQueries({
    queries: paths.map((path) => runnerQueryOptions<Row>(status, path)),
  });
  if (query.error)
    return (
      <p role="alert" className="text-danger">
        {runnerError(query.error)}
      </p>
    );
  return (
    <ProposalList
      items={items}
      before={
        before ??
        ((item) => {
          const current = baseline[paths.indexOf(proposalResource(item) ?? '')]?.data;
          if (!current) return undefined;
          const previous =
            item.kind === 'agent.change' && current.config && typeof current.config === 'object'
              ? { ...current.config, ...current }
              : current;
          return Object.fromEntries(
            Object.keys(proposalChange(item)).map((key) => [key, previous[key]]),
          );
        })
      }
      canDecide={canDecide && status.status === 'online'}
      decide={async (id, action) => {
        await mutation.mutateAsync({ path: `/proposals/${id}/${action}`, method: 'POST' });
      }}
    />
  );
}
