import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { OrgProvider } from '../context/OrgContext';
import { ProjectMemoryPage } from './ProjectMemoryPage';
import type { MemoryEntry } from './hooks';

afterEach(() => tokenStore.clear());

const projectEntry: MemoryEntry = {
  id: 'mem_prj_1',
  title: 'Global incident escalation playbook',
  body_markdown: 'Always page the secondary on-call if no response within 5 minutes.',
  status: 'active',
  revision: 1,
  scope: { kind: 'project' },
  source: { kind: 'person', user_id: 'usr_1' },
  last_writer: { kind: 'person', actor: 'human:usr_1' },
  locked: false,
  tags: ['escalation', 'oncall'],
  created_at: new Date(Date.now() - 3600000).toISOString(),
  updated_at: new Date(Date.now() - 1800000).toISOString(),
};

const runbookEntry: MemoryEntry = {
  id: 'mem_rb_1',
  title: 'Database replica failover caution',
  body_markdown: 'Check lag before promotion.',
  status: 'active',
  revision: 1,
  scope: { kind: 'runbook', runbook_id: 'rb_db' },
  source: { kind: 'agent', sponsor_user_id: 'usr_1', token_id: 'tok_agent1' },
  last_writer: { kind: 'agent', actor: 'agent:tok_agent1:usr_1' },
  locked: false,
  tags: ['database', 'failover'],
  created_at: new Date(Date.now() - 7200000).toISOString(),
  updated_at: new Date(Date.now() - 3600000).toISOString(),
};

const proposedEntry: MemoryEntry = {
  id: 'mem_prop_1',
  title: 'Proposed connection pooling adjustment',
  body_markdown: 'Increase pool size to 50.',
  status: 'proposed',
  revision: 1,
  scope: { kind: 'project' },
  source: { kind: 'agent', sponsor_user_id: 'usr_1', token_id: 'tok_agent1' },
  last_writer: { kind: 'agent', actor: 'agent:tok_agent1:usr_1' },
  locked: false,
  tags: ['pool'],
  created_at: new Date(Date.now() - 7200000).toISOString(),
  updated_at: new Date(Date.now() - 3600000).toISOString(),
};

function baseHandlers(scopes: string[] = ['runbooks:read', 'runbooks:write', 'executions:read']) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'owner' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes,
          projects: null,
          expires_at_ms: Date.now() + 3_600_000,
        },
      }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [{ id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({
        items: [
          {
            runbook_id: 'rb_db',
            slug: 'database-ops',
            title: 'Database Operations',
            origin: 'manual',
            archived: false,
            draft_ahead: false,
            tags: [],
          },
          {
            runbook_id: 'rb_web',
            slug: 'web-deploy',
            title: 'Web Deployment',
            origin: 'manual',
            archived: false,
            draft_ahead: false,
            tags: [],
          },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({ items: [{ id: 'usr_1', name: 'Alice' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/projects/:p/members', () =>
      HttpResponse.json({ items: [{ user_id: 'usr_1', role: 'admin' }], next_cursor: null }),
    ),
  ];
}

function renderProjectMemoryPage() {
  tokenStore.set('trb_pat_test');
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OrgProvider>
        <ProjectMemoryPage project="prod" />
      </OrgProvider>
    </QueryClientProvider>,
  );
}

describe('ProjectMemoryPage', () => {
  it('lists project and runbook memory entries and passes axe check', async () => {
    server.use(
      ...baseHandlers(),
      http.get('*/v1/projects/:p/memory', () => {
        return HttpResponse.json({ items: [projectEntry, runbookEntry], next_cursor: null });
      }),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
    );

    const { container } = renderProjectMemoryPage();

    expect(
      await screen.findByRole('heading', { level: 1, name: 'Production memory' }),
    ).toBeInTheDocument();

    const listRegion = await screen.findByRole('list', { name: 'Memory entries' });
    expect(within(listRegion).getByText('Global incident escalation playbook')).toBeInTheDocument();
    expect(within(listRegion).getByText('Database replica failover caution')).toBeInTheDocument();
    expect(within(listRegion).getByText('Database Operations')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('searches memory with q parameter and filters by runbook', async () => {
    let capturedQ: string | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/projects/:p/memory', ({ request }) => {
        capturedQ = new URL(request.url).searchParams.get('q');
        return HttpResponse.json({ items: [projectEntry, runbookEntry], next_cursor: null });
      }),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
    );

    renderProjectMemoryPage();
    const user = userEvent.setup();

    const listRegion = await screen.findByRole('list', { name: 'Memory entries' });
    expect(within(listRegion).getByText('Global incident escalation playbook')).toBeInTheDocument();

    const searchInput = screen.getByLabelText('Search memory entries');
    await user.type(searchInput, 'escalation');

    await waitFor(() => expect(capturedQ).toBe('escalation'));
  });

  it('filters by status via select', async () => {
    let capturedStatus: string | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/projects/:p/memory', ({ request }) => {
        capturedStatus = new URL(request.url).searchParams.get('status');
        return HttpResponse.json({ items: [proposedEntry], next_cursor: null });
      }),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
    );

    renderProjectMemoryPage();
    const user = userEvent.setup();

    const listRegion = await screen.findByRole('list', { name: 'Memory entries' });
    expect(
      within(listRegion).getByText('Proposed connection pooling adjustment'),
    ).toBeInTheDocument();

    const statusTrigger = screen.getByLabelText('Filter by status');
    await user.click(statusTrigger);
    await user.click(screen.getByRole('option', { name: 'Proposed' }));

    await waitFor(() => expect(capturedStatus).toBe('proposed'));
  });

  it('creates project-level memory entry', async () => {
    let createdPayload: { title?: string; body_markdown?: string; tags?: string[] } | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/projects/:p/memory', () => {
        return HttpResponse.json({ items: [projectEntry], next_cursor: null });
      }),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
      http.post('*/v1/projects/:p/memory', async ({ request }) => {
        createdPayload = (await request.json()) as typeof createdPayload;
        const newEntry: MemoryEntry = {
          id: 'mem_new_1',
          title: createdPayload?.title ?? '',
          body_markdown: createdPayload?.body_markdown ?? '',
          status: 'active',
          revision: 1,
          scope: { kind: 'project' },
          source: { kind: 'person', user_id: 'usr_1' },
          last_writer: { kind: 'person', actor: 'human:usr_1' },
          locked: false,
          tags: createdPayload?.tags ?? [],
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        };
        return HttpResponse.json(newEntry, { status: 201 });
      }),
    );

    renderProjectMemoryPage();
    const user = userEvent.setup();

    const listRegion = await screen.findByRole('list', { name: 'Memory entries' });
    expect(within(listRegion).getByText('Global incident escalation playbook')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /new memory/i }));

    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByLabelText(/title/i), 'API Rate Limiting Guidelines');
    await user.type(
      within(dialog).getByLabelText(/body/i),
      'Clients exceeding 100 req/s receive 429.',
    );
    await user.type(within(dialog).getByLabelText(/tags/i), 'api, rate-limit');

    await user.click(within(dialog).getByRole('button', { name: /save entry/i }));

    await waitFor(() => {
      expect(createdPayload).toEqual({
        title: 'API Rate Limiting Guidelines',
        body_markdown: 'Clients exceeding 100 req/s receive 429.',
        tags: ['api', 'rate-limit'],
        step_id: undefined,
        propose: undefined,
      });
    });
  });

  it('renders empty state when there are no entries, passing axe check', async () => {
    server.use(
      ...baseHandlers(),
      http.get('*/v1/projects/:p/memory', () => {
        return HttpResponse.json({ items: [], next_cursor: null });
      }),
    );

    const { container } = renderProjectMemoryPage();

    expect(await screen.findByText('No memory entries yet')).toBeInTheDocument();
    expect(
      screen.getByText('Save run context, operating facts and learned knowledge for this project.'),
    ).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });
});
