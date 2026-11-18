import { useNavigate } from '@tanstack/react-router';
import { ApiError } from '../../api/client';
import { ErrorState, PageHeader, Skeleton } from '../../ui/layout';
import { InboxList } from '../../ui/lists';
import { useDirectoryNames } from '../context/names';
import { useRunIdentities } from '../execution/runIdentity';
import { useNow } from '../home/hooks';
import { useInbox } from './hooks';

export function TasksPage() {
  const inbox = useInbox();
  const navigate = useNavigate();
  const names = useDirectoryNames();
  // The page's clock, for "started 1d ago" (render stays pure).
  const now = useNow();
  const tasks = inbox.data?.items ?? [];
  const stepOf = new Map(tasks.map((task) => [task.execution_id, task.step_run_id]));
  const runInfo = useRunIdentities(
    tasks.map((task) => task.execution_id),
    names,
    (executionId) => stepOf.get(executionId),
  );
  return (
    <>
      <PageHeader title="My tasks" />
      {inbox.isLoading ? (
        <div aria-busy="true" className="grid gap-2">
          <p role="status" className="sr-only">
            Loading your tasks…
          </p>
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : null}
      {inbox.isError ? (
        <ErrorState
          message="Your tasks could not be loaded."
          code={inbox.error instanceof ApiError ? inbox.error.code : undefined}
          requestId={
            inbox.error instanceof ApiError ? (inbox.error.requestId ?? undefined) : undefined
          }
          onRetry={() => void inbox.refetch()}
        />
      ) : null}
      {inbox.data?.truncated ? (
        <p role="status" className="text-xs text-fg-muted">
          Showing your first {inbox.data.items.length} tasks. Finish some to see the rest.
        </p>
      ) : null}
      {inbox.data ? (
        <InboxList
          items={inbox.data.items}
          runInfo={runInfo}
          now={now}
          onSelectTask={(task) =>
            void navigate({ to: '/x/$exe', params: { exe: task.execution_id } })
          }
        />
      ) : null}
    </>
  );
}
