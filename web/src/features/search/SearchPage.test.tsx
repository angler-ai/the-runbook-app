import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from '@tanstack/react-router';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { SearchPage } from './SearchPage';

function renderSearch(onQueryChange = () => {}) {
  tokenStore.set('trb_pat_test');
  const rootRoute = createRootRoute({ component: Outlet });
  const searchRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/search',
    component: () => <SearchPage initialQuery="" onQueryChange={onQueryChange} />,
  });
  const catchAll = createRoute({
    getParentRoute: () => rootRoute,
    path: '$',
    component: () => <p>elsewhere</p>,
  });
  const runbookRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/p/$project/runbooks/$rb/editor',
    component: () => <p>editor page</p>,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([searchRoute, runbookRoute, catchAll]),
    history: createMemoryHistory({ initialEntries: ['/search'] }),
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    router,
    ...render(
      <QueryClientProvider client={client}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    ),
  };
}

describe('universal search page', () => {
  it('groups hits by kind with their projects and opens across projects', async () => {
    let queried: string | null = null;
    server.use(
      http.get('*/v1/me', () =>
        HttpResponse.json({
          user: { id: 'usr_1', name: 'Alice' },
          org: { id: 'org_1', title: 'Acme', role: 'owner' },
          teams: [],
          credential: {
            kind: 'personal',
            actor_kind: 'human',
            scopes: ['runbooks:read'],
            projects: null,
            expires_at_ms: Date.now() + 3_600_000,
          },
        }),
      ),
      http.get('*/v1/orgs/:org/search', ({ request }) => {
        queried = new URL(request.url).searchParams.get('q');
        return HttpResponse.json({
          runbooks: [
            {
              project_id: 'prj_2',
              project_slug: 'staging',
              runbook_id: 'rb_9',
              slug: 'deploy-check',
              title: 'Deploy check',
            },
          ],
          runs: [
            {
              project_id: 'prj_1',
              project_slug: 'prod',
              id: 'exe_9',
              runbook_id: 'rb_1',
              status: 'waiting',
              title: 'Deploy to prod',
              tags: [],
              fields: {},
              created_by: 'usr_1',
              created_at_ms: 1,
              completed_at_ms: null,
            },
          ],
          steps: [],
          memory: [],
          skills: [],
        });
      }),
    );
    const { router } = renderSearch();
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText(/search everything/i), 'deploy');
    expect(await screen.findByText('Deploy check')).toBeInTheDocument();
    expect(screen.getByText('Deploy to prod')).toBeInTheDocument();
    expect(screen.getByText(/staging · deploy-check/)).toBeInTheDocument();
    expect(queried).toBe('deploy');
    await user.click(screen.getByRole('link', { name: /Deploy check/ }));
    expect(await screen.findByText('editor page')).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/p/staging/runbooks/rb_9/editor');
  });
});
