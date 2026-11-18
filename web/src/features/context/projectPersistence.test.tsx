// UI-2: the project the person chose stays chosen. Pages without a project
// in their URL (/tasks, /x/$exe) used to fall back to the first project,
// and the sidebar's links followed it.
import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { server } from '../../test/msw/server';
import { fixtures } from '../../test/msw/fixtures';

afterEach(() => {
  tokenStore.clear();
  localStorage.clear();
});

const empty = () => HttpResponse.json({ items: [], next_cursor: null });

function handlers() {
  const document = fixtures.executionDocument as { header: Record<string, unknown> };
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
        items: [
          { id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' },
          { id: 'prj_2', org_id: 'org_1', slug: 'sandbox', title: 'Sandbox' },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', empty),
    http.get('*/v1/orgs/:org/teams', empty),
    http.get('*/v1/me/tasks', empty),
    http.get('*/v1/projects/:p/runbooks', empty),
    http.get('*/v1/projects/:p/executions', empty),
    // A run in the sandbox project.
    http.get('*/v1/executions/:exe/report', () =>
      HttpResponse.json({ ...document, header: { ...document.header, project_id: 'prj_2' } }),
    ),
  ];
}

function renderAt(path: string) {
  tokenStore.set('trb_pat_good');
  const router = createAppRouter(createMemoryHistory({ initialEntries: [path] }));
  return { router, ...render(<App router={router} />) };
}

async function expectSandbox() {
  const nav = await screen.findByRole('navigation', { name: 'Primary' });
  await waitFor(() =>
    expect(within(nav).getByRole('link', { name: /^home/i })).toHaveAttribute('href', '/p/sandbox'),
  );
  expect(within(nav).getByRole('link', { name: /^runbooks/i })).toHaveAttribute(
    'href',
    '/p/sandbox/runbooks',
  );
  expect(within(nav).getByRole('link', { name: /^runs/i })).toHaveAttribute(
    'href',
    '/p/sandbox/executions',
  );
  expect(screen.getByRole('button', { name: /^project: sandbox/i })).toBeInTheDocument();
}

describe('the current project', () => {
  it('stays the one chosen across every nav item and a run page', async () => {
    server.use(...handlers());
    const { router } = renderAt('/p/prod/runbooks');
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^project: production/i }));
    await user.click(await screen.findByRole('menuitem', { name: /sandbox/i }));
    // The switcher keeps the section: the sandbox's runbooks.
    await waitFor(() => expect(router.state.location.pathname).toBe('/p/sandbox/runbooks'));
    await expectSandbox();

    const nav = screen.getByRole('navigation', { name: 'Primary' });
    for (const [name, path] of [
      [/^home/i, '/p/sandbox'],
      [/^runs/i, '/p/sandbox/executions'],
      [/^inbox/i, '/tasks'],
      [/^runbooks/i, '/p/sandbox/runbooks'],
    ] as const) {
      await user.click(within(nav).getByRole('link', { name }));
      await waitFor(() => expect(router.state.location.pathname).toBe(path));
      await expectSandbox();
    }
    // The inbox has no project in its URL: still the sandbox.
    await user.click(within(nav).getByRole('link', { name: /^inbox/i }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/tasks'));
    await expectSandbox();
    // A run page takes its run's project (the sandbox here).
    await router.navigate({ to: '/x/$exe', params: { exe: 'exe_1' } });
    await expectSandbox();
  });

  it('survives a reload on a page without a project', async () => {
    server.use(...handlers());
    localStorage.setItem('rb.project.org_1', 'sandbox');
    renderAt('/tasks');
    await expectSandbox();
  });

  it('falls back to the first project only when nothing is known', async () => {
    server.use(...handlers());
    renderAt('/tasks');
    const nav = await screen.findByRole('navigation', { name: 'Primary' });
    await waitFor(() =>
      expect(within(nav).getByRole('link', { name: /^home/i })).toHaveAttribute('href', '/p/prod'),
    );
  });
});
