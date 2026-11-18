import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { OrgProvider } from '../context/OrgContext';
import { CreateProjectDialog } from './CreateProjectDialog';

afterEach(() => tokenStore.clear());

const meAdmin = {
  user: { id: 'usr_admin', name: 'Admin Alice' },
  org: { id: 'org_1', title: 'Acme Corp', role: 'owner' },
  teams: [],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read'],
    projects: null,
    expires_at_ms: Date.now() + 3_600_000,
  },
};

function setupHandlers() {
  server.use(
    http.get('*/.well-known/runbook', () =>
      HttpResponse.json({ auth: { methods: ['email_code'] } }),
    ),
    http.get('*/v1/me', () => HttpResponse.json(meAdmin)),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme Corp' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
  );
}

describe('CreateProjectDialog', () => {
  it('creates a new project and passes axe check', async () => {
    tokenStore.set('trb_pat_admin');
    setupHandlers();
    let createdPayload: { title: string; slug: string } | null = null;

    server.use(
      http.post('*/v1/orgs/:org/projects', async ({ request }) => {
        createdPayload = (await request.json()) as { title: string; slug: string };
        return HttpResponse.json({
          id: 'prj_new',
          org_id: 'org_1',
          title: createdPayload.title,
          slug: createdPayload.slug,
        });
      }),
    );

    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    const { container } = render(
      <QueryClientProvider client={queryClient}>
        <OrgProvider>
          <CreateProjectDialog open={true} onOpenChange={() => {}} />
        </OrgProvider>
      </QueryClientProvider>,
    );

    expect(await screen.findByRole('heading', { name: /new project/i })).toBeInTheDocument();
    await expectNoAxeViolations(container);

    const user = userEvent.setup();
    const titleInput = screen.getByLabelText(/project title/i);
    await user.type(titleInput, 'Payments Platform');

    expect(screen.getByLabelText(/project slug/i)).toHaveValue('payments-platform');

    await user.click(screen.getByRole('button', { name: /create project/i }));

    await waitFor(() =>
      expect(createdPayload).toEqual({
        title: 'Payments Platform',
        slug: 'payments-platform',
      }),
    );
  });
});
