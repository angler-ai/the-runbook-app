import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { App } from '../App';
import { tokenStore } from '../api/token-store';
import { server } from '../test/msw/server';
import { createAppRouter } from './router';

const meBody = {
  user: { id: 'usr_1', name: 'Alice' },
  org: { id: 'org_1', title: 'Acme', role: 'owner' },
  teams: [{ id: 'team_1', slug: 'oncall' }],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read', 'executions:read'],
    projects: null,
    expires_at_ms: Date.now() + 3_600_000,
  },
};

function directoryHandlers() {
  return [
    http.get('*/v1/me', () => HttpResponse.json(meBody)),
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
      HttpResponse.json({
        items: [{ id: 'usr_1', name: 'Alice', active: true, role: 'owner' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/teams', () =>
      HttpResponse.json({
        items: [{ id: 'team_1', org_id: 'org_1', slug: 'oncall', title: 'Oncall' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
  ];
}

function renderAt(path: string) {
  const router = createAppRouter(createMemoryHistory({ initialEntries: [path] }));
  return { router, ...render(<App router={router} />) };
}

afterEach(() => tokenStore.clear());

describe('route guards and sign-in flow', () => {
  it('sends unauthenticated users to sign-in with their destination', async () => {
    const { router } = renderAt('/tasks');
    expect(await screen.findByRole('heading', { name: 'Sign in' })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/sign-in');
    expect(router.state.location.search).toMatchObject({ next: '/tasks' });
  });

  it('signs in and lands on the requested page inside the shell', async () => {
    server.use(...directoryHandlers());
    const { router } = renderAt('/tasks');
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText(/personal access token/i), 'trb_pat_good');
    await user.click(screen.getByRole('button', { name: /sign in/i }));
    expect(await screen.findByRole('heading', { name: 'My tasks' })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/tasks');
    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(await screen.findByText('Inbox is clear')).toBeInTheDocument();
  });

  it('sends a 401 mid-session back to sign-in with a reason and no token', async () => {
    server.use(
      http.get('*/v1/me', () =>
        HttpResponse.json(
          {
            code: 'auth.invalid_credential',
            status: 401,
            detail: 'revoked',
            type: 't',
            title: 't',
            instance: 'urn:request:req_9',
            errors: [],
          },
          { status: 401, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    tokenStore.set('trb_pat_revoked');
    const { router } = renderAt('/tasks');
    expect(await screen.findByText(/session ended/i)).toBeInTheDocument();
    await waitFor(() => expect(router.state.location.pathname).toBe('/sign-in'));
    expect(tokenStore.get()).toBeNull();
  });

  it('signs out from the shell: revoke, clear, explain', async () => {
    let deleteCalls = 0;
    server.use(
      ...directoryHandlers(),
      http.delete('*/v1/me/token', () => {
        deleteCalls += 1;
        return new HttpResponse(null, { status: 204 });
      }),
    );
    tokenStore.set('trb_pat_good');
    renderAt('/tasks');
    const user = userEvent.setup();
    // UI-1: sign-out lives in the account menu at the sidebar's foot.
    await user.click(await screen.findByRole('button', { name: /^account:/i }));
    await user.click(await screen.findByRole('button', { name: /sign out/i }));
    expect(await screen.findByText(/have signed out/i)).toBeInTheDocument();
    expect(deleteCalls).toBe(1);
    expect(tokenStore.get()).toBeNull();
    expect(sessionStorage.getItem('runbook.session.token.v1')).toBeNull();
  });

  it('drops a restored token that no longer qualifies', async () => {
    server.use(
      http.get('*/v1/me', () =>
        HttpResponse.json({
          ...meBody,
          credential: { ...meBody.credential, scopes: ['tokens:manage'] },
        }),
      ),
    );
    tokenStore.set('trb_pat_wide');
    const { router } = renderAt('/tasks');
    expect(await screen.findByText(/session ended/i)).toBeInTheDocument();
    await waitFor(() => expect(router.state.location.pathname).toBe('/sign-in'));
    expect(tokenStore.get()).toBeNull();
  });

  it('has no token settings route (F90)', async () => {
    server.use(...directoryHandlers());
    tokenStore.set('trb_pat_good');
    renderAt('/settings/tokens');
    expect(await screen.findByRole('heading', { name: 'Page not found' })).toBeInTheDocument();
  });

  it('routes to organization settings', async () => {
    server.use(
      ...directoryHandlers(),
      http.get('*/v1/orgs/:org/invitations', () => HttpResponse.json({ items: [], next_cursor: null })),
      http.get('*/v1/tokens', () => HttpResponse.json({ items: [], next_cursor: null })),
    );
    tokenStore.set('trb_pat_good');
    const { router } = renderAt('/settings/organization');
    expect(await screen.findByRole('heading', { name: /organization administration/i })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/settings/organization');
  });

  it('routes to project settings', async () => {
    server.use(
      ...directoryHandlers(),
      http.get('*/v1/projects/:p/members', () => HttpResponse.json({ items: [], next_cursor: null })),
    );
    tokenStore.set('trb_pat_good');
    const { router } = renderAt('/p/prod/settings');
    expect(await screen.findByRole('heading', { name: /production settings/i })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/p/prod/settings');
  });

  it('shows "Page not found" inside the app frame, with a way back (walkthrough #12)', async () => {
    server.use(...directoryHandlers());
    tokenStore.set('trb_pat_good');
    const { router } = renderAt('/no/such/page');
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Page not found' }),
    ).toBeInTheDocument();
    // The authenticated shell: primary navigation and the person's controls.
    const nav = await screen.findByRole('navigation', { name: 'Primary' });
    expect(await within(nav).findByRole('link', { name: /inbox/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^account:/i })).toBeInTheDocument();
    const back = screen.getByRole('link', { name: 'Go to My tasks' });
    await userEvent.setup().click(back);
    await waitFor(() => expect(router.state.location.pathname).toBe('/tasks'));
  });

  it('sends unauthenticated users trying to access /device to sign-in with next=/device', async () => {
    const { router } = renderAt('/device?user_code=ABCD-1234');
    expect(await screen.findByRole('heading', { name: 'Sign in' })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/sign-in');
    expect(router.state.location.search).toMatchObject({ next: '/device?user_code=ABCD-1234' });
  });

  it('renders /device when authenticated', async () => {
    server.use(...directoryHandlers());
    tokenStore.set('trb_pat_good');
    const { router } = renderAt('/device');
    expect(await screen.findByRole('heading', { level: 1, name: 'Device approval' })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/device');
  });

  it('renders /settings inside the shell when authenticated', async () => {
    server.use(
      ...directoryHandlers(),
      http.get('*/api/auth/list-sessions', () => HttpResponse.json([])),
      http.get('*/v1/tokens', () => HttpResponse.json({ items: [] })),
      http.get('*/v1/me/apps', () => HttpResponse.json({ items: [] })),
    );
    tokenStore.set('trb_pat_good');
    const { router } = renderAt('/settings');
    expect(await screen.findByRole('heading', { level: 1, name: 'Account settings' })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/settings');
    expect(screen.getByRole('tab', { name: /sessions/i })).toBeInTheDocument();
  });

  it('renders skills and webhooks routes inside the shell', async () => {
    server.use(
      ...directoryHandlers(),
      http.get('*/v1/projects/:p/skills', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
      http.get('*/v1/projects/:p/webhooks', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
      http.get('*/v1/projects/:p/members', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
    );
    tokenStore.set('trb_pat_good');
    const { router: skillsRouter } = renderAt('/p/prod/skills');
    expect(await screen.findByRole('heading', { name: 'Skills' })).toBeInTheDocument();
    expect(skillsRouter.state.location.pathname).toBe('/p/prod/skills');

    const { router: webhooksRouter } = renderAt('/p/prod/webhooks');
    expect(await screen.findByRole('heading', { name: 'Webhooks' })).toBeInTheDocument();
    expect(webhooksRouter.state.location.pathname).toBe('/p/prod/webhooks');
  });
});

