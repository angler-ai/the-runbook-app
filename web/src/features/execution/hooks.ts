// Execution reads and actor mutations (M2). Every mutation names its
// intent as an action id, so a retry of the same action reuses one
// idempotency key (D20, see actions.ts), and invalidates the document on
// settle; the server stays the authority on every gate.
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useRef, useState } from 'react';
import { api, type Schemas } from '../../api/client';
import { fetchAllPages, useMe } from '../../api/hooks';
import { queryKeys } from '../../api/query-keys';
import { invalidateInbox } from '../inbox/hooks';
import { actionId, runAction } from './actions';
import { tabClaims, tabSessionId, type ClaimRef } from './session';

export type ExecutionDocument = Schemas['ExecutionDocumentView'];
export type StepSectionData = Schemas['StepSection'];
export type StepViewData = Schemas['StepView'];
export type ClaimedStepRead = Schemas['ClaimedStepRead'];
export type StepLogPage = Schemas['StepLogPage'];

export const stepReadKey = (stepRunId: string) => ['step-runs', stepRunId, 'read'] as const;
export const stepLogsKey = (stepRunId: string) => ['step-runs', stepRunId, 'logs'] as const;

const keyHeader = (key: string) => ({ 'Idempotency-Key': key });

export function useInvalidateExecution(executionId: string) {
  const queryClient = useQueryClient();
  return () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.executionDocument(executionId) });
    void queryClient.invalidateQueries({ queryKey: queryKeys.execution(executionId) });
    void queryClient.invalidateQueries({ queryKey: queryKeys.executionEvidence(executionId) });
    // Every action here (decide, claim, report, release, ...) can open or
    // close one of this person's tasks: the sidebar count follows (#10).
    invalidateInbox(queryClient);
  };
}

/**
 * The signed-in person's role in a project (GET /v1/projects/{p}/members,
 * a D37 directory read the browser token may make): the role, null when
 * not a member, undefined while unknown (loading, refused, no user). An org
 * owner or admin is admin of every project, as on the server.
 */
export function useMyProjectRole(
  projectId: string | null | undefined,
  userId: string | null | undefined,
): Schemas['ProjectRole'] | null | undefined {
  const me = useMe();
  const members = useQuery({
    queryKey: ['projects', projectId ?? '', 'members'] as const,
    enabled: Boolean(projectId && userId),
    staleTime: 60_000,
    queryFn: () =>
      fetchAllPages(async (cursor) => {
        const { data } = await api.GET('/v1/projects/{p}/members', {
          params: {
            path: { p: projectId ?? '' },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as Schemas['DirectoryPage_ProjectMemberRequest'];
      }),
  });
  // An org owner or admin is admin of every project, granted or not.
  const orgRole = me.data?.org.role;
  if (userId && (orgRole === 'owner' || orgRole === 'admin')) return 'admin';
  if (!members.data || !userId) return undefined;
  return members.data.find((member) => member.user_id === userId)?.role ?? null;
}

export function useStepLogs(stepRunId: string, enabled: boolean) {
  return useInfiniteQuery({
    queryKey: stepLogsKey(stepRunId),
    enabled,
    initialPageParam: '0',
    queryFn: async ({ pageParam }) => {
      const { data } = await api.GET('/v1/step-runs/{sr}/logs', {
        params: { path: { sr: stepRunId }, query: { cursor: pageParam, limit: 200 } },
      });
      return data as StepLogPage;
    },
    getNextPageParam: (last) => (last.has_more ? last.next_cursor : undefined),
  });
}

export function useClaimStep(executionId: string) {
  const queryClient = useQueryClient();
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: async ({ stepRunId }: { stepRunId: string }) => {
      const session = tabSessionId();
      const claim = await runAction(actionId('claim', { stepRunId, session }), async (key) => {
        const { data } = await api.POST('/v1/step-runs/{sr}/claim', {
          params: { path: { sr: stepRunId }, header: keyHeader(key) },
          body: { session_id: session, actor: { kind: 'human' } },
        });
        return data as ClaimRef;
      });
      tabClaims.set(stepRunId, claim);
      return claim;
    },
    onSettled: (_data, _error, variables) => {
      invalidate();
      void queryClient.invalidateQueries({ queryKey: stepReadKey(variables.stepRunId) });
    },
  });
}

const humanWork = (brief: Schemas['ResumeBrief']): ReadonlySet<string> =>
  new Set(brief.ready_work.filter((work) => work.actor_kind === 'human').map((w) => w.step_run_id));

/**
 * Step runs a person may claim right now (R55). With a person's token the
 * resume brief is the human audience's: ready tasks whose actor is human or
 * any, and tasks handed off to a person (human continuation). The document
 * carries neither the declared actor nor the handoff, so this is the
 * server's own answer. Its key sits under ['executions', exe], so every
 * mutation and live event that refreshes the document refreshes it too.
 */
export function useHumanReadyWork(executionId: string, enabled: boolean) {
  return useQuery({
    queryKey: queryKeys.executionResumeBrief(executionId),
    enabled,
    queryFn: async () => {
      const { data } = await api.GET('/v1/executions/{exe}/next', {
        params: { path: { exe: executionId } },
      });
      return data as Schemas['ResumeBrief'];
    },
    select: humanWork,
  });
}

// Enabled only once this tab holds the claim, so the claim POST always
// precedes this read (D32/F60).
export function useStepRead(stepRunId: string, enabled: boolean) {
  return useQuery({
    queryKey: stepReadKey(stepRunId),
    enabled,
    queryFn: async () => {
      const { data } = await api.GET('/v1/step-runs/{sr}', {
        params: { path: { sr: stepRunId } },
      });
      return data as ClaimedStepRead;
    },
  });
}

type ChecklistTick = Schemas['ChecklistTick'];

export interface ChecklistQueue {
  /** This tab's ticks while it holds the claim; null means "use the server copy". */
  ticks: ChecklistTick[] | null;
  /** True while a save is in flight or queued. */
  saving: boolean;
  toggle(
    base: ChecklistTick[],
    index: number,
    completed: boolean,
    claim: ClaimRef,
    onError: (error: unknown) => void,
  ): void;
  /** Forget local ticks (the claim ended or changed). */
  reset(): void;
}

/**
 * Checklist saves for one step, one request at a time (R53). A toggle
 * edits the local list, never a list rebuilt from a possibly stale server
 * copy, and the queue always sends the latest local list in full. So a
 * response or refetch from an earlier save cannot undo a newer tick. The
 * server stays the authority: a refusal drops the local list (and anything
 * queued behind it) and refetches, and the server copy shows again.
 */
export function useChecklistQueue(executionId: string, stepRunId: string): ChecklistQueue {
  const invalidate = useInvalidateExecution(executionId);
  const [ticks, setTicks] = useState<ChecklistTick[] | null>(null);
  const [saving, setSaving] = useState(false);
  // Mutable queue state: clicks can arrive between renders.
  const queue = useRef<{ latest: ChecklistTick[] | null; running: boolean; epoch: number }>({
    latest: null,
    running: false,
    epoch: 0,
  });

  async function drain(claim: ClaimRef, onError: (error: unknown) => void) {
    const q = queue.current;
    const epoch = q.epoch;
    q.running = true;
    setSaving(true);
    let sent: ChecklistTick[] | null = null;
    try {
      while (q.epoch === epoch && q.latest && q.latest !== sent) {
        const checklist: ChecklistTick[] = q.latest;
        sent = checklist;
        // Re-ticking to the same list after a lost response is a retry.
        await runAction(actionId('checklist', { stepRunId, claim, checklist }), (key) =>
          api.POST('/v1/step-runs/{sr}/checklist', {
            params: { path: { sr: stepRunId }, header: keyHeader(key) },
            body: { claim, checklist },
          }),
        );
      }
    } catch (error) {
      if (q.epoch === epoch) {
        q.latest = null;
        setTicks(null);
        onError(error);
      }
    } finally {
      if (q.epoch === epoch) {
        q.running = false;
        setSaving(false);
      }
      invalidate();
    }
  }

  return {
    ticks,
    saving,
    toggle(base, index, completed, claim, onError) {
      const q = queue.current;
      const next = (q.latest ?? base).map((tick, i) =>
        i === index ? { ...tick, completed } : tick,
      );
      q.latest = next;
      setTicks(next);
      if (!q.running) void drain(claim, onError);
    },
    reset() {
      const q = queue.current;
      q.epoch += 1;
      q.latest = null;
      q.running = false;
      setTicks(null);
      setSaving(false);
    },
  };
}

export interface ReportInput {
  stepRunId: string;
  claim: ClaimRef;
  status: Schemas['ReportStatus'];
  summary?: string;
  outputs?: Record<string, unknown>;
  verdict?: Schemas['Verdict'];
  evidence?: string[];
}

export function useReportStep(executionId: string) {
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: async (input: ReportInput) => {
      const body = {
        step_run_id: input.stepRunId,
        claim: input.claim,
        status: input.status,
        summary: input.summary ?? null,
        outputs: input.outputs ?? {},
        verdict: input.verdict ?? null,
        evidence: input.evidence ?? [],
        logs: [],
      };
      await runAction(actionId('report', body), (key) =>
        api.POST('/v1/step-runs/{sr}/report', {
          params: { path: { sr: input.stepRunId }, header: keyHeader(key) },
          body,
        }),
      );
      tabClaims.clear(input.stepRunId);
    },
    onSettled: invalidate,
  });
}

export function useExecutionEvidence(executionId: string | undefined, enabled = true) {
  return useQuery({
    queryKey: queryKeys.executionEvidence(executionId ?? ''),
    enabled: Boolean(executionId) && enabled,
    queryFn: async () => {
      try {
        const { data } = await api.GET('/v1/executions/{exe}/evidence', {
          params: { path: { exe: executionId! } },
        });
        return (data as Schemas['EvidencePage'])?.items ?? [];
      } catch {
        return [];
      }
    },
    staleTime: 30_000,
  });
}

export function useReleaseStep(executionId: string) {
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: async ({ stepRunId, claim }: { stepRunId: string; claim: ClaimRef }) => {
      await runAction(actionId('release', { stepRunId, claim }), (key) =>
        api.POST('/v1/step-runs/{sr}/release', {
          params: { path: { sr: stepRunId }, header: keyHeader(key) },
          body: { claim },
        }),
      );
      tabClaims.clear(stepRunId);
    },
    onSettled: invalidate,
  });
}

export function useDecide(executionId: string) {
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: async ({
      stepRunId,
      option,
      comment,
    }: {
      stepRunId: string;
      option: string;
      comment?: string;
    }) => {
      const body = {
        option,
        comment: comment ?? null,
        session_id: tabSessionId(),
        actor: { kind: 'human' as const },
      };
      return runAction(actionId('decide', { stepRunId, ...body }), async (key) => {
        const { data } = await api.POST('/v1/step-runs/{sr}/decisions', {
          params: { path: { sr: stepRunId }, header: keyHeader(key) },
          body,
        });
        return data;
      });
    },
    onSettled: invalidate,
  });
}

export function useSignal(executionId: string) {
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: async ({ name }: { name: string }) => {
      await runAction(actionId('signal', { executionId, name }), (key) =>
        api.POST('/v1/executions/{exe}/signals/{name}', {
          params: { path: { exe: executionId, name }, header: keyHeader(key) },
          body: { payload: {} },
        }),
      );
    },
    onSettled: invalidate,
  });
}

export type ControlAction = 'pause' | 'resume' | 'cancel';
/**
 * Refusals that, on a retry, mean the earlier attempt already took effect.
 * Pause and resume are no-ops in the state they ask for (control.rs); a
 * cancel that landed leaves the run closed.
 */
const ALREADY: Record<ControlAction, string[]> = {
  pause: [],
  resume: [],
  cancel: ['execution.closed'],
};

export function useExecutionControl(executionId: string) {
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: async (action: ControlAction) => {
      // PATCH has no Idempotency-Key (D27 keeps it idempotent by state), so
      // a retry after a lost response may find the run already there.
      await runAction(
        actionId('control', { executionId, action }),
        () =>
          api.PATCH('/v1/executions/{exe}', {
            params: { path: { exe: executionId } },
            body: { action },
          }),
        { landedEarlier: (error) => ALREADY[action].includes(error.code) },
      );
    },
    onSettled: invalidate,
  });
}

export function useRerun(executionId: string) {
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: () =>
      runAction(actionId('rerun', { executionId }), async (key) => {
        const { data } = await api.POST('/v1/executions/{exe}/rerun', {
          params: { path: { exe: executionId }, header: keyHeader(key) },
          body: {},
        });
        return data as Schemas['ExecutionView'];
      }),
    onSettled: invalidate,
  });
}

/** Fetch an execution report in the given format and save it as a download. */
export async function downloadReport(
  executionId: string,
  title: string,
  format: 'markdown' | 'junit' | 'pdf',
): Promise<void> {
  let blob: Blob;
  let extension: string;

  if (format === 'markdown') {
    const { data } = await api.GET('/v1/executions/{exe}/report', {
      params: { path: { exe: executionId }, query: { format: 'markdown' } },
      parseAs: 'text',
    });
    blob = new Blob([String(data ?? '')], { type: 'text/markdown' });
    extension = '.md';
  } else {
    const { data } = await api.GET('/v1/executions/{exe}/report', {
      params: { path: { exe: executionId }, query: { format } },
      parseAs: 'blob',
    });
    blob = (data as unknown as Blob) ?? new Blob([]);
    extension = format === 'junit' ? '.xml' : '.pdf';
  }

  const url = URL.createObjectURL(blob);
  try {
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `${title.replace(/[^a-z0-9-_]+/gi, '-').toLowerCase() || executionId}${extension}`;
    anchor.click();
  } finally {
    // Safari reads the URL after click() returns; revoking in the same
    // tick can cancel the download (U18).
    setTimeout(() => URL.revokeObjectURL(url), REVOKE_AFTER_MS);
  }
}

const REVOKE_AFTER_MS = 1_000;
