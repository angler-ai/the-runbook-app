// UI-3: the run page's Graph and Document views, the live overlay (mock
// SSE), the side panel, iterations, take-over (LANG7) and edge states.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
  useNavigate,
  useParams,
  useSearch,
} from '@tanstack/react-router';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Schemas } from '../../api/client';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { expectNoAxeViolations } from '../../test/axe';
import type { Outline } from '../../ui/types';
import releaseTrainJson from '../../ui/graph/fixtures/release-train.outline.json';
import { OrgProvider } from '../context/OrgContext';
import type { CompilerBackend } from '../compile/useCompiler';
import { ExecutionDocumentPage } from './ExecutionDocumentPage';
import { forgetLiveResumePoints } from './live';
import { validateRunSearch, type RunSearch } from './views';
import {
  failedInPreflight,
  midRollout,
  RIVER,
  riley,
  RUN_ID,
  stepRun,
  waitingOnApproval,
} from './fixtures/runs';

type Doc = Schemas['ExecutionDocumentView'];
type Section = Schemas['StepSection'];
const outline = releaseTrainJson as Outline;

afterEach(() => {
  forgetLiveResumePoints();
  tokenStore.clear();
  sessionStorage.clear();
});

const compiler: CompilerBackend = {
  validate: async () => ({ diagnostics: [], step_budget: 1 }),
  outline: async () => ({ outline, diagnostics: [] }),
  describe: async () => ({ runbook: null, diagnostics: [] }),
};

const me = {
  user: { id: RIVER, name: 'Riley Chen', email: 'riley@example.test' },
  org: { id: 'org_1', title: 'Acme' },
  credential: {
    kind: 'pat',
    scopes: ['runbooks:read', 'executions:read', 'executions:act', 'executions:start'],
  },
  teams: [{ id: 'team_oncall', slug: 'oncall', title: 'On-call' }],
};

/** A live stream the test writes events into. */
function controllableStream() {
  const encoder = new TextEncoder();
  let push: (text: string) => void = () => {};
  const stream = () =>
    new ReadableStream<Uint8Array>({
      start(controller) {
        push = (text) => controller.enqueue(encoder.encode(text));
        controller.enqueue(encoder.encode(': keep-alive\n\n'));
      },
    });
  return {
    stream,
    send: (id: number, event: string) => push(`id: ${id}\nevent: ${event}\ndata: {}\n\n`),
  };
}

interface Options {
  meta?: Partial<Schemas['MetaView']>;
  sourceStatus?: number;
  logs?: string[];
  /** Called with each claim body (a take-over is a claim with `take_over: true`). */
  onClaim?: (body: unknown) => void;
  /** Step runs the human resume brief lists as people's work. */
  humanReady?: string[];
  stream?: () => ReadableStream<Uint8Array>;
}

function handlers(doc: () => Doc, options: Options = {}) {
  return [
    http.get('*/v1/me', () => HttpResponse.json(me)),
    http.get('*/v1/meta', () =>
      HttpResponse.json({
        edition: 'self_hosted',
        version: '0.0.0',
        schema: {},
        capabilities: {},
        limits: { max_request_bytes: 1 },
        ...options.meta,
      }),
    ),
    http.get('*/v1/projects/:p/members', () =>
      HttpResponse.json({ items: [{ user_id: RIVER, role: 'operator' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [{ id: 'prj_1', org_id: 'org_1', slug: 'ops', title: 'Platform Ops' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({ items: [{ id: RIVER, name: 'Riley Chen' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/teams', () =>
      HttpResponse.json({
        items: [{ id: 'team_oncall', slug: 'oncall', title: 'On-call' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/runbooks/:rb', () =>
      HttpResponse.json({ runbook_id: 'rb_release', slug: 'release-train', latest_version: 2 }),
    ),
    http.get('*/v1/runbooks/:rb/versions/:v', () =>
      options.sourceStatus
        ? HttpResponse.json(
            { code: 'auth.forbidden', title: 'Forbidden' },
            { status: options.sourceStatus },
          )
        : HttpResponse.json({ source: 'kind: Runbook # release train' }),
    ),
    http.get('*/v1/executions/:exe/next', () =>
      HttpResponse.json({
        execution_id: RUN_ID,
        schema_revision: 1,
        comments: [],
        completed_steps: [],
        decisions: [],
        next_action: { kind: 'wait' },
        ready_work: (options.humanReady ?? []).map((sr) => ({
          step_run_id: sr,
          path: sr,
          title: sr,
          actor_kind: 'human',
        })),
      }),
    ),
    http.get('*/v1/executions/:exe/report', () => HttpResponse.json(doc())),
    http.get('*/v1/executions/:exe', ({ params }) =>
      HttpResponse.json({ ...doc().header, id: params.exe, status: 'succeeded' }),
    ),
    http.get('*/v1/step-runs/:sr/logs', () =>
      HttpResponse.json({
        lines: (options.logs ?? []).map((line, i) => ({ line, seq: i + 1 })),
        has_more: false,
        next_cursor: String(options.logs?.length ?? 0),
      }),
    ),
    http.post('*/v1/step-runs/:sr/claim', async ({ request }) => {
      options.onClaim?.(await request.json());
      return HttpResponse.json({ id: 'clm_taken', generation: 2 });
    }),
    http.get(
      '*/v1/executions/:exe/events',
      () =>
        new HttpResponse(options.stream ? options.stream() : new ReadableStream({ start() {} }), {
          headers: { 'Content-Type': 'text/event-stream' },
        }),
    ),
  ];
}

function renderRun(path: string, doc: () => Doc, options: Options = {}) {
  return renderRunWith(path, doc, options);
}

function renderRunWith(
  path: string,
  doc: () => Doc,
  options: Options & { compiler?: CompilerBackend } = {},
) {
  server.use(...handlers(doc, options));
  tokenStore.set('trb_pat_good');
  const root = createRootRoute({
    component: () => (
      <main>
        <Outlet />
      </main>
    ),
  });
  function Page() {
    const { exe } = useParams({ strict: false }) as { exe: string };
    const search = useSearch({ strict: false }) as RunSearch;
    const navigate = useNavigate();
    return (
      <ExecutionDocumentPage
        executionId={exe}
        view={search.view ?? 'graph'}
        onViewChange={(view) =>
          void navigate({ to: '.', search: view === 'graph' ? {} : { view }, replace: true })
        }
        compiler={options.compiler ?? compiler}
      />
    );
  }
  const page = createRoute({
    getParentRoute: () => root,
    path: '/x/$exe',
    validateSearch: validateRunSearch,
    component: Page,
  });
  const router = createRouter({
    routeTree: root.addChildren([page]),
    history: createMemoryHistory({ initialEntries: [path] }),
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const utils = render(
    <QueryClientProvider client={client}>
      <OrgProvider>
        <RouterProvider router={router} />
      </OrgProvider>
    </QueryClientProvider>,
  );
  return { router, client, ...utils };
}

// React Flow keeps nodes hidden until measured (never, in jsdom): query
// cards by their data hooks.
async function card(id: string): Promise<HTMLElement> {
  return waitFor(() => {
    const el = document.querySelector<HTMLElement>(`[data-node-id="${id}"]`);
    if (!el) throw new Error(`no card ${id}`);
    return el;
  });
}

/** The low-risk release with us-west's health check ready for an agent (no claim). */
function readyAgentCheck(edit?: (section: Section, doc: Doc) => void): () => Doc {
  return () => {
    const doc = midRollout();
    const health = doc.sections.find((s) => s.path === 'rollout[1].settle#1.region_health')!;
    health.status = 'ready';
    delete health.actor;
    const step = doc.header.steps.find((s) => s.id === health.step_run_id)!;
    step.status = 'ready';
    step.claim = null;
    step.actor = null;
    edit?.(health, doc);
    return doc;
  };
}

/** Open us-west's health check in the side panel. */
async function openPanel(): Promise<HTMLElement> {
  fireEvent.click(within(await card('region_health')).getByRole('button', { name: /error rate/i }));
  return screen.findByRole('complementary', { name: 'Step details' });
}

describe('the live execution graph', () => {
  it('opens on the graph with each step run’s state over the runbook', async () => {
    renderRun(`/x/${RUN_ID}`, waitingOnApproval);
    expect(
      await screen.findByRole('heading', { level: 1, name: /release v2026\.11\.14/i }),
    ).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Graph', selected: true })).toBeInTheDocument();
    const changelog = await card('changelog');
    expect(changelog).toHaveAttribute('data-state', 'succeeded');
    expect(within(changelog).getByText('Pass')).toBeInTheDocument();
    expect(within(changelog).getByText(/release-agent · claude-sonnet/)).toBeInTheDocument();
    const cab = await card('cab_review');
    expect(cab).toHaveAttribute('data-state', 'waiting');
    expect(cab).toHaveAttribute('data-current', 'true');
    expect(within(cab).getByText('Approval · team oncall')).toBeInTheDocument();
    // The branches the risk switch did not take are dimmed; catch and
    // finally have not run.
    expect(await card('lead_signoff')).toHaveAttribute('data-dimmed', 'true');
    expect(within(await card('lead_signoff')).getByText('Not taken')).toBeInTheDocument();
    expect(await card('preflight_failed')).toHaveAttribute('data-dimmed', 'true');
    expect(await card('retro_notes')).toHaveAttribute('data-dimmed', 'true');
    expect(await card('hold_if_rejected')).not.toHaveAttribute('data-dimmed');
    // The header: runbook and version, progress, who started it.
    expect(screen.getByRole('link', { name: 'release-train' })).toHaveAttribute(
      'href',
      '/p/ops/runbooks/rb_release',
    );
    expect(await screen.findByText('Older version · latest is v2')).toBeInTheDocument();
    expect(screen.getByRole('progressbar', { name: '1 of 7 steps done' })).toBeInTheDocument();
    expect(await screen.findByText('Started by Riley Chen')).toBeInTheDocument();
  });

  it('opens the selected step run in the side panel, with its log and the definition folded', async () => {
    renderRun(`/x/${RUN_ID}`, waitingOnApproval, {
      logs: ['dry run on replica-2: 2 pending migrations, both additive'],
    });
    const migrations = await card('migrations');
    fireEvent.click(within(migrations).getByRole('button', { name: /database migrations/i }));
    const panel = await screen.findByRole('complementary', { name: 'Step details' });
    expect(within(panel).getByText(/two additive migrations/i)).toBeInTheDocument();
    expect(within(panel).getByRole('rowheader', { name: 'pending' })).toBeInTheDocument();
    expect(within(panel).getByText('Definition')).toBeInTheDocument();
    await userEvent.setup().click(within(panel).getByRole('button', { name: /show log/i }));
    expect(await within(panel).findByText(/both additive/)).toBeInTheDocument();
    await expectNoAxeViolations(document.body);
  });

  it('shows an approval’s prompt and votes in the panel', async () => {
    renderRun(`/x/${RUN_ID}`, waitingOnApproval);
    fireEvent.click(
      within(await card('cab_review')).getByRole('button', { name: /change advisory/i }),
    );
    const panel = await screen.findByRole('complementary', { name: 'Step details' });
    expect(
      within(panel).getByText('Approve the high-risk release v2026.11.14?'),
    ).toBeInTheDocument();
    expect(within(panel).getByText(/no votes yet/i)).toBeInTheDocument();
  });

  it('counts iterations and lets the person pick one', async () => {
    renderRun(`/x/${RUN_ID}`, midRollout);
    const rollout = await card('rollout');
    await waitFor(() => expect(within(rollout).getByText('region 2 of 3')).toBeInTheDocument());
    expect(within(rollout).getByText(/us-west/)).toBeInTheDocument();
    expect(within(await card('settle')).getByText('attempt 1 of 4')).toBeInTheDocument();
    expect(await card('region_health')).toHaveAttribute('data-state', 'running');
    fireEvent.click(
      within(rollout).getByRole('button', { name: 'Previous iteration of Roll out per region' }),
    );
    await waitFor(() => expect(within(rollout).getByText('region 1 of 3')).toBeInTheDocument());
    expect(within(await card('settle')).getByText('attempt 2 of 4')).toBeInTheDocument();
    expect(await card('region_health')).toHaveAttribute('data-state', 'succeeded');
    fireEvent.click(
      within(await card('settle')).getByRole('button', {
        name: 'Previous iteration of Wait for region to settle',
      }),
    );
    await waitFor(() =>
      expect(
        within(document.querySelector('[data-node-id="region_health"]')!).getByText('Fail'),
      ).toBeInTheDocument(),
    );
    // The default branch ran; the cases did not.
    expect(await card('auto_ok')).toHaveAttribute('data-state', 'succeeded');
    expect(await card('cab_review')).toHaveAttribute('data-dimmed', 'true');
  });

  it('updates in place from the event stream, without re-drawing the graph', async () => {
    let doc = waitingOnApproval();
    const live = controllableStream();
    renderRun(`/x/${RUN_ID}`, () => doc, { stream: live.stream });
    const cab = await card('cab_review');
    expect(cab).toHaveAttribute('data-state', 'waiting');
    await screen.findByText('Live');
    // The approval is decided elsewhere; the stream says something changed.
    doc = waitingOnApproval();
    const section = doc.sections.find((s) => s.path === 'cab_review')!;
    const step = doc.header.steps.find((s) => s.path === 'cab_review')!;
    section.status = 'succeeded';
    step.status = 'succeeded';
    section.outputs = { decision: 'ship', decided_by: RIVER };
    act(() => live.send(20, 'ApprovalDecided'));
    await waitFor(
      () =>
        expect(document.querySelector('[data-node-id="cab_review"]')).toHaveAttribute(
          'data-state',
          'succeeded',
        ),
      { timeout: 3000 },
    );
    // The same card element: state changed in place, nothing re-mounted.
    expect(document.querySelector('[data-node-id="cab_review"]')).toBe(cab);
    expect(screen.getByText('Live')).toBeInTheDocument();
  });

  it('links a call step to its child run and shows the child’s status', async () => {
    const doc = midRollout();
    const verify = doc.header.steps.find((s) => s.path === 'verify')!;
    verify.status = 'waiting';
    verify.call = { child_execution_id: 'exe_CHILD', started: true };
    doc.sections.find((s) => s.path === 'verify')!.status = 'waiting';
    const { router } = renderRun(`/x/${RUN_ID}`, () => doc);
    const link = await waitFor(() =>
      within(document.querySelector<HTMLElement>('[data-node-id="verify"]')!).getByRole('link', {
        name: /child run · succeeded/i,
      }),
    );
    expect(link).toHaveAttribute('href', '/x/exe_CHILD');
    // Followed in the app (the router), not by reloading it.
    expect(fireEvent.click(link)).toBe(false);
    await waitFor(() => expect(router.state.location.pathname).toBe('/x/exe_CHILD'));
  });

  it('reads only the child runs in view, and refreshes them with the parent (a foreach of 50 calls)', async () => {
    // The release train with a call inside the rollout: one child per region.
    const withCall = structuredClone(outline);
    const rollout = withCall.steps.find((n) => n.id === 'rollout')!;
    rollout.children = [
      ...(rollout.children ?? []),
      { id: 'notify', path: 'rollout.notify', kind: 'call', call: { runbook: 'notify-region' } },
    ];
    const callCompiler: CompilerBackend = {
      ...compiler,
      outline: async () => ({ outline: withCall, diagnostics: [] }),
    };
    const build = (childStatus: Schemas['StepView']['status']) => {
      const doc = midRollout();
      const steps = doc.header.steps;
      const roll = steps.find((s) => s.path === 'rollout')!;
      roll.iterations = { started: 50, total: 50 };
      for (let i = 2; i < 50; i += 1) {
        const run = stepRun(`rollout[${i}].deploy_region`, { status: 'succeeded' });
        doc.sections.push(run.section);
        steps.push(run.step);
      }
      for (let i = 0; i < 50; i += 1) {
        const run = stepRun(`rollout[${i}].notify`, {
          status: childStatus,
          step: { call: { child_execution_id: `exe_CHILD${i}`, started: true } },
        });
        doc.sections.push(run.section);
        steps.push(run.step);
      }
      return doc;
    };
    let doc = build('waiting');
    const reads: string[] = [];
    let childState = 'running';
    const live = controllableStream();
    const { client } = renderRunWith(`/x/${RUN_ID}`, () => doc, {
      stream: live.stream,
      compiler: callCompiler,
    });
    server.use(
      http.get('*/v1/executions/:exe', ({ params }) => {
        const exe = String(params.exe);
        if (exe === RUN_ID) return HttpResponse.json(doc.header);
        reads.push(exe);
        return HttpResponse.json({ ...doc.header, id: exe, status: childState });
      }),
    );
    await waitFor(() =>
      expect(
        within(document.querySelector<HTMLElement>('[data-node-id="notify"]')!).getByRole('link', {
          name: /child run · running/i,
        }),
      ).toBeInTheDocument(),
    );
    // One read: the iteration on screen (us-west), not one per region.
    expect(reads).toEqual(['exe_CHILD1']);
    await screen.findByText('Live');
    // The parent changes: the child on screen is read again, and its new state shows.
    childState = 'succeeded';
    doc = build('succeeded');
    act(() => live.send(30, 'StepCompleted'));
    await waitFor(
      () =>
        expect(
          within(document.querySelector<HTMLElement>('[data-node-id="notify"]')!).getByRole(
            'link',
            { name: /child run · succeeded/i },
          ),
        ).toBeInTheDocument(),
      { timeout: 3000 },
    );
    expect(reads).toEqual(['exe_CHILD1', 'exe_CHILD1']);
    // A finished child is final: later parent refreshes do not read it again.
    act(() => live.send(31, 'StepCompleted'));
    await waitFor(
      () =>
        expect(
          client.getQueryState(['executions', RUN_ID, 'report'])?.dataUpdateCount,
        ).toBeGreaterThanOrEqual(3),
      { timeout: 3000 },
    );
    await new Promise((r) => setTimeout(r, 50));
    expect(reads).toEqual(['exe_CHILD1', 'exe_CHILD1']);
  });

  it('keeps the side panel on the step run the person opened when a new iteration starts', async () => {
    let doc = midRollout();
    const live = controllableStream();
    renderRun(`/x/${RUN_ID}`, () => doc, { stream: live.stream });
    const panel = await openPanel();
    const opened = doc.sections.find((s) => s.path === 'rollout[1].settle#1.region_health')!;
    expect(within(panel).getByText('us-west · attempt 1')).toBeInTheDocument();
    await screen.findByText('Live');
    // us-west's first check fails and the loop starts attempt 2.
    doc = midRollout();
    const first = doc.sections.find((s) => s.path === 'rollout[1].settle#1.region_health')!;
    const firstStep = doc.header.steps.find((s) => s.id === first.step_run_id)!;
    first.status = 'succeeded';
    firstStep.status = 'succeeded';
    first.verdict = 'fail';
    const next = stepRun('rollout[1].settle#2.region_health', { status: 'claimed', actor: riley });
    doc.sections.push(next.section);
    doc.header.steps.push(next.step);
    act(() => live.send(40, 'StepCompleted'));
    await waitFor(
      () =>
        expect(
          screen
            .getByRole('complementary', { name: 'Step details' })
            .querySelector('[data-slot="run-details"]'),
        ).toHaveAttribute('data-step-run', opened.step_run_id),
      { timeout: 3000 },
    );
    // The card's state came from the new read, and the panel stayed put.
    await waitFor(
      () =>
        expect(document.querySelector('[data-node-id="region_health"]')).toHaveAttribute(
          'data-state',
          'succeeded',
        ),
      { timeout: 3000 },
    );
    expect(
      screen
        .getByRole('complementary', { name: 'Step details' })
        .querySelector('[data-slot="run-details"]'),
    ).toHaveAttribute('data-step-run', opened.step_run_id);
    // Closed, the graph follows the run again: attempt 2.
    fireEvent.click(screen.getByRole('button', { name: 'Close step details' }));
    await waitFor(() =>
      expect(
        within(document.querySelector<HTMLElement>('[data-node-id="settle"]')!).getByText(
          'attempt 2 of 4',
        ),
      ).toBeInTheDocument(),
    );
  });

  it('takes over a ready agent step with a take-over claim (LANG7)', async () => {
    const onClaim = vi.fn();
    renderRun(`/x/${RUN_ID}`, readyAgentCheck(), { onClaim });
    const panel = await openPanel();
    await userEvent
      .setup()
      .click(await within(panel).findByRole('button', { name: /^take over$/i }));
    await waitFor(() => expect(onClaim).toHaveBeenCalled());
    expect(onClaim.mock.calls[0]?.[0]).toMatchObject({
      actor: { kind: 'human' },
      take_over: true,
    });
  });

  it('offers no take-over on a paused run or on a step that is not an agent’s (LANG7)', async () => {
    const paused = renderRun(
      `/x/${RUN_ID}`,
      readyAgentCheck((_section, doc) => {
        doc.header.status = 'paused';
      }),
    );
    let panel = await openPanel();
    expect(within(panel).getByText('Status')).toBeInTheDocument();
    await new Promise((r) => setTimeout(r, 50));
    expect(within(panel).queryByRole('button', { name: /take over/i })).not.toBeInTheDocument();
    paused.unmount();

    renderRun(
      `/x/${RUN_ID}`,
      readyAgentCheck((section) => {
        section.task_actor = 'any';
      }),
    );
    panel = await openPanel();
    await new Promise((r) => setTimeout(r, 50));
    expect(within(panel).queryByRole('button', { name: /take over/i })).not.toBeInTheDocument();
  });

  it('offers a plain claim, not take-over, where a person’s claim is the action (LANG7)', async () => {
    let sr = '';
    renderRun(
      `/x/${RUN_ID}`,
      readyAgentCheck((section) => {
        sr = section.step_run_id;
      }),
      {
        get humanReady() {
          return [sr];
        },
      },
    );
    const panel = await openPanel();
    expect(
      await within(panel).findByRole('button', { name: 'Claim this step' }),
    ).toBeInTheDocument();
    expect(within(panel).queryByRole('button', { name: /take over/i })).not.toBeInTheDocument();
  });

  it('shows a strict assignee, and no claim, to anyone else (LANG7)', async () => {
    let sr = '';
    renderRun(
      `/x/${RUN_ID}`,
      readyAgentCheck((section) => {
        sr = section.step_run_id;
        section.task_actor = 'human';
        section.assignee = {
          to: 'team:sre',
          strict: true,
          recipients: [{ kind: 'team', reference: 'team_sre' }],
        };
      }),
      {
        get humanReady() {
          return [sr];
        },
      },
    );
    const panel = await openPanel();
    const assigned = await within(panel).findByText(/\(strict\)/);
    expect(assigned.closest('div')).toHaveTextContent(/assigned to\s*team_sre \(strict\)/i);
    expect(within(panel).getAllByText(/\(strict\)/)).toHaveLength(1);
    await new Promise((r) => setTimeout(r, 50));
    expect(
      within(panel).queryByRole('button', { name: 'Claim this step' }),
    ).not.toBeInTheDocument();
    expect(within(panel).queryByRole('button', { name: /take over/i })).not.toBeInTheDocument();
  });

  it('shows a strict assignee to the assignee too, with the claim (LANG7)', async () => {
    let sr = '';
    renderRun(
      `/x/${RUN_ID}`,
      readyAgentCheck((section) => {
        sr = section.step_run_id;
        section.task_actor = 'any';
        section.assignee = {
          to: 'team:oncall',
          strict: true,
          recipients: [{ kind: 'team', reference: 'team_oncall' }],
        };
      }),
      {
        get humanReady() {
          return [sr];
        },
      },
    );
    const panel = await openPanel();
    expect(
      await within(panel).findByRole('button', { name: 'Claim this step' }),
    ).toBeInTheDocument();
    expect(
      within(panel)
        .getByText(/\(strict\)/)
        .closest('div'),
    ).toHaveTextContent(/assigned to\s*oncall \(strict\)/i);
  });

  it('says who took a step over on its card, in the panel and in the document (LANG7)', async () => {
    const doc = () => {
      const d = midRollout();
      const health = d.sections.find((s) => s.path === 'rollout[1].settle#1.region_health')!;
      health.actor = riley;
      health.taken_over = { by: riley, at_ms: 1_794_500_100_000, previous_actor_kind: 'agent' };
      const step = d.header.steps.find((s) => s.id === health.step_run_id)!;
      step.actor = riley;
      return d;
    };
    const { router } = renderRun(`/x/${RUN_ID}`, doc);
    expect(await card('region_health')).toHaveTextContent('Taken over by Riley Chen');
    const panel = await openPanel();
    expect(within(panel).getByText(/taken over by riley chen/i)).toBeInTheDocument();
    await router.navigate({ to: '.', search: { view: 'document' } });
    const article = await screen.findByRole('article', { name: /as a document/i });
    const callout = article.querySelector('[data-slot="doc-takeover"]');
    expect(callout).toHaveTextContent('Taken over by Riley Chen from an agent.');
  });

  it('folds undelivered notifications into a compact notice that can be dismissed', async () => {
    const doc = waitingOnApproval();
    doc.header.notifications_not_delivered = [
      { event_id: 'evt_01UNDELIVERED', event_type: 'step.blocked', reason: 'no_subscriber' },
    ];
    renderRun(`/x/${RUN_ID}`, () => doc);
    const summary = await screen.findByText('1 notification not delivered');
    const notice = summary.closest('[data-slot="undelivered-notice"]') as HTMLElement;
    expect(notice.tagName).toBe('DETAILS');
    expect(within(notice).getByText('evt_01UNDELIVERED')).not.toBeVisible();
    fireEvent.click(summary);
    expect(within(notice).getByText('evt_01UNDELIVERED')).toBeVisible();
    await userEvent.setup().click(screen.getByRole('button', { name: /dismiss/i }));
    expect(screen.queryByText('1 notification not delivered')).not.toBeInTheDocument();
  });

  it('says why there is no graph for a preview run or an unreadable runbook', async () => {
    const preview = waitingOnApproval();
    preview.header.runbook_version = null;
    const { unmount } = renderRun(`/x/${RUN_ID}`, () => preview);
    expect(await screen.findByText('No graph for a preview run')).toBeInTheDocument();
    unmount();
    renderRun(`/x/${RUN_ID}`, waitingOnApproval, { sourceStatus: 403 });
    expect(await screen.findByText('The runbook is not visible to you')).toBeInTheDocument();
    await userEvent
      .setup()
      .click(screen.getByRole('button', { name: /read the run as a document/i }));
    expect(await screen.findByRole('article')).toBeInTheDocument();
  });

  it('says a queued run waits for a slot', async () => {
    const queued = waitingOnApproval();
    queued.header.status = 'queued';
    delete queued.header.started_at_ms;
    renderRun(`/x/${RUN_ID}`, () => queued);
    expect(
      await screen.findByText(/queued: this run starts when a concurrency slot frees up/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/^Waiting for a concurrency slot · queued/)).toBeInTheDocument();
  });
});

describe('the Document view', () => {
  it('reads the run as one page: outcome, who, nested sections and folded lines', async () => {
    renderRun(`/x/${RUN_ID}?view=document`, failedInPreflight);
    const article = await screen.findByRole('article', { name: /as a document/i });
    expect(within(article).getByText('Failed in 4m 12s')).toBeInTheDocument();
    expect(within(article).getByText('2 of 3 checks passed · 1 failed')).toBeInTheDocument();
    expect(within(article).getByText(/started by/i)).toHaveTextContent('Started by Riley Chen');
    expect(within(article).getByText(/worked on by/i)).toHaveTextContent(
      'release-agent · claude-sonnet',
    );
    expect(
      within(article).getByRole('heading', { level: 3, name: /preflight$/i }),
    ).toBeInTheDocument();
    expect(
      within(article).getByRole('heading', { level: 4, name: /database migrations/i }),
    ).toBeInTheDocument();
    expect(
      within(article).getByText(/drops a column the old release still reads/),
    ).toBeInTheDocument();
    const folded = article.querySelectorAll('[data-slot="doc-folded"]');
    expect([...folded].map((f) => f.textContent)).toEqual([
      'Skipped: Build and sign the release artifacts',
      'Did not run: Risk gate, Canary, Roll out per region, Post-deploy checks, Announce, shipped',
    ]);
    // Logs stay collapsed until asked for.
    expect(within(article).getAllByRole('button', { name: /show log/i }).length).toBeGreaterThan(0);
    expect(within(article).queryByRole('region', { name: 'Log lines' })).not.toBeInTheDocument();
    await expectNoAxeViolations(article);
  });

  it('prints what the Markdown export prints: memory, a replaced run, a call that did not start', async () => {
    const doc = failedInPreflight();
    doc.header.status = 'cancelled';
    doc.header.superseded_by = { execution_id: 'exe_NEXT', started_by: `user:${RIVER}` };
    const changelog = doc.sections.find((s) => s.path === 'changelog')!;
    changelog.memory_proposed = [{ memory_id: 'mem_PROPOSED' }];
    changelog.memory_saved = [{ memory_id: 'mem_SAVED', dropped: 'memory.too_long' }];
    changelog.memory_edited = [{ memory_id: 'mem_EDITED' }];
    const verify = doc.sections.find((s) => s.path === 'verify')!;
    const verifyStep = doc.header.steps.find((s) => s.id === verify.step_run_id)!;
    verify.status = 'failed';
    verifyStep.status = 'failed';
    const refused = {
      child_execution_id: 'exe_REFUSED',
      started: false,
      error: 'limit.executions_monthly',
    };
    verify.call = refused;
    verifyStep.call = refused;
    renderRun(`/x/${RUN_ID}?view=document`, () => doc);
    const article = await screen.findByRole('article', { name: /as a document/i });
    const replaced = within(article).getByText(/replaced by/i);
    expect(replaced).toHaveTextContent('Cancelled: replaced by exe_NEXT, started by Riley Chen');
    expect(within(replaced).getByRole('link', { name: 'exe_NEXT' })).toHaveAttribute(
      'href',
      '/x/exe_NEXT',
    );
    const memory = [...article.querySelectorAll('[data-slot="doc-memory"] li')].map(
      (li) => li.textContent,
    );
    expect(memory).toEqual([
      'Memory proposed: mem_PROPOSED',
      'Memory saved: mem_SAVED (dropped: memory.too_long)',
      'Memory edited: mem_EDITED',
    ]);
    expect(within(article).getByText(/did not start/)).toHaveTextContent(
      'The child run did not start (limit.executions_monthly).',
    );
    expect(within(article).queryByRole('link', { name: /open the child run/i })).toBeNull();
  });

  it('shows a step a person skipped, with who and why, instead of folding it', async () => {
    const doc = midRollout();
    const build = doc.sections.find((s) => s.path === 'build')!;
    const buildStep = doc.header.steps.find((s) => s.id === build.step_run_id)!;
    build.status = 'skipped';
    buildStep.status = 'skipped';
    build.actor = riley;
    buildStep.actor = riley;
    build.summary = 'Reusing yesterday’s signed artifacts.';
    renderRun(`/x/${RUN_ID}?view=document`, () => doc);
    const article = await screen.findByRole('article', { name: /as a document/i });
    const section = article.querySelector<HTMLElement>('[data-step="build"]')!;
    expect(within(section).getByText(/skipped by/i)).toHaveTextContent('Skipped by Riley Chen');
    expect(within(section).getByText('Reusing yesterday’s signed artifacts.')).toBeInTheDocument();
    const folded = [...article.querySelectorAll('[data-slot="doc-folded"]')].map(
      (f) => f.textContent,
    );
    expect(folded.join(' ')).not.toContain('Build and sign');
  });

  it('opens a child run in the app, without reloading the page', async () => {
    const doc = midRollout();
    const verify = doc.sections.find((s) => s.path === 'verify')!;
    const verifyStep = doc.header.steps.find((s) => s.id === verify.step_run_id)!;
    verify.status = 'waiting';
    verifyStep.status = 'waiting';
    verifyStep.call = { child_execution_id: 'exe_CHILD', started: true };
    const { router } = renderRun(`/x/${RUN_ID}?view=document`, () => doc);
    const article = await screen.findByRole('article', { name: /as a document/i });
    fireEvent.click(within(article).getByRole('link', { name: /open the child run/i }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/x/exe_CHILD'));
  });

  it('shows approvals as callouts and the open run’s progress', async () => {
    renderRun(`/x/${RUN_ID}?view=document`, waitingOnApproval);
    const article = await screen.findByRole('article', { name: /as a document/i });
    const callout = article.querySelector('[data-slot="doc-approval"]');
    expect(callout).toHaveTextContent('Approve the high-risk release v2026.11.14?');
    expect(callout).toHaveTextContent('Waiting for a decision from team oncall.');
    expect(within(article).getByText(/so far/)).toBeInTheDocument();
    // The untaken risk cases fold, each with its condition.
    const untaken = [...article.querySelectorAll('[data-reason="untaken"]')].map(
      (f) => f.textContent,
    );
    expect(untaken).toEqual([
      "Not taken (inputs.risk == 'medium'): Release lead signs off",
      'Not taken (default): auto_ok',
    ]);
  });

  it('heads a check with its verdict’s icon, not its status’s', async () => {
    renderRun(`/x/${RUN_ID}?view=document`, midRollout);
    const article = await screen.findByRole('article', { name: /as a document/i });
    const step = (path: string) => {
      const el = article.querySelector<HTMLElement>(`[data-step="${path}"]`);
      if (!el) throw new Error(`no step ${path}`);
      return el.querySelector('[data-slot="doc-step-icon"]');
    };
    // Succeeded with a fail verdict ("Error rate in eu-west" in the review seed).
    expect(step('rollout[0].settle#1.region_health')).toHaveAttribute('data-icon', 'fail');
    expect(step('rollout[0].settle#2.region_health')).toHaveAttribute('data-icon', 'pass');
    // No verdict: the status decides.
    expect(step('rollout[0].deploy_region')).toHaveAttribute('data-icon', 'succeeded');
  });

  it('switches views through the tabs and keeps the choice in the URL', async () => {
    const { router } = renderRun(`/x/${RUN_ID}`, waitingOnApproval);
    await card('changelog');
    await userEvent.setup().click(screen.getByRole('tab', { name: 'Document' }));
    expect(await screen.findByRole('article')).toBeInTheDocument();
    expect(router.state.location.search).toEqual({ view: 'document' });
    await userEvent.setup().click(screen.getByRole('tab', { name: 'Steps' }));
    expect(await screen.findByRole('navigation', { name: 'Steps navigation' })).toBeInTheDocument();
  });
});
