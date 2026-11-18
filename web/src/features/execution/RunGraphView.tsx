// UI-3: the run's live execution graph, the run page's default view. The
// runbook's own graph (UI-2's component, laid out from the run's version)
// with the run's state over it. Live updates arrive as new document reads
// (live.ts); the layout depends on the outline only, so cards change state
// in place without a reload or a re-layout.
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { api, ApiError, type Schemas } from '../../api/client';
import type { MeView } from '../../api/hooks';
import { queryKeys } from '../../api/query-keys';
import { RunbookGraph } from '../../ui/graph';
import type { Outline, OutlineNode } from '../../ui/types';
import { EmptyState, ErrorState, Skeleton } from '../../ui/layout';
import { useCompiler, type CompilerBackend } from '../compile/useCompiler';
import type { DirectoryNames } from '../context/names';
import type { Capabilities } from './gating';
import { buildOverlay, type RunDocument, type RunRecord, type Selection } from './overlay';
import { RunStepPanel } from './RunStepPanel';

export type RunOutlineState =
  | { kind: 'loading' }
  | { kind: 'ready'; outline: Outline }
  /** A preview version has no number to read its source by. */
  | { kind: 'preview' }
  /** The person may read the run but not its runbook. */
  | { kind: 'forbidden' }
  | { kind: 'error'; message: string; code?: string };

/** The run's version's outline, compiled in the browser (UI-2's compiler). */
export function useRunOutline(
  header: Schemas['ExecutionView'],
  compiler?: CompilerBackend,
): RunOutlineState {
  const runbookId = header.runbook_id ?? null;
  const version = header.runbook_version;
  const source = useQuery({
    // The runbook page's key: the two pages share the version's source.
    queryKey: [
      'runbooks',
      runbookId,
      'versions',
      version == null ? null : String(version),
      'source',
    ],
    enabled: Boolean(runbookId) && version != null,
    staleTime: Infinity,
    retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 2,
    queryFn: async () => {
      const { data } = await api.GET('/v1/runbooks/{rb}/versions/{version}', {
        params: { path: { rb: runbookId ?? '', version: String(version) } },
      });
      return data as { source: string };
    },
  });
  const text = source.data?.source ?? '';
  const compiled = useCompiler(text, { enabled: Boolean(text), backend: compiler });
  if (version == null || !runbookId) return { kind: 'preview' };
  if (
    source.error instanceof ApiError &&
    (source.error.status === 403 || source.error.status === 404)
  )
    return { kind: 'forbidden' };
  if (source.error) {
    return {
      kind: 'error',
      message: 'The runbook version could not be read.',
      ...(source.error instanceof ApiError ? { code: source.error.code } : {}),
    };
  }
  if (compiled.error) return { kind: 'error', message: compiled.error };
  const outline = compiled.outline?.outline;
  if (outline && compiled.source === text) return { kind: 'ready', outline };
  return { kind: 'loading' };
}

const CHILD_DONE = new Set(['succeeded', 'failed', 'cancelled']);

/**
 * The status of the child runs on screen: one execution header read
 * (`GET /v1/executions/{child}`) per call card in view, that is the
 * iteration each foreach or loop shows, not one per iteration. Each parent
 * refresh reads an unfinished child again (a finished one is final), so a
 * card does not go stale while its call step changes.
 */
function useChildStatuses(
  records: ReadonlyMap<string, RunRecord>,
  documentAt: number,
): ReadonlyMap<string, string> {
  const queryClient = useQueryClient();
  const key = [...records.values()]
    .map((record) => (record.step?.call?.started ? record.step.call.child_execution_id : null))
    .filter((id): id is string => Boolean(id))
    .join(',');
  const ids = useMemo(() => (key ? key.split(',') : []), [key]);
  const results = useQueries({
    queries: ids.map((id) => ({
      queryKey: queryKeys.execution(id),
      staleTime: 10_000,
      queryFn: async () => {
        const { data } = await api.GET('/v1/executions/{exe}', { params: { path: { exe: id } } });
        return data as Schemas['ExecutionView'];
      },
    })),
  });
  useEffect(() => {
    for (const id of ids) {
      const status = queryClient.getQueryData<Schemas['ExecutionView']>(
        queryKeys.execution(id),
      )?.status;
      if (status && !CHILD_DONE.has(status)) {
        void queryClient.invalidateQueries({ queryKey: queryKeys.execution(id), exact: true });
      }
    }
    // Only a new parent read should trigger this, not a new pick of iteration.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [documentAt]);
  return useMemo(() => {
    const map = new Map<string, string>();
    results.forEach((result, i) => {
      const id = ids[i];
      if (id && result.data) map.set(id, result.data.status);
    });
    return map;
  }, [results, ids]);
}

const STATUS_WORDS: Record<string, string> = {
  queued: 'queued',
  running: 'running',
  waiting: 'waiting',
  paused: 'paused',
  succeeded: 'succeeded',
  failed: 'failed',
  cancelled: 'cancelled',
};

export interface RunGraphViewProps {
  executionId: string;
  doc: RunDocument;
  outline: Outline;
  me: MeView | undefined;
  caps: Capabilities;
  documentAt: number;
  names: DirectoryNames;
  /** Where a call step's child runbook lives. */
  callHref?: (slug: string) => string | undefined;
  className?: string;
}

/** The graph with the run's state over it (the outline already compiled). */
export function RunGraph({
  executionId,
  doc,
  outline,
  me,
  caps,
  documentAt,
  names,
  callHref,
  className,
}: RunGraphViewProps) {
  const [selection, setSelection] = useState<Selection>({});
  const [selected, setSelected] = useState<string | undefined>(undefined);
  const navigate = useNavigate();
  const follow = useCallback((href: string) => void navigate({ href }), [navigate]);
  // Iterations held for an open panel (not picked by the person): released
  // when the panel closes, so the graph follows the run again.
  const [held, setHeld] = useState<readonly string[]>([]);
  const pickIteration = useCallback((containerId: string, key: string) => {
    setHeld((prev) => prev.filter((id) => id !== containerId));
    setSelection((prev) => ({ ...prev, [containerId]: key }));
  }, []);
  const built = useMemo(
    () => buildOverlay(outline, doc, { selection, names }),
    [outline, doc, selection, names],
  );
  // The panel stays on the step run the person opened: opening a step holds
  // the iterations it runs in, so a new loop attempt or foreach item does
  // not swap the panel (or the cards around it) under the reader.
  const select = useCallback(
    (id: string | undefined) => {
      setSelected(id);
      if (id === undefined) {
        if (held.length > 0) {
          setSelection((prev) => {
            const next = { ...prev };
            for (const container of held) delete next[container];
            return next;
          });
          setHeld([]);
        }
        return;
      }
      // Hold the iteration every foreach and loop shows now, not only the
      // opened step's own scope: a step with no record yet has no scope,
      // and the next iteration starting would swap it (UI-3 re-check).
      const shownNow = new Map<string, string>();
      for (const entry of built.records.get(id)?.scope ?? [])
        shownNow.set(entry.container, entry.key);
      for (const [container, node] of built.nodes) {
        const it = node.iteration;
        if (it && !shownNow.has(container)) {
          const key = it.keys[it.index];
          if (key !== undefined) shownNow.set(container, key);
        }
      }
      const hold = [...shownNow]
        .map(([container, key]) => ({ container, key }))
        .filter((entry) => selection[entry.container] === undefined);
      if (hold.length === 0) return;
      setSelection((prev) => ({
        ...prev,
        ...Object.fromEntries(hold.map((entry) => [entry.container, entry.key])),
      }));
      setHeld((prev) => [...prev, ...hold.map((entry) => entry.container)]);
    },
    [built, held, selection],
  );
  const children = useChildStatuses(built.records, documentAt);
  const overlay = useMemo(() => {
    const nodes = new Map(built.nodes);
    for (const [id, run] of nodes) {
      if (!run.child) continue;
      const child = built.records.get(id)?.step?.call?.child_execution_id;
      const status = child ? children.get(child) : undefined;
      if (status)
        nodes.set(id, {
          ...run,
          child: { ...run.child, label: `Child run · ${STATUS_WORDS[status] ?? status}` },
        });
    }
    return { ...built, nodes, pickIteration };
  }, [built, children, pickIteration]);

  const renderDetails = useCallback(
    (node: OutlineNode, close: () => void) => {
      const record = overlay.records.get(node.id);
      const child = record?.step?.call?.child_execution_id;
      const childStatus = child ? children.get(child) : undefined;
      return (
        <RunStepPanel
          key={record?.section.step_run_id ?? node.id}
          executionId={executionId}
          node={node}
          record={record}
          run={overlay.nodes.get(node.id)}
          me={me}
          caps={caps}
          documentAt={documentAt}
          names={names}
          runPaused={doc.header.status === 'paused'}
          onClose={close}
          {...(callHref ? { callHref } : {})}
          {...(childStatus ? { childStatus } : {})}
        />
      );
    },
    [overlay, children, executionId, me, caps, documentAt, names, callHref, doc.header.status],
  );

  return (
    <RunbookGraph
      outline={outline}
      overlay={overlay}
      {...(overlay.currentId ? { focusId: overlay.currentId } : {})}
      selectedId={selected}
      onSelect={select}
      renderDetails={renderDetails}
      onNavigate={follow}
      viewKey={`run:${executionId}`}
      {...(callHref ? { callHref } : {})}
      className={className ?? 'h-[min(74vh,720px)]'}
      fillViewport={className == null}
    />
  );
}

export function RunGraphView(
  props: Omit<RunGraphViewProps, 'outline'> & {
    compiler?: CompilerBackend;
    onShowDocument: () => void;
  },
) {
  const { compiler, onShowDocument, ...rest } = props;
  const state = useRunOutline(props.doc.header, compiler);
  if (state.kind === 'loading') {
    return (
      <div aria-busy="true">
        <p role="status" className="sr-only">
          Drawing the graph…
        </p>
        <Skeleton className="h-[min(74vh,720px)] w-full" />
      </div>
    );
  }
  if (state.kind === 'ready') return <RunGraph {...rest} outline={state.outline} />;
  const action = (
    <button
      type="button"
      onClick={onShowDocument}
      className="text-sm font-medium text-accent underline-offset-2 hover:underline"
    >
      Read the run as a document
    </button>
  );
  if (state.kind === 'preview') {
    return (
      <EmptyState
        title="No graph for a preview run"
        description="This run used an unpublished preview of the runbook, so there is no version to draw."
        action={action}
      />
    );
  }
  if (state.kind === 'forbidden') {
    return (
      <EmptyState
        title="The runbook is not visible to you"
        description="You can see this run, but not its runbook's source, so the graph cannot be drawn."
        action={action}
      />
    );
  }
  return (
    <ErrorState
      title="The graph could not be drawn"
      message={state.message}
      {...(state.code ? { code: state.code } : {})}
    />
  );
}
