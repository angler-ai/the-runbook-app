import { useState } from 'react';
import { Button } from '../../ui/primitives/button';
import {
  runnerError,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type Row,
  type RunnerStatus,
} from '../runner/api';
import { panel } from '../runner/presentation';
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
  if (!items.length) return <p className="text-fg-muted">No proposals.</p>;
  return (
    <div className="grid gap-3">
      {items.map((item) => (
        <section key={item.id} className={panel}>
          <h3 className="font-semibold">
            {item.kind === 'agent.change'
              ? item.target_id
                ? 'Change agent'
                : 'Create agent'
              : item.kind === 'credential.change'
                ? 'Change credential'
                : 'Change runner catalog'}{' '}
            · <span className="capitalize">{item.status}</span>
          </h3>
          <p className="my-2 text-sm text-fg-muted">
            {typeof item.proposed_by === 'string' ? item.proposed_by : item.proposed_by?.id}
            {item.sponsor_id && ` · Sponsored by ${item.sponsor_id}`}
            {item.expires_at && ` · Expires ${new Date(item.expires_at).toLocaleString()}`}
          </p>
          {item.reason && <p className="mb-2 text-sm">{item.reason}</p>}
          <Diff
            before={before?.(item)}
            after={Object.fromEntries(
              Object.entries(item.diff).filter(([key]) => key !== 'project_id'),
            )}
          />
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
  if (query.error)
    return (
      <p role="alert" className="text-danger">
        {runnerError(query.error)}
      </p>
    );
  return (
    <ProposalList
      items={(query.data?.items ?? []).filter((item) =>
        project
          ? item.kind === 'agent.change' && item.diff.project_id === project
          : item.kind !== 'agent.change',
      )}
      before={before}
      canDecide={canDecide && status.status === 'online'}
      decide={async (id, action) => {
        await mutation.mutateAsync({ path: `/proposals/${id}/${action}`, method: 'POST' });
      }}
    />
  );
}
