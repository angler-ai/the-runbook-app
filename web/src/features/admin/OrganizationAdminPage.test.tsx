import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { OrgProvider } from '../context/OrgContext';
import { OrganizationAdminPage } from './OrganizationAdminPage';

afterEach(() => tokenStore.clear());

const meOwner = {
  user: { id: 'usr_admin', name: 'Admin Alice' },
  org: { id: 'org_1', title: 'Acme Corp', role: 'owner' },
  teams: [],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read', 'executions:read', 'tokens:manage', 'project:admin'],
    projects: null,
    expires_at_ms: Date.now() + 3_600_000,
  },
};

const meMember = {
  user: { id: 'usr_member', name: 'Regular Bob' },
  org: { id: 'org_1', title: 'Acme Corp', role: 'member' },
  teams: [],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read', 'executions:read'],
    projects: null,
    expires_at_ms: Date.now() + 3_600_000,
  },
};

const sampleUsers = [
  { id: 'usr_admin', name: 'Admin Alice', active: true, role: 'owner' },
  { id: 'usr_dev', name: 'Dev David', active: true, role: 'member' },
  { id: 'usr_inactive', name: 'Old Ian', active: false, role: 'member' },
];

const sampleInvitations = [
  {
    id: 'inv_1',
    email: 'newhire@example.com',
    role: 'member',
    projects: [{ project: 'prod', role: 'editor' }],
    state: 'pending',
    created_at_ms: Date.now() - 3600000,
    expires_at_ms: Date.now() + 86400000 * 6,
    invited_by: 'usr_admin',
  },
];

const sampleTeams = [{ id: 'team_infra', slug: 'infra', title: 'Infrastructure', org_id: 'org_1' }];

const sampleTeamMembers = [{ user_id: 'usr_admin' }];

const sampleTokens = [
  {
    id: 'tok_svc_1',
    kind: 'service',
    name: 'Deployer Daemon',
    role: 'admin',
    projects: ['prod'],
    scopes: ['runbooks:read', 'executions:start'],
    expires_at_ms: Date.now() + 86400000 * 30,
    created_at_ms: Date.now() - 3600000,
    last_used_at_ms: Date.now() - 60000,
  },
  {
    id: 'tok_pat_1',
    kind: 'personal',
    name: 'Alice CLI',
    role: null,
    projects: null,
    scopes: ['runbooks:read'],
    expires_at_ms: null,
    created_at_ms: Date.now() - 3600000,
    last_used_at_ms: null,
  },
];

function setupHandlers(me = meOwner) {
  server.use(
    http.get('*/.well-known/runbook', () =>
      HttpResponse.json({ auth: { methods: ['email_code'] } }),
    ),
    http.get('*/v1/me', () => HttpResponse.json(me)),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme Corp' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [{ id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({ items: sampleUsers, next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/teams', () =>
      HttpResponse.json({ items: sampleTeams, next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/teams/:team/members', () =>
      HttpResponse.json({ items: sampleTeamMembers, next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/invitations', () =>
      HttpResponse.json({ items: sampleInvitations, next_cursor: null }),
    ),
    http.get('*/v1/tokens', () => HttpResponse.json({ items: sampleTokens, next_cursor: null })),
  );
}

function renderPage() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OrgProvider>
        <OrganizationAdminPage />
      </OrgProvider>
    </QueryClientProvider>,
  );
}

describe('OrganizationAdminPage', () => {
  it('renders members and pending invitations for an org owner, passing axe check', async () => {
    tokenStore.set('trb_pat_owner');
    setupHandlers(meOwner);
    const { container } = renderPage();

    expect(
      await screen.findByRole('heading', { name: /organization administration/i }),
    ).toBeInTheDocument();
    expect(await screen.findByText('Admin Alice')).toBeInTheDocument();
    expect(screen.getByText('Dev David')).toBeInTheDocument();
    expect(screen.getByText('newhire@example.com')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('shows "Ask an organization admin" when signed in as a non-admin member', async () => {
    tokenStore.set('trb_pat_member');
    setupHandlers(meMember);
    const { container } = renderPage();

    expect(
      await screen.findByRole('heading', { name: /ask an organization admin/i }),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /invite member/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('tab', { name: /service tokens/i })).not.toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('deactivates and reactivates a member', async () => {
    tokenStore.set('trb_pat_owner');
    setupHandlers(meOwner);
    let patchedActive: boolean | null = null;
    server.use(
      http.patch('*/v1/orgs/:org/users/:user', async ({ request }) => {
        const body = (await request.json()) as { active: boolean };
        patchedActive = body.active;
        return HttpResponse.json({
          id: 'usr_dev',
          name: 'Dev David',
          active: body.active,
          role: 'member',
        });
      }),
    );

    renderPage();
    expect(await screen.findByText('Dev David')).toBeInTheDocument();

    const user = userEvent.setup();
    const deactivateBtn = screen.getByRole('button', { name: /deactivate/i });
    await user.click(deactivateBtn);

    await waitFor(() => expect(patchedActive).toBe(false));
  });

  it('revokes a pending invitation', async () => {
    tokenStore.set('trb_pat_owner');
    setupHandlers(meOwner);
    let revokedInv: string | null = null;
    server.use(
      http.delete('*/v1/orgs/:org/invitations/:inv', ({ params }) => {
        revokedInv = String(params.inv);
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderPage();
    expect(await screen.findByText('newhire@example.com')).toBeInTheDocument();

    const user = userEvent.setup();
    const revokeBtn = screen.getByRole('button', {
      name: /revoke invitation for newhire@example.com/i,
    });
    await user.click(revokeBtn);

    await waitFor(() => expect(revokedInv).toBe('inv_1'));
  });

  it('navigates to Teams tab, adds and removes team member', async () => {
    tokenStore.set('trb_pat_owner');
    setupHandlers(meOwner);
    let removedUser: string | null = null;

    server.use(
      http.post('*/v1/orgs/:org/teams/:team/members', async ({ request }) => {
        const body = (await request.json()) as { user_id: string };
        return HttpResponse.json(body);
      }),
      http.delete('*/v1/orgs/:org/teams/:team/members/:user', ({ params }) => {
        removedUser = String(params.user);
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderPage();
    expect(await screen.findByText('Admin Alice')).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole('tab', { name: /teams/i }));

    expect(await screen.findByText('Infrastructure')).toBeInTheDocument();
    expect(screen.getByText('infra')).toBeInTheDocument();

    // Remove member
    const removeBtn = screen.getByRole('button', {
      name: /remove admin alice from infrastructure/i,
    });
    await user.click(removeBtn);
    await waitFor(() => expect(removedUser).toBe('usr_admin'));
  });

  it('navigates to Service Tokens tab and revokes a token', async () => {
    tokenStore.set('trb_pat_owner');
    setupHandlers(meOwner);
    let revokedTokenId: string | null = null;

    server.use(
      http.delete('*/v1/tokens/:id', ({ params }) => {
        revokedTokenId = String(params.id);
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderPage();
    expect(await screen.findByText('Admin Alice')).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole('tab', { name: /service tokens/i }));

    expect(await screen.findByText('Deployer Daemon')).toBeInTheDocument();
    expect(screen.queryByText('Alice CLI')).not.toBeInTheDocument(); // Personal tokens filtered out

    const revokeBtn = screen.getByRole('button', { name: /revoke deployer daemon/i });
    await user.click(revokeBtn);
    await waitFor(() => expect(revokedTokenId).toBe('tok_svc_1'));
  });

  it('shows refusal message on members and teams tabs when signed in with token lacking project:admin, passing axe check', async () => {
    tokenStore.set('trb_pat_no_admin');
    const meNoAdmin = {
      ...meOwner,
      credential: {
        ...meOwner.credential,
        scopes: ['runbooks:read', 'tokens:manage'],
      },
    };
    setupHandlers(meNoAdmin);
    const { container } = renderPage();

    expect(
      await screen.findByRole('heading', { name: /organization administration/i }),
    ).toBeInTheDocument();
    expect(
      await screen.findByText(/Sign in with email or Google to manage organization settings/i),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /invite member/i })).not.toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole('tab', { name: /teams/i }));
    expect(
      await screen.findByText(/Sign in with email or Google to manage organization settings/i),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /new team/i })).not.toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('shows refusal message on service tokens tab when signed in with token lacking tokens:manage, passing axe check', async () => {
    tokenStore.set('trb_pat_no_tokens');
    const meNoTokens = {
      ...meOwner,
      credential: {
        ...meOwner.credential,
        scopes: ['runbooks:read', 'project:admin'],
      },
    };
    setupHandlers(meNoTokens);
    const { container } = renderPage();

    expect(
      await screen.findByRole('heading', { name: /organization administration/i }),
    ).toBeInTheDocument();

    const user = userEvent.setup();
    const tab = await screen.findByRole('tab', { name: /service tokens/i });
    await user.click(tab);
    expect(
      await screen.findByText(/Sign in with email or Google to manage service tokens/i),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /new service token/i })).not.toBeInTheDocument();

    await expectNoAxeViolations(container);
  });
});
