import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { server } from '../../test/msw/server';

const entry = (id: string, title: string, status = 'waiting') => ({
  id,
  runbook_id: 'rb_1',
  status,
  title,
  tags: ['prod'],
  fields: {},
  created_by: 'usr_1',
  created_at_ms: Date.now() - 60_000,
  completed_at_ms: null,
});

function handlers(seen: URLSearchParams[]) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'owner' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes: ['runbooks:read', 'executions:read'],
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
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({
        items: [
          {
            runbook_id: 'rb_1',
            slug: 'post-deploy',
            origin: 'ui',
            archived: false,
            title: 'Post-deploy',
            latest_version: 3,
          },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/executions', ({ request }) => {
      const params = new URL(request.url).searchParams;
      seen.push(params);
      if (params.get('cursor') === 'page-2') {
        return HttpResponse.json({ items: [entry('exe_2', 'Second run')], next_cursor: null });
      }
      return HttpResponse.json({ items: [entry('exe_1', 'First run')], next_cursor: 'page-2' });
    }),
  ];
}

afterEach(() => tokenStore.clear());

describe('execution list filters and paging', () => {
  it('round-trips status/runbook/tag filters through the URL and pages by cursor', async () => {
    const seen: URLSearchParams[] = [];
    server.use(...handlers(seen));
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(
      createMemoryHistory({ initialEntries: ['/p/prod/executions?status=waiting&tags=prod'] }),
    );
    render(<App router={router} />);
    expect(await screen.findByText('First run')).toBeInTheDocument();
    // Inbound: URL filters reached the search query (waiting included, R50).
    expect(seen[0]?.get('status')).toBe('waiting');
    expect(seen[0]?.get('tags')).toBe('prod');

    const user = userEvent.setup();
    await user.click(screen.getByRole('checkbox', { name: /^running$/i }));
    expect(await screen.findByText('First run')).toBeInTheDocument();
    // Outbound: toggling a status writes the URL and the next query.
    expect(router.state.location.search).toMatchObject({ status: 'waiting,running', tags: 'prod' });
    expect(seen.at(-1)?.get('status')).toBe('waiting,running');

    // D22 cursor paging.
    await user.click(screen.getByRole('button', { name: /load more/i }));
    expect(await screen.findByText('Second run')).toBeInTheDocument();
    expect(seen.at(-1)?.get('cursor')).toBe('page-2');
    expect(screen.getByText('First run')).toBeInTheDocument();
  });
});

describe('execution list runbook filter (U20)', () => {
  it('picks the runbook from the catalog instead of free text', async () => {
    const seen: URLSearchParams[] = [];
    server.use(...handlers(seen));
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/executions'] }));
    render(<App router={router} />);
    expect(await screen.findByText('First run')).toBeInTheDocument();
    const user = userEvent.setup();
    // The app's Select lists its options once opened.
    await user.click(screen.getByRole('combobox', { name: /filter by runbook/i }));
    expect(await screen.findByRole('option', { name: 'All runbooks' })).toBeInTheDocument();
    await user.click(await screen.findByRole('option', { name: 'Post-deploy' }));
    await waitFor(() => expect(seen.at(-1)?.get('runbook')).toBe('post-deploy'));
    expect(router.state.location.search).toMatchObject({ runbook: 'post-deploy' });
  });

  it('reads an unknown runbook filter as no such runbook, not a load failure', async () => {
    const seen: URLSearchParams[] = [];
    server.use(
      http.get('*/v1/projects/:p/executions', ({ request }) => {
        const params = new URL(request.url).searchParams;
        seen.push(params);
        if (params.get('runbook') === 'gone') {
          return HttpResponse.json(
            {
              type: 'urn:runbook:problem:resource.not_found',
              title: 'resource.not_found',
              status: 404,
              detail: 'resource.not_found',
              instance: 'urn:request:req_9',
              code: 'resource.not_found',
              errors: [],
            },
            { status: 404, headers: { 'Content-Type': 'application/problem+json' } },
          );
        }
        return HttpResponse.json({ items: [entry('exe_1', 'First run')], next_cursor: null });
      }),
      ...handlers(seen),
    );
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(
      createMemoryHistory({ initialEntries: ['/p/prod/executions?runbook=gone'] }),
    );
    render(<App router={router} />);
    expect(
      await screen.findByText(/no runbook .*gone.* in this project/i, {}, { timeout: 4000 }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/executions could not be loaded/i)).not.toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: /show all runbooks/i }));
    expect(await screen.findByText('First run')).toBeInTheDocument();
  });
});
