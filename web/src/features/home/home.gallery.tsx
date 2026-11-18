// UI-2: the Home page with data, an empty project, and the waiting-on
// badges. The page reads through React Query, so each example gets its own
// client seeded with the reads Home makes (nothing is fetched on mount).
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from '@tanstack/react-router';
import { useState, type ReactNode } from 'react';
import { defineGallery } from '../../gallery/registry';
import { queryKeys } from '../../api/query-keys';
import { OrgProvider } from '../context/OrgContext';
import { HomePage, WaitBadge } from './HomePage';
import { HOME_WINDOW_DAYS } from './hooks';
import { OPEN_STATUSES, windowStart } from './model';

const NOW = Date.parse('2026-11-14T16:30:00Z');
const MIN = 60_000;
const PROJECT = { id: 'prj_gallery', org_id: 'org_gallery', slug: 'ops', title: 'Ops' };
const EMPTY_PROJECT = { id: 'prj_empty', org_id: 'org_gallery', slug: 'new', title: 'New project' };

const run = (
  id: string,
  title: string,
  status: string,
  ago: number,
  took: number | null,
  runbook = 'rb_release',
) => ({
  id,
  runbook_id: runbook,
  status,
  title,
  tags: [],
  fields: {},
  created_by: 'user:usr_gallery',
  created_at_ms: NOW - ago * MIN,
  completed_at_ms: took == null ? null : NOW - ago * MIN + took * MIN,
});

const RELEASE = run('exe_release', 'Release v2026.11.14 (high risk)', 'waiting', 48, null);
const ONBOARD = run('exe_onboard', 'Onboard Northwind', 'waiting', 190, null, 'rb_onboard');
const CHECKS = run('exe_checks', 'Post-deploy checks for api', 'running', 3, null, 'rb_checks');
const FAILED = run('exe_failed', 'Post-deploy checks for billing', 'failed', 130, 6, 'rb_checks');
const OK = run('exe_ok', 'Post-deploy checks for web', 'succeeded', 75, 4, 'rb_checks');

function step(id: string, path: string, status: string, kind: string, extra: object = {}) {
  return {
    schema_revision: 1,
    id,
    path,
    status,
    kind,
    verdict: null,
    outputs: {},
    summary: null,
    actor: null,
    claim: null,
    attempt: 1,
    satisfied: false,
    exhausted: false,
    checklist: [],
    amended: false,
    amendments: [],
    stale: false,
    eligible_recipients: [],
    ...extra,
  };
}

function document(
  r: ReturnType<typeof run>,
  done: number,
  total: number,
  steps: ReturnType<typeof step>[],
  titles: Record<string, string>,
  summaries: Record<string, string> = {},
) {
  return {
    header: {
      schema_revision: 1,
      id: r.id,
      runbook_id: r.runbook_id,
      runbook_version_id: 'rbv_gallery',
      runbook_version: 1,
      title: r.title,
      status: r.status,
      phase: 'ordinary',
      result: null,
      pending_terminal_status: null,
      inputs: {},
      fields: {},
      tags: [],
      steps,
      amended: false,
      amendments: [],
      stale_step_ids: [],
      rerun_of: null,
      resume_brief: null,
      created_at_ms: r.created_at_ms,
      started_at_ms: r.created_at_ms,
      progress: { done, total },
    },
    sections: steps.map((s) => ({
      step_run_id: s.id,
      path: s.path,
      title: titles[s.id] ?? s.path,
      status: s.status,
      kind: s.kind,
      verdict: null,
      actor: null,
      summary: summaries[s.id] ?? null,
      outputs: {},
      evidence: [],
      approval: null,
      memory_read: [],
      amended: false,
      amendments: [],
      stale: false,
      logs: { href: '', next_cursor: null },
    })),
  };
}

const RUNBOOKS = [
  ['rb_checks', 'post-deploy-checks', 'Post-deploy checks', 'failed'],
  ['rb_release', 'release-train', 'Production release train', 'waiting'],
  ['rb_onboard', 'customer-onboarding', 'Customer onboarding', 'waiting'],
  ['rb_certs', 'cert-rotation', 'Certificate rotation', 'succeeded'],
  ['rb_drill', 'db-failover-drill', 'Database failover drill', null],
].map(([runbook_id, slug, title, status]) => ({
  runbook_id,
  slug,
  title,
  origin: 'ui',
  archived: false,
  tags: [],
  latest_version: 1,
  draft_ahead: false,
  active_runs: status === 'waiting' ? 1 : 0,
  ...(status
    ? {
        last_run: {
          id: `exe_${slug}`,
          status,
          created_at_ms: NOW - 60 * MIN,
          completed_at_ms: null,
        },
      }
    : {}),
}));

function seeded(project: typeof PROJECT, withData: boolean): QueryClient {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false, refetchInterval: false } },
  });
  const set = (key: readonly unknown[], data: unknown) => client.setQueryData(key, data);
  set(queryKeys.me, {
    user: { id: 'usr_gallery', name: 'Jordan Rivera' },
    org: { id: 'org_gallery', title: 'Acme', role: 'owner' },
    teams: [],
    credential: {
      kind: 'personal',
      actor_kind: 'human',
      scopes: ['runbooks:read', 'runbooks:write', 'executions:read', 'executions:start'],
      projects: null,
      expires_at_ms: null,
    },
  });
  set(queryKeys.orgs, [{ id: 'org_gallery', title: 'Acme' }]);
  set(queryKeys.projects('org_gallery'), [PROJECT, EMPTY_PROJECT]);
  set(queryKeys.directoryUsers('org_gallery'), [
    { id: 'usr_gallery', name: 'Jordan Rivera', active: true, role: 'owner' },
    { id: 'usr_sam', name: 'Sam Okafor', active: true, role: 'member' },
  ]);
  set(queryKeys.directoryTeams('org_gallery'), [
    { id: 'team_cs', org_id: 'org_gallery', slug: 'customer-success', title: 'Customer success' },
    { id: 'team_oncall', org_id: 'org_gallery', slug: 'oncall', title: 'On call' },
  ]);
  set(queryKeys.myTasks(), {
    items: withData
      ? [
          {
            project_id: project.id,
            execution_id: RELEASE.id,
            step_run_id: 'sr_cab',
            path: 'cab_review',
            title: 'Change advisory review',
            execution_title: RELEASE.title,
            kind: 'approval',
            since_ms: NOW - 40 * MIN,
          },
          {
            project_id: project.id,
            execution_id: ONBOARD.id,
            step_run_id: 'sr_kick',
            path: 'kickoff_call',
            title: 'Hold the kickoff call with Northwind',
            execution_title: ONBOARD.title,
            kind: 'human_task',
            since_ms: NOW - 26 * 60 * MIN,
          },
        ]
      : [],
    next_cursor: null,
    truncated: false,
  });
  set(queryKeys.runbooks(project.id), withData ? RUNBOOKS : []);
  if (!withData) return client;
  set(['projects', project.id, 'home', 'runs', OPEN_STATUSES.join(',')], {
    items: [CHECKS, RELEASE, ONBOARD],
    next_cursor: null,
  });
  set(['projects', project.id, 'home', 'runs', 'failed'], { items: [FAILED], next_cursor: null });
  set(['projects', project.id, 'home', 'recent', windowStart(NOW, HOME_WINDOW_DAYS)], {
    entries: [
      CHECKS,
      OK,
      FAILED,
      RELEASE,
      ONBOARD,
      run('exe_old1', 'Post-deploy checks for api', 'succeeded', 26 * 60, 5, 'rb_checks'),
      run('exe_old2', 'Rotate certificates', 'succeeded', 3 * 24 * 60, 12, 'rb_certs'),
      run('exe_old3', 'Post-deploy checks for web', 'succeeded', 4 * 24 * 60, 4, 'rb_checks'),
    ],
    complete: true,
    fromMs: windowStart(NOW, HOME_WINDOW_DAYS),
  });
  set(
    queryKeys.executionDocument(CHECKS.id),
    document(
      CHECKS,
      1,
      4,
      [
        step('sr_c1', 'health', 'claimed', 'task', {
          actor: { kind: 'agent', name: 'claude', principal_id: 'p', session_id: 's' },
        }),
      ],
      {},
    ),
  );
  set(
    queryKeys.executionDocument(RELEASE.id),
    document(
      RELEASE,
      3,
      8,
      [
        step('sr_cab', 'cab_review', 'waiting', 'approval', {
          eligible_recipients: [{ kind: 'team', reference: 'team_oncall' }],
        }),
      ],
      {},
    ),
  );
  set(
    queryKeys.executionDocument(ONBOARD.id),
    document(
      ONBOARD,
      2,
      6,
      [
        step('sr_kick', 'kickoff_call', 'ready', 'task', {
          eligible_recipients: [{ kind: 'user', reference: 'usr_sam' }],
        }),
        step('sr_sync', 'first_sync', 'waiting', 'wait', {
          wait: { signal: 'customer_first_sync' },
        }),
      ],
      {},
    ),
  );
  set(
    queryKeys.executionDocument(FAILED.id),
    document(
      FAILED,
      2,
      4,
      [step('sr_f', 'error_rate', 'failed', 'task')],
      { sr_f: 'Error rate in eu-west' },
      { sr_f: '5xx rate 4.2% over 15 minutes, above the 1% threshold.' },
    ),
  );
  return client;
}

function Seeded({
  project,
  withData,
  children,
}: {
  project: typeof PROJECT;
  withData: boolean;
  children: ReactNode;
}) {
  const [client] = useState(() => seeded(project, withData));
  // Its own memory router, so links render (and stay put) inside or outside the app.
  const [router] = useState(() =>
    createRouter({
      routeTree: createRootRoute({ component: () => <>{children}</> }),
      history: createMemoryHistory({ initialEntries: [`/p/${project.slug}`] }),
    }),
  );
  return (
    <QueryClientProvider client={client}>
      <OrgProvider>
        <RouterProvider router={router} />
      </OrgProvider>
    </QueryClientProvider>
  );
}

export default [
  defineGallery({
    id: 'home-page',
    group: 'Home',
    title: 'HomePage',
    examples: [
      {
        name: 'With data',
        render: () => (
          <Seeded project={PROJECT} withData>
            <HomePage projectRef="ops" now={NOW} />
          </Seeded>
        ),
      },
      {
        name: 'Empty project: getting started',
        render: () => (
          <Seeded project={EMPTY_PROJECT} withData={false}>
            <HomePage projectRef="new" now={NOW} />
          </Seeded>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'home-wait-badges',
    group: 'Home',
    title: 'WaitBadge',
    examples: [
      {
        name: 'What a run waits on',
        render: () => (
          <div className="flex flex-wrap gap-2">
            <WaitBadge kind="agent" label="Agent · claude" />
            <WaitBadge kind="person" label="Sam Okafor" />
            <WaitBadge kind="approval" label="Approval · team oncall" />
            <WaitBadge kind="signal" label="Signal · release_email_sent" />
            <WaitBadge kind="timer" label="Timer · 17:45" />
            <WaitBadge kind="child" label="Child run" />
            <WaitBadge kind="queued" label="Queued" />
            <WaitBadge kind="paused" label="Paused" />
          </div>
        ),
      },
    ],
  }),
];
