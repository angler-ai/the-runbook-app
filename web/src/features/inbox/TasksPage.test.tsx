import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { sessionStore, tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { server } from '../../test/msw/server';
import { INBOX_MAX_PAGES, INBOX_POLL_MS } from './hooks';

function handlers(counter: { tasks: number }) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'owner' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes: ['executions:read'],
          projects: null,
          expires_at_ms: Date.now() + 3_600_000,
        },
      }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/me/tasks', () => {
      counter.tasks += 1;
      return HttpResponse.json({
        items: [
          {
            project_id: 'prj_1',
            execution_id: 'exe_1',
            step_run_id: 'sr_1',
            path: 'decide.signoff',
            title: 'Decide whether to roll back',
            execution_title: 'Verify api',
            kind: 'approval',
          },
        ],
        next_cursor: null,
      });
    }),
  ];
}

afterEach(() => {
  tokenStore.clear();
  vi.useRealTimers();
});

describe('inbox polling', () => {
  it('shows grouped items with a sidebar count and refetches every 30 seconds', async () => {
    const counter = { tasks: 0 };
    server.use(...handlers(counter));
    tokenStore.set('trb_pat_good');
    vi.useFakeTimers();
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/tasks'] }));
    render(<App router={router} />);
    await vi.waitFor(() =>
      expect(screen.getByText('Decide whether to roll back')).toBeInTheDocument(),
    );
    const before = counter.tasks;
    expect(before).toBeGreaterThanOrEqual(1);
    await vi.advanceTimersByTimeAsync(INBOX_POLL_MS + 100);
    await vi.waitFor(() => expect(counter.tasks).toBeGreaterThan(before));
  });

  it('loads for an email or Google session too (no pasted token)', async () => {
    const counter = { tasks: 0 };
    server.use(...handlers(counter));
    sessionStore.setCookieSession();
    try {
      const router = createAppRouter(createMemoryHistory({ initialEntries: ['/tasks'] }));
      render(<App router={router} />);
      expect(await screen.findByText('Decide whether to roll back')).toBeInTheDocument();
      expect(counter.tasks).toBeGreaterThanOrEqual(1);
    } finally {
      sessionStore.clearCookieSession();
    }
  });
});

const task = (n: number, kind: 'approval' | 'human_task') => ({
  project_id: 'prj_1',
  execution_id: `exe_${n}`,
  step_run_id: `sr_${n}`,
  path: `step.${n}`,
  title: `Task number ${n}`,
  execution_title: `Run ${n}`,
  kind,
});

describe('inbox paging (U19)', () => {
  it('follows the cursor to the last page and counts every task', async () => {
    const cursors: (string | null)[] = [];
    server.use(
      // The shell renders its links (and the count) once a project is known.
      http.get('*/v1/orgs/:org/projects', () =>
        HttpResponse.json({
          items: [{ id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' }],
          next_cursor: null,
        }),
      ),
      ...handlers({ tasks: 0 }).slice(0, -1),
      http.get('*/v1/me/tasks', ({ request }) => {
        const cursor = new URL(request.url).searchParams.get('cursor');
        cursors.push(cursor);
        if (cursor === null) {
          return HttpResponse.json({
            items: [task(1, 'approval'), task(2, 'human_task')],
            next_cursor: 'p2',
          });
        }
        if (cursor === 'p2') {
          return HttpResponse.json({ items: [task(3, 'human_task')], next_cursor: 'p3' });
        }
        return HttpResponse.json({ items: [task(4, 'approval')], next_cursor: null });
      }),
    );
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/tasks'] }));
    render(<App router={router} />);
    expect(await screen.findByText('Task number 4')).toBeInTheDocument();
    for (const n of [1, 2, 3]) expect(screen.getByText(`Task number ${n}`)).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Approvals (2)' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Tasks (2)' })).toBeInTheDocument();
    const myTasks = await screen.findByRole('link', { name: /inbox/i });
    expect(myTasks.querySelector('[data-slot="sidebar-nav-count"]')).toHaveTextContent('4');
    expect(cursors.slice(0, 3)).toEqual([null, 'p2', 'p3']);
  });
});

describe('inbox badge past the paging cap (U20)', () => {
  it('shows 1000+ in the sidebar when the inbox has more than INBOX_MAX_PAGES pages', async () => {
    let pages = 0;
    server.use(
      http.get('*/v1/orgs/:org/projects', () =>
        HttpResponse.json({
          items: [{ id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' }],
          next_cursor: null,
        }),
      ),
      ...handlers({ tasks: 0 }).slice(0, -1),
      http.get('*/v1/projects/:p/runbooks', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
      http.get('*/v1/projects/:p/executions', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
      // Every page is full and has another after it: the read stops at the cap.
      http.get('*/v1/me/tasks', () => {
        pages += 1;
        const base = pages * 50;
        return HttpResponse.json({
          items: Array.from({ length: 50 }, (_, i) => task(base + i, 'human_task')),
          next_cursor: `p${pages + 1}`,
        });
      }),
    );
    tokenStore.set('trb_pat_good');
    // Away from My tasks, so only the sidebar shows the count.
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
    render(<App router={router} />);
    const myTasks = await screen.findByRole('link', { name: /inbox/i });
    await vi.waitFor(
      () =>
        expect(myTasks.querySelector('[data-slot="sidebar-nav-count"]')).toHaveTextContent(
          /^1000\+$/,
        ),
      { timeout: 3000 },
    );
    expect(pages).toBe(INBOX_MAX_PAGES);
  });
});
