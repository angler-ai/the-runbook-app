// UI-2: the Home page, through the app's router: each section's data,
// empty, loading and error states, the empty project's checklist, and axe.
import { createMemoryHistory } from '@tanstack/react-router';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { delay, http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { HOME_POLL_MS } from './hooks';
import { windowStart } from './model';

afterEach(() => {
  vi.useRealTimers();
  tokenStore.clear();
  localStorage.clear();
});

const NOW = Date.now();
const HOUR = 3_600_000;

const RUNBOOKS = [
  {
    runbook_id: 'rb_1',
    slug: 'post-deploy-checks',
    origin: 'ui',
    archived: false,
    title: 'Post-deploy checks',
    tags: [],
    latest_version: 2,
    draft_ahead: false,
    last_run: {
      id: 'exe_ok',
      status: 'succeeded',
      created_at_ms: NOW - HOUR,
      completed_at_ms: NOW - HOUR + 60_000,
    },
    active_runs: 0,
  },
  {
    runbook_id: 'rb_2',
    slug: 'release-train',
    origin: 'ui',
    archived: false,
    title: 'Production release train',
    tags: [],
    latest_version: 1,
    draft_ahead: false,
    last_run: { id: 'exe_run', status: 'waiting', created_at_ms: NOW - 2 * HOUR },
    active_runs: 1,
  },
  {
    runbook_id: 'rb_3',
    slug: 'cert-rotation',
    origin: 'ui',
    archived: false,
    title: 'Certificate rotation',
    tags: [],
    latest_version: 1,
    draft_ahead: false,
    last_run: {
      id: 'exe_bad',
      status: 'failed',
      created_at_ms: NOW - 3 * HOUR,
      completed_at_ms: NOW - 3 * HOUR + 5_000,
    },
    active_runs: 0,
  },
  {
    runbook_id: 'rb_4',
    slug: 'onboarding',
    origin: 'ui',
    archived: false,
    title: 'Customer onboarding',
    tags: [],
    latest_version: 1,
    draft_ahead: false,
    active_runs: 0,
  },
];

const entry = (
  id: string,
  runbook: string,
  status: string,
  created: number,
  completed: number | null,
) => ({
  id,
  runbook_id: runbook,
  status,
  title: `Run ${id}`,
  tags: [],
  fields: {},
  created_by: 'user:usr_1',
  created_at_ms: created,
  completed_at_ms: completed,
});

const OPEN_RUN = entry('exe_run', 'rb_2', 'waiting', NOW - 2 * HOUR, null);
const FAILED_RUN = entry('exe_bad', 'rb_3', 'failed', NOW - 3 * HOUR, NOW - 3 * HOUR + 5_000);
const OK_RUN = entry('exe_ok', 'rb_1', 'succeeded', NOW - HOUR, NOW - HOUR + 60_000);

function stepView(id: string, path: string, status: string, kind: string, extra: object = {}) {
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

function documentOf(
  run: ReturnType<typeof entry>,
  steps: ReturnType<typeof stepView>[],
  titles: Record<string, string>,
  summaries: Record<string, string> = {},
) {
  return {
    header: {
      schema_revision: 1,
      id: run.id,
      runbook_id: run.runbook_id,
      runbook_version_id: 'rbv_1',
      runbook_version: 1,
      title: run.title,
      status: run.status,
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
      created_at_ms: run.created_at_ms,
      started_at_ms: run.created_at_ms,
      progress: { done: 3, total: 8 },
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

const DOCUMENTS: Record<string, ReturnType<typeof documentOf>> = {
  exe_run: documentOf(
    OPEN_RUN,
    [
      stepView('sr_a', 'cab_review', 'waiting', 'approval', {
        eligible_recipients: [{ kind: 'team', reference: 'team_1' }],
      }),
      stepView('sr_s', 'customer_go', 'waiting', 'wait', {
        wait: { signal: 'release_email_sent' },
      }),
    ],
    { sr_a: 'Change advisory review', sr_s: 'Customer success confirms the go-live email' },
  ),
  exe_bad: documentOf(
    FAILED_RUN,
    [stepView('sr_f', 'region_health', 'failed', 'task')],
    { sr_f: 'Error rate in eu-west' },
    { sr_f: '5xx rate at 4.2%, above the 1% threshold' },
  ),
};

type Overrides = {
  tasks?: Parameters<typeof http.get>[1];
  report?: Parameters<typeof http.get>[1];
  runbooks?: Parameters<typeof http.get>[1];
  executions?: Parameters<typeof http.get>[1];
};

function handlers(over: Overrides = {}) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice Doe' },
        org: { id: 'org_1', title: 'Acme', role: 'owner' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes: ['runbooks:read', 'runbooks:write', 'executions:read', 'executions:start'],
          projects: null,
          expires_at_ms: Date.now() + HOUR,
        },
      }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [
          { id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' },
          { id: 'prj_2', org_id: 'org_1', slug: 'empty', title: 'Empty project' },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({
        items: [{ id: 'usr_1', name: 'Alice Doe', active: true, role: 'owner' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/teams', () =>
      HttpResponse.json({
        items: [{ id: 'team_1', org_id: 'org_1', slug: 'oncall', title: 'Oncall' }],
        next_cursor: null,
      }),
    ),
    http.get(
      '*/v1/me/tasks',
      over.tasks ??
        (() =>
          HttpResponse.json({
            items: [
              {
                project_id: 'prj_1',
                execution_id: 'exe_run',
                step_run_id: 'sr_a',
                path: 'cab_review',
                title: 'Change advisory review',
                execution_title: 'Release v2026.11.14 (high risk)',
                kind: 'approval',
                since_ms: NOW - 2 * HOUR,
              },
              {
                project_id: 'prj_9',
                execution_id: 'exe_other',
                step_run_id: 'sr_x',
                path: 'x',
                title: 'Elsewhere',
                kind: 'human_task',
              },
            ],
            next_cursor: null,
          })),
    ),
    http.get(
      '*/v1/projects/:p/runbooks',
      over.runbooks ??
        (({ params }) =>
          HttpResponse.json({ items: params.p === 'prj_1' ? RUNBOOKS : [], next_cursor: null })),
    ),
    http.get(
      '*/v1/projects/:p/executions',
      over.executions ??
        (({ request }) => {
          const status = new URL(request.url).searchParams.get('status');
          const items =
            status === 'failed'
              ? [FAILED_RUN]
              : status
                ? [OPEN_RUN]
                : [OPEN_RUN, OK_RUN, FAILED_RUN];
          return HttpResponse.json({ items, next_cursor: null });
        }),
    ),
    http.get(
      '*/v1/executions/:exe/report',
      over.report ??
        (({ params }) => HttpResponse.json(DOCUMENTS[params.exe as string] ?? DOCUMENTS.exe_run)),
    ),
  ];
}

function renderAt(path: string) {
  tokenStore.set('trb_pat_good');
  const router = createAppRouter(createMemoryHistory({ initialEntries: [path] }));
  return { router, ...render(<App router={router} />) };
}

async function section(name: RegExp) {
  const heading = await screen.findByRole('heading', { level: 2, name });
  return heading.closest('section') as HTMLElement;
}

describe('Home', () => {
  it('greets the person and answers each question, with no axe violations', async () => {
    server.use(...handlers());
    renderAt('/p/prod');
    expect(await screen.findByRole('heading', { level: 1, name: /, Alice$/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /start a run/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /new runbook/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /create with your agent/i })).toBeInTheDocument();

    // Needs you: only this project's task, with what it waits on and its age.
    const needs = await section(/needs you/i);
    const task = await within(needs).findByRole('link', { name: /change advisory review/i });
    expect(task).toHaveAttribute('href', '/x/exe_run');
    expect(task).toHaveTextContent(/your approval/i);
    expect(task).toHaveTextContent('waiting 2h');
    expect(within(needs).queryByText('Elsewhere')).not.toBeInTheDocument();

    // Running now: progress and what the run waits on.
    const running = await section(/running now/i);
    expect(
      await within(running).findByRole('progressbar', { name: '3 of 8 steps done' }),
    ).toBeInTheDocument();
    expect(await within(running).findByText('Approval · team oncall')).toBeInTheDocument();
    expect(within(running).getByText('Signal · release_email_sent')).toBeInTheDocument();
    expect(within(running).getByRole('link', { name: /view all open runs/i })).toHaveAttribute(
      'href',
      '/p/prod/executions?status=queued%2Crunning%2Cwaiting%2Cpaused',
    );

    // Failed: the failing step and its summary.
    const failed = await section(/recently failed/i);
    expect(await within(failed).findByText('Failed at Error rate in eu-west')).toBeInTheDocument();
    expect(within(failed).getByText(/5xx rate at 4.2%/)).toBeInTheDocument();

    // Health: honest counts from the catalog and today's runs.
    const health = await section(/health at a glance/i);
    const counts = within(health).getByLabelText('Runbooks by their last run');
    expect(counts).toHaveTextContent('1Passing');
    expect(counts).toHaveTextContent('1Failing');
    expect(counts).toHaveTextContent('1Never run');

    // Activity: who started what.
    const activity = await section(/recent activity/i);
    expect(await within(activity).findAllByText(/started/)).not.toHaveLength(0);
    expect(within(activity).getAllByText('Alice Doe').length).toBeGreaterThan(0);

    await expectNoAxeViolations(screen.getByRole('main'));
  });

  it('says so when nothing needs you, nothing runs and nothing failed', async () => {
    server.use(
      ...handlers({
        tasks: () => HttpResponse.json({ items: [], next_cursor: null }),
        runbooks: () => HttpResponse.json({ items: [RUNBOOKS[0]], next_cursor: null }),
        executions: () => HttpResponse.json({ items: [], next_cursor: null }),
      }),
    );
    renderAt('/p/prod');
    expect(
      await within(await section(/needs you/i)).findByText('Nothing needs you.'),
    ).toBeInTheDocument();
    expect(
      await within(await section(/running now/i)).findByText('Nothing is running.'),
    ).toBeInTheDocument();
    expect(
      await within(await section(/recently failed/i)).findByText(/no failed runs/i),
    ).toBeInTheDocument();
    expect(
      await within(await section(/recent activity/i)).findByText(/no runs in the last 7 days/i),
    ).toBeInTheDocument();
  });

  it('keeps the page when one section fails, and retries that section', async () => {
    let failing = true;
    server.use(
      ...handlers({
        tasks: () =>
          failing
            ? HttpResponse.json(
                {
                  code: 'storage.unavailable',
                  status: 503,
                  title: 't',
                  type: 't',
                  detail: 'd',
                  instance: 'urn:request:req_1',
                  errors: [],
                },
                { status: 503, headers: { 'Content-Type': 'application/problem+json' } },
              )
            : HttpResponse.json({ items: [], next_cursor: null }),
      }),
    );
    renderAt('/p/prod');
    const needs = await section(/needs you/i);
    expect(await within(needs).findByRole('alert', {}, { timeout: 4000 })).toHaveTextContent(
      /your tasks could not be loaded/i,
    );
    // The other sections still answer.
    expect(
      await within(await section(/running now/i)).findByText('Approval · team oncall'),
    ).toBeInTheDocument();
    failing = false;
    within(needs).getByRole('button', { name: 'Retry' }).click();
    expect(await within(needs).findByText('Nothing needs you.')).toBeInTheDocument();
  });

  it('shows skeletons while a section loads', async () => {
    server.use(
      ...handlers({
        executions: async () => {
          await delay('infinite');
          return HttpResponse.json({ items: [], next_cursor: null });
        },
      }),
    );
    renderAt('/p/prod');
    const running = await section(/running now/i);
    expect(within(running).getByRole('status')).toHaveTextContent(/loading open runs/i);
  });

  it('gives an empty project the getting-started checklist', async () => {
    server.use(...handlers());
    renderAt('/p/empty');
    const checklist = await section(/get this project going/i);
    const steps = within(checklist).getAllByRole('listitem');
    expect(steps.map((s) => s.textContent)).toEqual([
      expect.stringContaining('Create or import a runbook'),
      expect.stringContaining('Connect an agent'),
      expect.stringContaining('Start a first run'),
      expect.stringContaining('Invite a teammate'),
    ]);
    expect(checklist).toHaveTextContent('runbook skill install --agent claude --therunbook');
    // Each later step leads somewhere in the app, not to a CLI placeholder.
    expect(within(steps[2] as HTMLElement).getByRole('link', { name: 'Open Runbooks' })).toHaveAttribute(
      'href',
      '/p/empty/runbooks',
    );
    expect(within(steps[3] as HTMLElement).getByRole('link', { name: 'Project members' })).toHaveAttribute(
      'href',
      '/p/empty/settings',
    );
    expect(checklist).not.toHaveTextContent('<usr_');
    expect(screen.queryByRole('heading', { name: /running now/i })).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole('main')).toBeInTheDocument());
    await expectNoAxeViolations(screen.getByRole('main'));
  });

  it('is where sign-in lands: /home forwards to the current project', async () => {
    server.use(...handlers());
    const { router } = renderAt('/home');
    await waitFor(() => expect(router.state.location.pathname).toBe('/p/prod'));
  });

  it('lands on the project last chosen in this organization', async () => {
    localStorage.setItem('rb.project.org_1', 'empty');
    server.use(...handlers());
    const { router } = renderAt('/home');
    await waitFor(() => expect(router.state.location.pathname).toBe('/p/empty'));
  });
});

// UI-2 review round 1: each finding's fix, through the app's router.
describe('Home, review round 1', () => {
  const problem503 = () =>
    HttpResponse.json(
      {
        code: 'storage.unavailable',
        status: 503,
        title: 't',
        type: 't',
        detail: 'd',
        instance: 'urn:request:req_1',
        errors: [],
      },
      { status: 503, headers: { 'Content-Type': 'application/problem+json' } },
    );

  it("keeps its clock running: past midnight the date and today's runs move on", async () => {
    vi.useFakeTimers();
    const beforeMidnight = new Date(2026, 10, 13, 23, 59, 40);
    vi.setSystemTime(beforeMidnight);
    const late = entry('exe_late', 'rb_1', 'succeeded', beforeMidnight.getTime() - 40_000, null);
    server.use(
      ...handlers({
        executions: ({ request }) =>
          HttpResponse.json({
            items: new URL(request.url).searchParams.get('status') ? [] : [late],
            next_cursor: null,
          }),
      }),
    );
    renderAt('/p/prod');
    const day = (d: Date) =>
      d.toLocaleDateString([], { weekday: 'long', month: 'long', day: 'numeric' });
    const strip = () => document.querySelector('[data-slot="home-greeting"]') as HTMLElement;
    await vi.waitFor(() => expect(strip()).toHaveTextContent(day(beforeMidnight)));
    const health = () =>
      screen.getByRole('heading', { level: 2, name: /health at a glance/i }).closest('section');
    await vi.waitFor(() => expect(health()).toHaveTextContent('Runs today1'));

    await act(() => vi.advanceTimersByTimeAsync(31_000));
    await vi.waitFor(() => expect(strip()).toHaveTextContent(day(new Date(2026, 10, 14))));
    await vi.waitFor(() => expect(health()).toHaveTextContent('Runs today0'));
  });

  it('bounds Recently failed to the last 7 days, and says so', async () => {
    const failedQueries: URLSearchParams[] = [];
    server.use(
      ...handlers({
        executions: ({ request }) => {
          const query = new URL(request.url).searchParams;
          if (query.get('status') === 'failed') failedQueries.push(query);
          return HttpResponse.json({
            items: query.get('status') === 'failed' ? [FAILED_RUN] : [],
            next_cursor: null,
          });
        },
      }),
    );
    renderAt('/p/prod');
    const failed = await section(/recently failed/i);
    expect(await within(failed).findByText('Failed at Error rate in eu-west')).toBeInTheDocument();
    expect(failed).toHaveTextContent(/last 7 days/i);
    expect(failedQueries[0]?.get('from')).toBe(String(windowStart(Date.now(), 7)));
  });

  it("does not claim every last run passed when the runbook list didn't load", async () => {
    server.use(
      ...handlers({
        runbooks: problem503,
        executions: () => HttpResponse.json({ items: [], next_cursor: null }),
      }),
    );
    renderAt('/p/prod');
    const failed = await section(/recently failed/i);
    expect(await within(failed).findByText(/no failed runs/i)).toBeInTheDocument();
    await waitFor(() => expect(failed).toHaveTextContent(/could not be checked/i), {
      timeout: 4000,
    });
    expect(failed).not.toHaveTextContent(/every runbook/i);
  });

  it('names runbooks whose last run was cancelled instead of calling them passed', async () => {
    const cancelled = {
      ...RUNBOOKS[0],
      runbook_id: 'rb_9',
      slug: 'nightly',
      last_run: { id: 'exe_c', status: 'cancelled', created_at_ms: NOW - HOUR },
    };
    server.use(
      ...handlers({
        runbooks: () => HttpResponse.json({ items: [RUNBOOKS[0], cancelled], next_cursor: null }),
        executions: () => HttpResponse.json({ items: [], next_cursor: null }),
      }),
    );
    renderAt('/p/prod');
    const failed = await section(/recently failed/i);
    expect(
      await within(failed).findByText(/1 runbook's last run was cancelled/i),
    ).toBeInTheDocument();
    expect(failed).not.toHaveTextContent(/every runbook/i);
  });

  it('says a project is not found, and links the projects the person has', async () => {
    server.use(...handlers());
    renderAt('/p/nope');
    expect(
      await screen.findByRole('heading', { level: 1, name: /project not found/i }),
    ).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Production' })).toHaveAttribute('href', '/p/prod');
    expect(screen.getByRole('link', { name: 'Empty project' })).toHaveAttribute('href', '/p/empty');
    await expectNoAxeViolations(screen.getByRole('main'));
  });

  it("does not poll a finished run's document", async () => {
    const reads: Record<string, number> = {};
    server.use(
      ...handlers({
        report: ({ params }) => {
          const id = params.exe as string;
          reads[id] = (reads[id] ?? 0) + 1;
          return HttpResponse.json(DOCUMENTS[id] ?? DOCUMENTS.exe_run);
        },
      }),
    );
    vi.useFakeTimers();
    renderAt('/p/prod');
    await vi.waitFor(() =>
      expect(screen.getByText('Failed at Error rate in eu-west')).toBeInTheDocument(),
    );
    const open = reads.exe_run ?? 0;
    const finished = reads.exe_bad ?? 0;
    expect(finished).toBe(1);
    await act(() => vi.advanceTimersByTimeAsync(2 * HOME_POLL_MS + 100));
    await vi.waitFor(() => expect(reads.exe_run ?? 0).toBeGreaterThan(open));
    expect(reads.exe_bad).toBe(finished);
  });

  it('confirms the copied agent command on screen and to assistive tech', async () => {
    server.use(...handlers());
    const user = userEvent.setup();
    renderAt('/p/prod');
    const button = await screen.findByRole('button', { name: /create with your agent/i });
    await user.click(button);
    expect(await screen.findByRole('button', { name: /copied/i })).toBeInTheDocument();
    expect(
      screen
        .getAllByRole('status')
        .some((node) => /agent setup command copied/i.test(node.textContent ?? '')),
    ).toBe(true);
    await expect(navigator.clipboard.readText()).resolves.toBe(
      'runbook skill install --agent claude --therunbook',
    );
  });

  it('labels the health counts as a group', async () => {
    server.use(...handlers());
    renderAt('/p/prod');
    const health = await section(/health at a glance/i);
    expect(
      await within(health).findByRole('group', { name: 'Runbooks by their last run' }),
    ).toBeInTheDocument();
  });

  it("marks today's count as a lower bound when the read was capped", async () => {
    const now = Date.now();
    let page = 0;
    server.use(
      ...handlers({
        executions: ({ request }) => {
          if (new URL(request.url).searchParams.get('status'))
            return HttpResponse.json({ items: [], next_cursor: null });
          const start = page * 100;
          page += 1;
          const items = Array.from({ length: 100 }, (_, i) =>
            entry(`exe_${start + i}`, 'rb_1', 'succeeded', now - (start + i), null),
          );
          return HttpResponse.json({ items, next_cursor: `c${page}` });
        },
      }),
    );
    renderAt('/p/prod');
    const health = await section(/health at a glance/i);
    await waitFor(() => expect(health).toHaveTextContent('Runs today300+'));
  });
});
