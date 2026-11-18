import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { OrgProvider } from '../context/OrgContext';
import { ProjectSettingsPage } from './ProjectSettingsPage';

afterEach(() => tokenStore.clear());

const meAdmin = {
  user: { id: 'usr_admin', name: 'Admin Alice' },
  org: { id: 'org_1', title: 'Acme Corp', role: 'owner' },
  teams: [],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read', 'executions:read', 'project:admin'],
    projects: null,
    expires_at_ms: Date.now() + 3_600_000,
  },
};

const meViewer = {
  user: { id: 'usr_dev', name: 'Dev David' },
  org: { id: 'org_1', title: 'Acme Corp', role: 'member' },
  teams: [],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read'],
    projects: null,
    expires_at_ms: Date.now() + 3_600_000,
  },
};

const sampleProjectMembers = [
  { user_id: 'usr_admin', role: 'admin' as const },
  { user_id: 'usr_dev', role: 'editor' as const },
];

const sampleUsers = [
  { id: 'usr_admin', name: 'Admin Alice', active: true, role: 'owner' as const },
  { id: 'usr_dev', name: 'Dev David', active: true, role: 'member' as const },
  { id: 'usr_other', name: 'Other Oliver', active: true, role: 'member' as const },
];

function setupHandlers(me = meAdmin) {
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
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/projects/:p/members', () =>
      HttpResponse.json({ items: sampleProjectMembers, next_cursor: null }),
    ),
  );
}

function renderPage() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OrgProvider>
        <ProjectSettingsPage />
      </OrgProvider>
    </QueryClientProvider>,
  );
}

describe('ProjectSettingsPage', () => {
  it('renders project members for a project admin, passing axe check', async () => {
    tokenStore.set('trb_pat_admin');
    setupHandlers(meAdmin);
    const { container } = renderPage();

    expect(
      await screen.findByRole('heading', { name: /production settings/i }),
    ).toBeInTheDocument();
    expect(await screen.findByText('Admin Alice')).toBeInTheDocument();
    expect(screen.getByText('Dev David')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('shows refusal message when token lacks project:admin scope, passing axe check', async () => {
    tokenStore.set('trb_pat_viewer');
    setupHandlers(meViewer);
    const { container } = renderPage();

    expect(
      await screen.findByRole('heading', { name: /production settings/i }),
    ).toBeInTheDocument();
    expect(
      await screen.findByText(/Sign in with email or Google to manage project settings/i),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /add project member/i })).not.toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('removes a project member', async () => {
    tokenStore.set('trb_pat_admin');
    setupHandlers(meAdmin);
    let revokedUser: string | null = null;
    server.use(
      http.delete('*/v1/projects/:p/members/:user', ({ params }) => {
        revokedUser = String(params.user);
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderPage();
    expect(await screen.findByText('Dev David')).toBeInTheDocument();

    const user = userEvent.setup();
    const removeBtn = screen.getByRole('button', { name: /remove dev david from project/i });
    await user.click(removeBtn);

    await waitFor(() => expect(revokedUser).toBe('usr_dev'));
  });
});
