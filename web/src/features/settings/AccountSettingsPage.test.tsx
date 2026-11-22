import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { sessionStore, tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { TestRouter } from '../../test/router';
import { OrgProvider } from '../context/OrgContext';
import { AccountSettingsPage } from './AccountSettingsPage';

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <OrgProvider>
        <TestRouter>
          <AccountSettingsPage />
        </TestRouter>
      </OrgProvider>
    </QueryClientProvider>,
  );
}

const mockMe = {
  user: { id: 'usr_1', name: 'Alice' },
  org: { id: 'org_1', title: 'Acme', role: 'owner' },
  teams: [],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['tokens:manage', 'runbooks:read'],
    projects: null,
    expires_at_ms: null,
  },
};

const sampleSessions = [
  {
    id: 'sess_1',
    token: 'tok_sess_1',
    createdAt: Date.now() - 3600000,
    expiresAt: Date.now() + 86400000,
    ipAddress: '192.168.1.10',
    userAgent: 'Chrome on macOS',
  },
  {
    id: 'sess_2',
    token: 'tok_sess_2',
    createdAt: Date.now() - 7200000,
    expiresAt: Date.now() + 86400000,
    ipAddress: '10.0.0.5',
    userAgent: 'Firefox on Linux',
  },
];

const sampleTokens = [
  {
    id: 'tok_00000000000000000000000001',
    kind: 'personal',
    name: 'laptop-cli',
    scopes: ['runbooks:read', 'executions:read'],
    created_at_ms: Date.now() - 86400000,
    expires_at_ms: Date.now() + 86400000 * 30,
    last_used_at_ms: Date.now() - 3600000,
  },
];

const sampleApps = [
  {
    client_id: 'app_mcp_client',
    client_name: 'Claude Desktop MCP',
    redirect_hosts: ['127.0.0.1:4000'],
    scopes: ['runbooks:read', 'executions:start'],
    created_at_ms: Date.now() - 86400000 * 5,
    expires_at_ms: Date.now() + 86400000 * 85,
    last_used_at_ms: Date.now() - 7200000,
  },
];

function baseHandlers() {
  return [
    http.get('*/v1/me', () => HttpResponse.json(mockMe)),
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
    http.get('*/api/auth/list-sessions', () => HttpResponse.json(sampleSessions)),
    http.get('*/v1/tokens', () => HttpResponse.json({ items: sampleTokens })),
    http.get('*/v1/me/apps', () => HttpResponse.json({ items: sampleApps })),
    http.get('*/v1/me/security', () =>
      HttpResponse.json({
        mfa_required: false,
        mfa_partial: false,
        mfa_enrolled: false,
        mfa_verified_at: null,
        mfa_method: null,
        allow_passkeys: true,
        allow_authenticator: true,
        fresh_minutes: 10,
        authenticator_enrolled: false,
        passkey_count: 0,
      }),
    ),
  ];
}

afterEach(() => {
  tokenStore.clear();
  sessionStore.clearCookieSession();
});

describe('AccountSettingsPage under PAT sign-in', () => {
  it('displays F90 refusal message on the personal tokens tab', async () => {
    server.use(...baseHandlers());
    tokenStore.set('trb_pat_good');

    const user = userEvent.setup();
    const { container } = renderPage();

    // Click on Personal tokens tab
    const tokensTab = await screen.findByRole('tab', { name: /personal tokens/i });
    await user.click(tokensTab);

    // Verify F90 message
    expect(
      await screen.findByText(/sign in with email or google to manage tokens \(f90\)/i),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /generate new token/i })).not.toBeInTheDocument();

    await expectNoAxeViolations(container);
  });
});

describe('AccountSettingsPage under cookie session', () => {
  it('manages active sessions (lists, revokes session, revokes other sessions)', async () => {
    let revokeCalled = false;
    let revokeOthersCalled = false;
    server.use(
      ...baseHandlers(),
      http.post('*/api/auth/revoke-session', async ({ request }) => {
        revokeCalled = true;
        const body = (await request.json()) as { session_id: string };
        expect(body.session_id).toBe('sess_1');
        return HttpResponse.json({ status: true });
      }),
      http.post('*/api/auth/revoke-other-sessions', () => {
        revokeOthersCalled = true;
        return HttpResponse.json({ status: true });
      }),
    );
    sessionStore.setCookieSession();

    const user = userEvent.setup();
    const { container } = renderPage();

    // Sessions tab is active by default
    expect(await screen.findByText('Chrome on macOS')).toBeInTheDocument();
    expect(screen.getByText('Firefox on Linux')).toBeInTheDocument();

    // Revoke one session
    const revokeButtons = screen.getAllByRole('button', { name: /^revoke$/i });
    await user.click(revokeButtons[0]!);
    expect(revokeCalled).toBe(true);

    // Revoke all other sessions
    const revokeOthersBtn = screen.getByRole('button', { name: /revoke all other sessions/i });
    await user.click(revokeOthersBtn);
    expect(revokeOthersCalled).toBe(true);

    await expectNoAxeViolations(container);
  });

  it('manages personal tokens (lists, creates with secret, revokes)', async () => {
    let createCalled = false;
    let revokeCalled = false;
    server.use(
      ...baseHandlers(),
      http.post('*/v1/tokens', async ({ request }) => {
        createCalled = true;
        const body = (await request.json()) as {
          kind: string;
          name: string;
          scopes: string[];
        };
        expect(body.kind).toBe('personal');
        expect(body.name).toBe('ci-runner');
        return HttpResponse.json(
          {
            secret: 'trb_pat_new_secret_123',
            token: {
              id: 'tok_00000000000000000000000002',
              kind: 'personal',
              name: 'ci-runner',
              scopes: body.scopes,
            },
          },
          { status: 201 },
        );
      }),
      http.delete('*/v1/tokens/:id', ({ params }) => {
        revokeCalled = true;
        expect(params.id).toBe('tok_00000000000000000000000001');
        return new HttpResponse(null, { status: 204 });
      }),
    );
    sessionStore.setCookieSession();

    const user = userEvent.setup();
    const { container } = renderPage();

    // Navigate to Personal tokens tab
    await user.click(await screen.findByRole('tab', { name: /personal tokens/i }));

    // Listed existing token
    expect(await screen.findByText('laptop-cli')).toBeInTheDocument();
    expect(screen.getByText('tok_00000000000000000000000001')).toBeInTheDocument();

    // Revoke existing token
    await user.click(screen.getByRole('button', { name: /^revoke$/i }));
    expect(revokeCalled).toBe(true);

    // Open create dialog
    await user.click(screen.getByRole('button', { name: /generate new token/i }));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();

    // Fill in token name
    await user.type(within(screen.getByRole('dialog')).getByLabelText(/name/i), 'ci-runner');
    await user.click(screen.getByRole('button', { name: /create token/i }));

    expect(createCalled).toBe(true);
    expect(await screen.findByText('Token generated')).toBeInTheDocument();
    expect(screen.getByDisplayValue('trb_pat_new_secret_123')).toBeInTheDocument();
    expect(screen.getByText(/you will not be able to see it again/i)).toBeInTheDocument();

    // Close dialog
    await user.click(screen.getByRole('button', { name: /done/i }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());

    await expectNoAxeViolations(container);
  });

  it('flags tokens issued without a fresh second factor and replaces one within the session limit', async () => {
    const sessionEnds = Date.now() + 86_400_000 * 29.5;
    let created: { name: string; scopes: string[]; expires_at_ms: number } | null = null;
    server.use(
      http.get('*/v1/me', () =>
        HttpResponse.json({
          ...mockMe,
          credential: { ...mockMe.credential, kind: 'session', expires_at_ms: sessionEnds },
        }),
      ),
      http.get('*/v1/me/security', () =>
        HttpResponse.json({
          mfa_required: true,
          mfa_partial: false,
          mfa_enrolled: true,
          mfa_verified_at: Date.now(),
          mfa_method: 'passkey',
          allow_passkeys: true,
          allow_authenticator: true,
          fresh_minutes: 10,
          authenticator_enrolled: false,
          passkey_count: 1,
        }),
      ),
      http.get('*/v1/tokens', () =>
        HttpResponse.json({
          items: [
            { ...sampleTokens[0], fresh_mfa_at_issue: false },
            {
              ...sampleTokens[0],
              id: 'tok_00000000000000000000000003',
              name: 'fresh-cli',
              fresh_mfa_at_issue: true,
            },
          ],
        }),
      ),
      http.post('*/v1/tokens', async ({ request }) => {
        created = (await request.json()) as typeof created;
        return HttpResponse.json(
          {
            secret: 'trb_pat_replacement',
            token: {
              id: 'tok_00000000000000000000000004',
              kind: 'personal',
              name: 'x',
              scopes: [],
              fresh_mfa_at_issue: true,
            },
          },
          { status: 201 },
        );
      }),
      ...baseHandlers(),
    );
    sessionStore.setCookieSession();
    const user = userEvent.setup();
    const { container } = renderPage();
    await user.click(await screen.findByRole('tab', { name: /personal tokens/i }));
    expect(await screen.findByText('Created without a second-factor check')).toBeInTheDocument();
    // Only the legacy token is flagged.
    expect(screen.getAllByText('Created without a second-factor check')).toHaveLength(1);
    await expectNoAxeViolations(container);

    await user.click(screen.getByRole('button', { name: 'Replace token' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByLabelText('Name')).toHaveValue('laptop-cli (replacement)');
    const expiry = within(dialog).getByLabelText('Expiration');
    // 30 days and longer would outlive this browser session: the largest preset that fits.
    expect(expiry).toHaveValue('7');
    expect(within(expiry).queryByRole('option', { name: /90 days/ })).not.toBeInTheDocument();
    expect(
      within(expiry).getByRole('option', { name: /when this sign-in ends/ }),
    ).toBeInTheDocument();
    expect(expiry).toHaveAccessibleDescription(/cannot outlive the sign-in that creates it/i);
    await user.selectOptions(expiry, 'limit');
    await user.click(within(dialog).getByRole('button', { name: /create token/i }));
    await waitFor(() => expect(created).not.toBeNull());
    expect(created!.scopes).toEqual(['runbooks:read', 'executions:read']);
    expect(created!.expires_at_ms).toBeLessThanOrEqual(sessionEnds);
    expect(created!.expires_at_ms).toBeGreaterThan(sessionEnds - 86_400_000);
  });

  it('manages connected applications (lists, revokes access)', async () => {
    let revokeAppCalled = false;
    server.use(
      ...baseHandlers(),
      http.delete('*/v1/me/apps/:client', ({ params }) => {
        revokeAppCalled = true;
        expect(params.client).toBe('app_mcp_client');
        return new HttpResponse(null, { status: 204 });
      }),
    );
    sessionStore.setCookieSession();

    const user = userEvent.setup();
    const { container } = renderPage();

    // Navigate to Connected apps tab
    await user.click(await screen.findByRole('tab', { name: /connected apps/i }));

    expect(await screen.findByText('Claude Desktop MCP')).toBeInTheDocument();
    expect(screen.getByText('app_mcp_client')).toBeInTheDocument();
    expect(screen.getByText(/127.0.0.1:4000/)).toBeInTheDocument();

    // Revoke app
    await user.click(screen.getByRole('button', { name: /revoke access/i }));
    expect(revokeAppCalled).toBe(true);

    await expectNoAxeViolations(container);
  });
});
