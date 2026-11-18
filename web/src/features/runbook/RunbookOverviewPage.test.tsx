// UI-2: the runbook's own page: header facts, Graph as the default tab,
// tabs in `?tab=`, the read-only Source with "Edit draft", the draft when
// nothing is published, and a slug that resolves through the catalog.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
  useParams,
  useSearch,
} from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { fixtures } from '../../test/msw/fixtures';
import { server } from '../../test/msw/server';
import type { Outline } from '../../ui/types';
import { OrgProvider } from '../context/OrgContext';
import type { CompilerBackend } from '../compile/useCompiler';
import { editorHandlers, problem, RB } from '../editor/test-support';
import { ownersOf, RunbookOverviewPage } from './RunbookOverviewPage';
import { validateOverviewSearch, type OverviewSearch } from './tabs';

// The graph itself is tested in ui/graph; here, what the page hands it.
vi.mock('../../ui/graph', () => ({
  RunbookGraph: ({
    outline,
    callHref,
  }: {
    outline: Outline;
    callHref?: (slug: string) => string;
  }) => (
    <div data-testid="graph">
      <p>Graph of {outline.title}</p>
      <p>{outline.steps.map((s) => s.id).join(',')}</p>
      <p>{callHref?.('post-deploy-checks')}</p>
    </div>
  ),
}));

// The run itself is tested on its own page; here, which run the Runs tab shows.
vi.mock('../execution/ExecutionDocumentPage', () => ({
  ExecutionDocumentPage: ({ executionId }: { executionId: string }) => (
    <p data-testid="run-view">Run {executionId}</p>
  ),
}));

afterEach(() => tokenStore.clear());

const PUBLISHED = `apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: post-deploy
  title: Post-deploy verification
  owners: [team:oncall, usr_alice]
spec:
  steps:
    - {id: published_step, title: Published}
`;
const DRAFT = PUBLISHED.replace('published_step', 'draft_step');

function compiler(): CompilerBackend & { sources: string[] } {
  const sources: string[] = [];
  return {
    sources,
    validate: async () => ({ diagnostics: [], step_budget: 1 }),
    outline: async ({ source }) => {
      sources.push(source);
      const step = /id: (\w+_step)/.exec(source)?.[1] ?? 'none';
      return {
        outline: {
          id: 'post-deploy',
          title: 'Post-deploy verification',
          steps: [{ id: step, path: '/spec/steps/0', kind: 'task' }],
          finally: [],
          stepBudget: 1,
        },
        diagnostics: [],
      };
    },
    describe: async () => ({ runbook: null, diagnostics: [] }),
  };
}

function handlers(latestVersion: number | null) {
  return [
    http.get('*/v1/runbooks/:rb', () =>
      HttpResponse.json({
        runbook_id: RB,
        slug: 'post-deploy',
        origin: 'ui',
        archived: false,
        title: 'Post-deploy verification',
        description: 'Health, error rates and latency after every deploy.',
        tags: ['deploy', 'agentic'],
        latest_version: latestVersion,
        draft_ahead: latestVersion != null,
      }),
    ),
    http.get('*/v1/runbooks/:rb/versions/:version', () =>
      HttpResponse.json({ source: PUBLISHED, runbook_version: latestVersion }),
    ),
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({
        items: [
          {
            runbook_id: RB,
            slug: 'post-deploy',
            origin: 'ui',
            archived: false,
            tags: [],
            draft_ahead: false,
          },
          {
            runbook_id: 'rb_child',
            slug: 'post-deploy-checks',
            origin: 'ui',
            archived: false,
            tags: [],
            draft_ahead: false,
          },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/executions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/runbooks/:rb/versions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    ...editorHandlers({ draft: { source: DRAFT, revision: 3 }, latestVersion }),
  ];
}

function renderPage(path: string, backend: CompilerBackend) {
  tokenStore.set('trb_pat_good');
  const root = createRootRoute({
    component: () => (
      <main>
        <Outlet />
      </main>
    ),
  });
  function Page() {
    const { project, rb } = useParams({ strict: false }) as { project: string; rb: string };
    const search = useSearch({ strict: false }) as OverviewSearch;
    return (
      <RunbookOverviewPage
        projectRef={project}
        runbookRef={rb}
        search={search}
        compiler={backend}
      />
    );
  }
  const page = createRoute({
    getParentRoute: () => root,
    path: '/p/$project/runbooks/$rb',
    validateSearch: validateOverviewSearch,
    component: Page,
  });
  const router = createRouter({
    routeTree: root.addChildren([page]),
    history: createMemoryHistory({ initialEntries: [path] }),
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <OrgProvider>
        <RouterProvider router={router} />
      </OrgProvider>
    </QueryClientProvider>,
  );
  return router;
}

describe('the runbook page', () => {
  it('leads with the header and the published graph, and has no axe violations', async () => {
    server.use(...handlers(2));
    const backend = compiler();
    renderPage(`/p/prod/runbooks/${RB}`, backend);
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Post-deploy verification' }),
    ).toBeInTheDocument();
    expect(
      screen.getByText('Health, error rates and latency after every deploy.'),
    ).toBeInTheDocument();
    expect(screen.getByText('v2')).toBeInTheDocument();
    expect(screen.getByText('draft ahead')).toBeInTheDocument();
    expect(
      within(screen.getByRole('list', { name: 'Tags' })).getAllByRole('listitem'),
    ).toHaveLength(2);
    expect(await screen.findByText('team:oncall, usr_alice')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /start run/i })).toBeEnabled();
    expect(screen.getByRole('link', { name: /edit/i })).toHaveAttribute(
      'href',
      `/p/prod/runbooks/${RB}/editor`,
    );

    // Graph is the default tab, drawn from the published version, with call links by ID.
    expect(screen.getByRole('tab', { name: 'Graph' })).toHaveAttribute('aria-selected', 'true');
    expect(await screen.findByText('published_step')).toBeInTheDocument();
    expect(screen.getByText('/p/prod/runbooks/rb_child')).toBeInTheDocument();
    await expectNoAxeViolations(screen.getByRole('main'));
  });

  it('shows published schedules and webhook URLs on the runbook page', async () => {
    const withTriggers = PUBLISHED.replace(
      'spec:\n  steps:',
      `spec:
  triggers:
    schedule:
      - {id: nightly, cron: "0 2 * * *", timezone: America/Los_Angeles}
    webhook:
      - {id: on_deploy}
  steps:`,
    );
    server.use(
      http.get('*/v1/runbooks/:rb/versions/:version', () =>
        HttpResponse.json({ source: withTriggers, runbook_version: 2 }),
      ),
      ...handlers(2),
    );
    renderPage(`/p/prod/runbooks/${RB}`, compiler());
    expect(await screen.findByRole('heading', { name: 'Triggers' })).toBeInTheDocument();
    expect(screen.getByText('nightly')).toBeInTheDocument();
    expect(screen.getByText('on_deploy')).toBeInTheDocument();
    expect(
      screen.getByText(
        `POST ${window.location.origin}/v1/projects/prod/runbooks/post-deploy/triggers/on_deploy`,
      ),
    ).toBeInTheDocument();
  });

  it('keeps the tab in the URL and shows the read-only source', async () => {
    server.use(...handlers(2));
    const router = renderPage(`/p/prod/runbooks/${RB}`, compiler());
    const user = userEvent.setup();
    await user.click(await screen.findByRole('tab', { name: 'Source' }));
    await waitFor(() => expect(router.state.location.search).toEqual({ tab: 'source' }));
    const source = await screen.findByLabelText('Source of version 2');
    expect(source).toHaveTextContent('published_step');
    expect(screen.getByRole('link', { name: 'Edit draft' })).toHaveAttribute(
      'href',
      `/p/prod/runbooks/${RB}/editor`,
    );
    await user.click(screen.getByRole('tab', { name: 'Runs' }));
    await waitFor(() => expect(router.state.location.search).toEqual({ tab: 'runs' }));
    await user.click(screen.getByRole('tab', { name: 'Graph' }));
    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });

  it('lists runs beside the selected run, the first by default', async () => {
    const entry = (id: string, at: number, createdBy: string) => ({
      id,
      runbook_id: RB,
      title: 'Post-deploy verification',
      status: 'running',
      fields: {},
      tags: [],
      created_at_ms: at,
      created_by: createdBy,
    });
    server.use(
      http.get('*/v1/projects/:p/executions', () =>
        HttpResponse.json({
          items: [
            entry('exe_first', Date.now() - 60_000, 'webhook:new_customer'),
            entry('exe_second', Date.now() - 3_600_000, 'service:tok_1'),
          ],
          next_cursor: null,
        }),
      ),
      http.get('*/v1/executions/:exe', ({ params }) =>
        HttpResponse.json({
          ...fixtures.postDeployCompleted,
          id: params.exe,
          inputs: { customer: params.exe === 'exe_first' ? 'Northwind' : 'Contoso' },
          fields: {},
        }),
      ),
      ...handlers(2),
    );
    const router = renderPage(`/p/prod/runbooks/${RB}?tab=runs`, compiler());
    const first = await screen.findByRole('button', { name: /customer=Northwind/ });
    expect(first).toHaveAttribute('aria-current', 'true');
    expect(first).toHaveTextContent('Webhook new_customer');
    expect(screen.getByTestId('run-view')).toHaveTextContent('Run exe_first');

    const user = userEvent.setup();
    await user.click(screen.getByText('customer=Contoso'));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ tab: 'runs', run: 'exe_second' }),
    );
    expect(screen.getByTestId('run-view')).toHaveTextContent('Run exe_second');
    expect(screen.getByRole('button', { name: /customer=Contoso/ })).toHaveAttribute(
      'aria-current',
      'true',
    );
    expect(screen.getByRole('link', { name: /Open the run’s own page/ })).toHaveAttribute(
      'href',
      '/x/exe_second',
    );
  });

  it('opens on the tab a link names', async () => {
    server.use(...handlers(2));
    renderPage(`/p/prod/runbooks/${RB}?tab=versions`, compiler());
    expect(await screen.findByRole('tab', { name: 'Versions' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('opens on the memory tab and shows Memory tab selected', async () => {
    server.use(
      ...handlers(2),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
    );
    renderPage(`/p/prod/runbooks/${RB}?tab=memory`, compiler());
    expect(await screen.findByRole('tab', { name: 'Memory' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('shows the draft, and says so, when nothing is published', async () => {
    server.use(...handlers(null));
    renderPage(`/p/prod/runbooks/${RB}`, compiler());
    expect(await screen.findByText('draft_step')).toBeInTheDocument();
    expect(screen.getByRole('note')).toHaveTextContent(/nothing is published yet/i);
    expect(screen.getByText('unpublished')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /start run/i })).toBeDisabled();
  });

  it('resolves a slug through the project catalog', async () => {
    server.use(...handlers(2));
    renderPage('/p/prod/runbooks/post-deploy', compiler());
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Post-deploy verification' }),
    ).toBeInTheDocument();
  });

  it('reads owners from the source', () => {
    expect(ownersOf(PUBLISHED)).toEqual(['team:oncall', 'usr_alice']);
    expect(ownersOf('not: [yaml')).toEqual([]);
    expect(ownersOf(undefined)).toEqual([]);
  });

  // UI-2 review round 1.
  it('shows an error with its code and a retry when the published source fails to load', async () => {
    let fail = true;
    server.use(...handlers(2));
    // Registered after (and so ahead of) the defaults.
    server.use(
      http.get('*/v1/runbooks/:rb/versions/:version', () =>
        fail
          ? problem(503, 'storage.unavailable')
          : HttpResponse.json({ source: PUBLISHED, runbook_version: 2 }),
      ),
    );
    renderPage(`/p/prod/runbooks/${RB}`, compiler());
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/source could not be loaded/i);
    expect(alert).toHaveTextContent('storage.unavailable');
    expect(screen.queryByText(/no source yet/i)).not.toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole('tab', { name: 'Source' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/source could not be loaded/i);
    expect(screen.queryByLabelText('Source of version 2')).not.toBeInTheDocument();

    fail = false;
    await user.click(screen.getByRole('button', { name: /retry/i }));
    expect(await screen.findByLabelText('Source of version 2')).toHaveTextContent('published_step');
  });

  it('shows an error, not "no source", when the draft fails to load', async () => {
    server.use(...handlers(null));
    server.use(http.get('*/v1/runbooks/:rb/draft', () => problem(500, 'internal.error')));
    renderPage(`/p/prod/runbooks/${RB}`, compiler());
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/source could not be loaded/i);
    expect(alert).toHaveTextContent('internal.error');
    expect(screen.queryByText(/no source yet/i)).not.toBeInTheDocument();
  });

  it('keeps one h1 on the Versions tab', async () => {
    server.use(...handlers(2));
    renderPage(`/p/prod/runbooks/${RB}?tab=versions`, compiler());
    await screen.findByRole('heading', { level: 1, name: 'Post-deploy verification' });
    await waitFor(() => expect(screen.queryByRole('status')).not.toBeInTheDocument());
    expect(screen.getAllByRole('heading', { level: 1 })).toHaveLength(1);
  });

  const twoProjects = (owner: string | null) => [
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [
          { id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' },
          { id: 'prj_2', org_id: 'org_1', slug: 'staging', title: 'Staging' },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/runbooks', ({ params }) =>
      HttpResponse.json({
        items:
          params.p === owner
            ? [{ runbook_id: RB, slug: 'post-deploy', origin: 'ui', archived: false, tags: [] }]
            : [],
        next_cursor: null,
      }),
    ),
  ];

  it("moves another project's runbook to that project's URL", async () => {
    server.use(...handlers(2));
    server.use(...twoProjects('prj_2'));
    const router = renderPage(`/p/prod/runbooks/${RB}?tab=source`, compiler());
    await waitFor(() => expect(router.state.location.pathname).toBe(`/p/staging/runbooks/${RB}`));
    expect(router.state.location.search).toEqual({ tab: 'source' });
  });

  it('says not found when the runbook is in none of your projects', async () => {
    server.use(...handlers(2));
    server.use(...twoProjects(null));
    const router = renderPage(`/p/prod/runbooks/${RB}`, compiler());
    expect(await screen.findByText(/not in this project/i)).toBeInTheDocument();
    expect(router.state.location.pathname).toBe(`/p/prod/runbooks/${RB}`);
    expect(
      screen.queryByRole('heading', { name: 'Post-deploy verification' }),
    ).not.toBeInTheDocument();
  });
});
