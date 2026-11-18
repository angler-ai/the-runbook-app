// UI-3 test fixtures: run documents of the release train (its outline is
// src/ui/graph/fixtures/release-train.outline.json, compiler output) in
// the states the run page draws. Built step by step so each fixture reads
// as what happened in the run.
import type { Schemas } from '../../../api/client';

type Doc = Schemas['ExecutionDocumentView'];
type Section = Schemas['StepSection'];
type Step = Schemas['StepView'];
type Actor = NonNullable<Section['actor']>;

export const RUN_ID = 'exe_01JRUNPAGE0000000000000001';
export const RIVER = 'usr_01JRIVERCHEN0000000000001';

export const releaseAgent: Actor = {
  kind: 'agent',
  name: 'release-agent',
  model: 'claude-sonnet',
  principal_id: 'execution:fam_1',
  session_id: 'sess-1',
};
export const riley: Actor = { kind: 'human', user_id: RIVER };

const KINDS: Record<string, string> = {
  preflight: 'group',
  checks: 'parallel',
  risk_gate: 'switch',
  canary: 'parallel',
  rollout: 'foreach',
  settle: 'loop',
  verify: 'call',
  announce: 'parallel',
  cab_review: 'approval',
  lead_signoff: 'approval',
  customer_go: 'wait',
  halt_preflight: 'set',
  auto_ok: 'set',
  shipped: 'set',
  stop_after_preflight: 'end',
  hold_if_rejected: 'end',
};

const TITLES: Record<string, string> = {
  preflight: 'Preflight',
  checks: 'Preflight checks',
  changelog: 'Changelog and release notes are ready',
  migrations: 'Database migrations are backward compatible',
  freeze: 'No change freeze is active',
  build: 'Build and sign the release artifacts',
  preflight_failed: 'Tell the release channel preflight failed',
  risk_gate: 'Risk gate',
  cab_review: 'Change advisory review',
  lead_signoff: 'Release lead signs off',
  canary: 'Canary',
  canary_us: 'Canary in us-east',
  canary_eu: 'Canary in eu-west',
  rollout: 'Roll out per region',
  verify: 'Post-deploy checks',
  announce: 'Announce',
  status_page: 'Update the status page',
  customer_go: 'Customer success confirms the go-live email went out',
  retro_notes: 'Write the release notes summary',
};

export interface StepInit {
  status?: Section['status'];
  actor?: Actor;
  verdict?: Section['verdict'];
  summary?: string;
  outputs?: Record<string, unknown>;
  evidence?: string[];
  title?: string;
  step?: Partial<Step>;
  section?: Partial<Section>;
}

let counter = 0;
function srId(path: string): string {
  counter += 1;
  const base = path
    .replace(/[^A-Za-z0-9]/g, '')
    .toUpperCase()
    .padEnd(20, 'X')
    .slice(0, 20);
  return `sr_${base}${String(counter).padStart(6, '0')}`;
}

/** One step run: its section and its step view. */
export function stepRun(path: string, init: StepInit = {}): { section: Section; step: Step } {
  const last = path.split('.').pop() ?? path;
  const id = last.replace(/[[#].*$/, '');
  const kind = KINDS[id] ?? 'task';
  const status = init.status ?? 'pending';
  const sr = srId(path);
  const index = /\[([^\]]+)\]/.exec(path)?.[1];
  const region =
    index === undefined ? undefined : (['us-east', 'us-west', 'eu-west'][Number(index)] ?? index);
  const title =
    init.title ??
    (id === 'deploy_region'
      ? `Deploy to ${region}`
      : id === 'settle'
        ? `Wait for ${region} to settle`
        : id === 'region_health'
          ? `Error rate in ${region}`
          : (TITLES[id] ?? id));
  const section: Section = {
    step_run_id: sr,
    path,
    title,
    status,
    kind,
    amended: false,
    amendments: [],
    evidence: init.evidence ?? [],
    outputs: init.outputs ?? {},
    memory_read: [],
    stale: false,
    logs: { href: `/v1/step-runs/${sr}/logs`, next_cursor: null },
    // LANG7: a task's declared actor, as the step view's actor_kind below.
    ...(kind === 'task' ? { task_actor: id === 'status_page' ? 'human' : 'agent' } : {}),
    ...(init.actor ? { actor: init.actor } : {}),
    ...(init.verdict ? { verdict: init.verdict } : {}),
    ...(init.summary ? { summary: init.summary } : {}),
    ...(['succeeded', 'failed', 'skipped', 'cancelled'].includes(status)
      ? { completed_at_ms: 1_794_500_000_000 + counter * 30_000 }
      : {}),
    ...init.section,
  };
  const step: Step = {
    id: sr,
    path,
    status,
    kind,
    schema_revision: 1,
    attempt: 1,
    satisfied: false,
    exhausted: false,
    checklist: [],
    amended: false,
    amendments: [],
    stale: false,
    eligible_recipients: [],
    outputs: init.outputs ?? {},
    verdict: init.verdict ?? null,
    summary: init.summary ?? null,
    actor: init.actor ?? null,
    claim: status === 'claimed' ? { id: `clm_${sr.slice(3)}`, generation: 1 } : null,
    ...(kind === 'task' ? { actor_kind: id === 'status_page' ? 'human' : 'agent' } : {}),
    ...init.step,
  } as Step;
  return { section, step };
}

export function runDoc(
  runs: { section: Section; step: Step }[],
  header: Partial<Schemas['ExecutionView']> = {},
): Doc {
  return {
    header: {
      id: RUN_ID,
      title: 'Release v2026.11.14 (high risk)',
      status: 'waiting',
      phase: 'ordinary',
      amended: false,
      amendments: [],
      fields: { outcome: 'pending' },
      inputs: { version: 'v2026.11.14', risk: 'high', regions: ['us-east', 'us-west', 'eu-west'] },
      project_id: 'prj_1',
      runbook_id: 'rb_release',
      runbook_slug: 'release-train',
      runbook_version: 1,
      runbook_version_id: 'rbv_release1',
      schema_revision: 1,
      stale_step_ids: [],
      tags: [],
      created_by: `user:${RIVER}`,
      created_at_ms: 1_794_499_900_000,
      started_at_ms: 1_794_499_900_000,
      progress: { done: 1, total: 7 },
      steps: runs.map((r) => r.step),
      ...header,
    } as Schemas['ExecutionView'],
    sections: runs.map((r) => r.section),
  };
}

const ok = (path: string, extra: StepInit = {}) =>
  stepRun(path, { status: 'succeeded', actor: releaseAgent, ...extra });

function preflightDone() {
  return [
    ok('preflight', { actor: undefined }),
    ok('checks', { actor: undefined }),
    ok('changelog', { verdict: 'pass', summary: 'Changelog complete for v2026.11.14.' }),
    ok('migrations', {
      verdict: 'pass',
      outputs: { pending: 2 },
      summary: 'Two additive migrations; safe to roll forward and back.',
    }),
    ok('freeze', { verdict: 'pass', summary: 'No freeze and no major incident.' }),
    ok('build', {
      outputs: { digest: 'sha256:5f1c0e2a' },
      summary: 'Images built and signed.',
      evidence: ['https://ci.internal/builds/4812'],
    }),
    stepRun('preflight_failed'),
    stepRun('halt_preflight'),
    stepRun('stop_after_preflight'),
  ];
}

/** The high-risk release waiting on the change advisory review. */
export function waitingOnApproval(): Doc {
  counter = 0;
  const cab = stepRun('cab_review', {
    status: 'waiting',
    section: {
      approval: {
        step_run_id: 'sr_CAB',
        prompt: 'Approve the high-risk release v2026.11.14?',
        options: [
          { id: 'ship', title: 'ship', auto: false },
          { id: 'hold', title: 'hold', auto: false },
        ],
        decisions: [],
        eligible_recipients: [{ kind: 'team', reference: 'team_oncall' }],
        escalated: false,
        escalation_recipients: [],
        quorum: 1,
        schema_revision: 1,
        status: 'waiting',
      },
    },
    step: { eligible_recipients: [{ kind: 'team', reference: 'team_oncall' }] },
  });
  return runDoc([
    ...preflightDone(),
    stepRun('risk_gate', { status: 'ready', step: { branch: { kind: 'case', index: 0 } } }),
    cab,
    stepRun('hold_if_rejected'),
    stepRun('lead_signoff'),
    stepRun('auto_ok'),
    stepRun('canary'),
    stepRun('canary_us'),
    stepRun('canary_eu'),
    stepRun('rollout'),
    stepRun('verify'),
    stepRun('announce'),
    stepRun('status_page'),
    stepRun('customer_go'),
    stepRun('shipped'),
    stepRun('retro_notes'),
  ]);
}

/** A low-risk release part-way through the per-region rollout. */
export function midRollout(): Doc {
  counter = 0;
  return runDoc(
    [
      ...preflightDone(),
      ok('risk_gate', { actor: undefined, step: { branch: { kind: 'default' } } }),
      stepRun('cab_review'),
      stepRun('hold_if_rejected'),
      stepRun('lead_signoff'),
      ok('auto_ok', { actor: undefined }),
      ok('canary', { actor: undefined, verdict: 'pass' }),
      ok('canary_us', { verdict: 'pass', summary: 'Canary matches the baseline.' }),
      stepRun('canary_eu', { status: 'cancelled' }),
      stepRun('rollout', {
        status: 'ready',
        step: { iterations: { started: 2, total: 3, labels: ['us-east', 'us-west', 'eu-west'] } },
      }),
      ok('rollout[0].deploy_region'),
      ok('rollout[0].settle', {
        actor: undefined,
        step: { iterations: { started: 2 } },
      }),
      ok('rollout[0].settle#1.region_health', {
        verdict: 'fail',
        outputs: { error_rate: 0.031 },
        summary: '3.1% 5xx right after the deploy.',
      }),
      ok('rollout[0].settle#2.region_health', {
        verdict: 'pass',
        outputs: { error_rate: 0.004 },
        summary: 'Settled at 0.4%.',
      }),
      ok('rollout[1].deploy_region'),
      stepRun('rollout[1].settle', {
        status: 'ready',
        step: { iterations: { started: 1 } },
      }),
      stepRun('rollout[1].settle#1.region_health', {
        status: 'claimed',
        actor: releaseAgent,
      }),
      stepRun('verify'),
      stepRun('announce'),
      stepRun('status_page'),
      stepRun('customer_go'),
      stepRun('shipped'),
      stepRun('retro_notes'),
    ],
    {
      title: 'Release v2026.11.15 (low risk)',
      status: 'running',
      inputs: { version: 'v2026.11.15', risk: 'low' },
      progress: { done: 4, total: 7 },
    },
  );
}

/** The same release, failed in preflight: the catch ran, the rest never did. */
export function failedInPreflight(): Doc {
  counter = 0;
  return runDoc(
    [
      stepRun('preflight', { status: 'failed' }),
      stepRun('checks', { status: 'failed' }),
      ok('changelog', { verdict: 'pass' }),
      stepRun('migrations', {
        status: 'failed',
        actor: releaseAgent,
        verdict: 'fail',
        summary: 'Migration 0042 drops a column the old release still reads.',
      }),
      ok('freeze', { verdict: 'pass' }),
      stepRun('build', { status: 'skipped', step: { skip_reason: 'condition' } }),
      ok('preflight_failed', { summary: 'Posted the failure to #releases.' }),
      ok('halt_preflight', { actor: undefined }),
      ok('stop_after_preflight', { actor: undefined }),
      stepRun('risk_gate'),
      stepRun('cab_review'),
      stepRun('hold_if_rejected'),
      stepRun('lead_signoff'),
      stepRun('auto_ok'),
      stepRun('canary'),
      stepRun('canary_us'),
      stepRun('canary_eu'),
      stepRun('rollout'),
      stepRun('verify'),
      stepRun('announce'),
      stepRun('status_page'),
      stepRun('customer_go'),
      stepRun('shipped'),
      ok('retro_notes', { summary: 'Preflight failed on migration 0042; nothing shipped.' }),
    ],
    {
      status: 'failed',
      completed_at_ms: 1_794_500_152_000,
      started_at_ms: 1_794_499_900_000,
      progress: { done: 1, total: 7 },
    },
  );
}
