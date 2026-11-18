import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { OrgProvider } from '../context/OrgContext';
import { RunbookMemoryTab } from './RunbookMemoryTab';
import type { MemoryEntry, MemoryRevision } from './hooks';

afterEach(() => tokenStore.clear());

const RB = 'rb_test';

const sampleEntry: MemoryEntry = {
  id: 'mem_1',
  title: 'Redis cluster failover rule',
  body_markdown: 'Always verify replica sync before promoting.',
  status: 'active',
  revision: 1,
  scope: { kind: 'runbook', runbook_id: RB },
  source: { kind: 'person', user_id: 'usr_1' },
  last_writer: { kind: 'person', actor: 'human:usr_1' },
  locked: false,
  tags: ['redis', 'failover'],
  step_id: 'step_verify',
  created_at: new Date(Date.now() - 3600000).toISOString(),
  updated_at: new Date(Date.now() - 1800000).toISOString(),
};

const proposedEntry: MemoryEntry = {
  id: 'mem_2',
  title: 'Agent proposed note',
  body_markdown: 'Proposed connection pool settings.',
  status: 'proposed',
  revision: 1,
  scope: { kind: 'runbook', runbook_id: RB },
  source: { kind: 'agent', sponsor_user_id: 'usr_1', token_id: 'tok_agent1' },
  last_writer: { kind: 'agent', actor: 'agent:tok_agent1:usr_1' },
  locked: false,
  tags: ['pool'],
  created_at: new Date(Date.now() - 7200000).toISOString(),
  updated_at: new Date(Date.now() - 3600000).toISOString(),
};

interface CapturedCreateBody {
  title?: string;
  body_markdown?: string;
  tags?: string[];
}

interface CapturedPatchBody {
  action?: string;
  revision?: number;
}

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

function renderMemoryTab({
  canWrite = true,
  scopes: _scopes = ['runbooks:read', 'runbooks:write', 'executions:read'],
}: {
  canWrite?: boolean;
  scopes?: string[];
} = {}) {
  tokenStore.set('trb_pat_test');
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OrgProvider>
        <RunbookMemoryTab runbookId={RB} projectId="prj_1" canWrite={canWrite} />
      </OrgProvider>
    </QueryClientProvider>,
  );
}

describe('RunbookMemoryTab', () => {
  it('lists memory entries and searches with q', async () => {
    let capturedQuery: string | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', ({ request }) => {
        capturedQuery = new URL(request.url).searchParams.get('q');
        return HttpResponse.json({ items: [sampleEntry], next_cursor: null });
      }),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    expect(
      await screen.findByRole('heading', { level: 3, name: 'Redis cluster failover rule' }),
    ).toBeInTheDocument();
    expect(screen.getByText(/^By Alice/)).toBeInTheDocument();

    const searchInput = screen.getByLabelText('Search memory entries');
    await user.type(searchInput, 'failover');

    await waitFor(() => expect(capturedQuery).toBe('failover'));
  });

  it('handles cursor-based paging with Load more', async () => {
    let requestedCursor: string | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', ({ request }) => {
        const cursor = new URL(request.url).searchParams.get('cursor');
        if (cursor === 'page-2') {
          requestedCursor = cursor;
          return HttpResponse.json({
            items: [{ ...sampleEntry, id: 'mem_p2', title: 'Second page entry' }],
            next_cursor: null,
          });
        }
        return HttpResponse.json({
          items: [sampleEntry],
          next_cursor: 'page-2',
        });
      }),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    expect(
      await screen.findByRole('heading', { level: 3, name: 'Redis cluster failover rule' }),
    ).toBeInTheDocument();

    const loadMoreBtn = await screen.findByRole('button', { name: /load more entries/i });
    await user.click(loadMoreBtn);

    await waitFor(() => expect(requestedCursor).toBe('page-2'));
    expect(await screen.findByText('Second page entry')).toBeInTheDocument();
  });

  it('creates a new memory entry via POST /v1/runbooks/:rb/memory', async () => {
    let createdBody: CapturedCreateBody | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [sampleEntry], next_cursor: null }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
      http.post('*/v1/runbooks/:rb/memory', async ({ request }) => {
        createdBody = (await request.json()) as CapturedCreateBody;
        return HttpResponse.json(
          {
            ...sampleEntry,
            id: 'mem_new',
            title: createdBody.title as string,
            body_markdown: createdBody.body_markdown as string,
          },
          { status: 201 },
        );
      }),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: /new memory/i }));

    const dialog = await screen.findByRole('dialog', { name: /new memory entry/i });
    await user.type(within(dialog).getByLabelText(/^title$/i), 'New operating fact');
    await user.type(
      within(dialog).getByLabelText(/body/i),
      'Never restart without draining connections.',
    );
    await user.type(within(dialog).getByLabelText(/tags/i), 'ops, restart');

    await user.click(within(dialog).getByRole('button', { name: /save entry/i }));

    const body = await waitFor(() => {
      expect(createdBody).not.toBeNull();
      return createdBody!;
    });
    expect(body.title).toBe('New operating fact');
    expect(body.body_markdown).toBe('Never restart without draining connections.');
    expect(body.tags).toEqual(['ops', 'restart']);
  });

  it('edits an entry sending If-Match', async () => {
    let ifMatchHeader: string | null = null;
    let patchBody: CapturedPatchBody | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [sampleEntry], next_cursor: null }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
      http.patch('*/v1/memory/:mem', async ({ request }) => {
        ifMatchHeader = request.headers.get('if-match');
        patchBody = (await request.json()) as CapturedPatchBody;
        return HttpResponse.json({
          ...sampleEntry,
          revision: 2,
          title: 'Updated title',
        });
      }),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: /^edit$/i }));

    await screen.findByRole('dialog', { name: /edit memory entry/i });
    const titleInput = screen.getByLabelText(/^title$/i);
    await user.clear(titleInput);
    await user.type(titleInput, 'Updated title');

    await user.click(screen.getByRole('button', { name: /save changes/i }));

    await waitFor(() => expect(ifMatchHeader).toBe('"r1"'));
    const patch = await waitFor(() => {
      expect(patchBody).not.toBeNull();
      return patchBody!;
    });
    expect(patch.action).toBe('edit');
  });

  it('handles a 412 conflict with "This entry changed since you opened it"', async () => {
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [sampleEntry], next_cursor: null }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
      http.patch('*/v1/memory/:mem', () =>
        HttpResponse.json(
          { code: 'memory.conflict', detail: 'Stale revision' },
          { status: 412, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: /^edit$/i }));
    await user.click(screen.getByRole('button', { name: /save changes/i }));

    expect(await screen.findByText('This entry changed since you opened it.')).toBeInTheDocument();
  });

  it('accepts a proposal via PATCH /v1/memory/:mem', async () => {
    let patchBody: CapturedPatchBody | null = null;
    let ifMatch: string | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [proposedEntry], next_cursor: null }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
      http.patch('*/v1/memory/:mem', async ({ request }) => {
        ifMatch = request.headers.get('if-match');
        patchBody = (await request.json()) as CapturedPatchBody;
        return HttpResponse.json({
          ...proposedEntry,
          status: 'active',
          revision: 2,
        });
      }),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    const acceptBtn = await screen.findByRole('button', { name: /^accept$/i });
    await user.click(acceptBtn);

    const patch = await waitFor(() => {
      expect(patchBody).not.toBeNull();
      return patchBody!;
    });
    expect(patch.action).toBe('accept');
    expect(ifMatch).toBe('"r1"');
  });

  it('archives an active entry via PATCH /v1/memory/:mem', async () => {
    let patchBody: CapturedPatchBody | null = null;
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [sampleEntry], next_cursor: null }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
      http.patch('*/v1/memory/:mem', async ({ request }) => {
        patchBody = (await request.json()) as CapturedPatchBody;
        return HttpResponse.json({
          ...sampleEntry,
          status: 'archived',
          revision: 2,
        });
      }),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    const archiveBtn = await screen.findByRole('button', { name: /^archive$/i });
    await user.click(archiveBtn);

    const patch = await waitFor(() => {
      expect(patchBody).not.toBeNull();
      return patchBody!;
    });
    expect(patch.action).toBe('archive');
  });

  it('shows revision history and allows restoring a prior revision', async () => {
    let restorePatch: CapturedPatchBody | null = null;
    const history: MemoryRevision[] = [
      {
        revision: 1,
        action: 'create',
        actor: 'human:usr_1',
        actor_kind: 'person',
        at: new Date(Date.now() - 3600000).toISOString(),
        entry: sampleEntry,
      },
    ];

    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({
          items: [{ ...sampleEntry, revision: 2 }],
          next_cursor: null,
        }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: history, next_cursor: null, pruned: 0 }),
      ),
      http.patch('*/v1/memory/:mem', async ({ request }) => {
        restorePatch = (await request.json()) as CapturedPatchBody;
        return HttpResponse.json({
          ...sampleEntry,
          revision: 3,
        });
      }),
    );

    renderMemoryTab();
    const user = userEvent.setup();

    expect(
      await screen.findByRole('heading', { level: 4, name: /revision history/i }),
    ).toBeInTheDocument();
    expect(await screen.findByText('r1')).toBeInTheDocument();

    const restoreBtn = await screen.findByRole('button', { name: /restore/i });
    await user.click(restoreBtn);

    const restore = await waitFor(() => {
      expect(restorePatch).not.toBeNull();
      return restorePatch!;
    });
    expect(restore.action).toBe('restore');
    expect(restore.revision).toBe(1);
  });

  it('hides mutating actions for a principal without runbooks:write scope', async () => {
    server.use(
      ...baseHandlers(['runbooks:read']), // read-only scope
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [sampleEntry], next_cursor: null }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
    );

    renderMemoryTab({ canWrite: false, scopes: ['runbooks:read'] });

    expect(
      await screen.findByRole('heading', { level: 3, name: 'Redis cluster failover rule' }),
    ).toBeInTheDocument();
    // New memory button should be absent
    expect(screen.queryByRole('button', { name: /new memory/i })).not.toBeInTheDocument();
    // Edit and Archive buttons should be absent
    expect(screen.queryByRole('button', { name: /^edit$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^archive$/i })).not.toBeInTheDocument();
  });

  it('has no axe violations', async () => {
    server.use(
      ...baseHandlers(),
      http.get('*/v1/runbooks/:rb/memory', () =>
        HttpResponse.json({ items: [sampleEntry], next_cursor: null }),
      ),
      http.get('*/v1/memory/:mem/revisions', () =>
        HttpResponse.json({ items: [], next_cursor: null, pruned: 0 }),
      ),
    );

    const { container } = renderMemoryTab();

    expect(
      await screen.findByRole('heading', { level: 3, name: 'Redis cluster failover rule' }),
    ).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});
