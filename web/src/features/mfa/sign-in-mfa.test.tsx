import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { App } from '../../App';
import { sessionStore, tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { createAppRouter } from '../../routes/router';
import { defaultState, security, serveAuth, stubWebAuthn, type AuthState } from './testing';

const fullMe = {
  user: { id: 'usr_1', name: 'Ada' },
  org: { id: 'org_1', title: 'Acme', role: 'member' },
  teams: [],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read'],
    projects: null,
    expires_at_ms: null,
  },
};

afterEach(() => {
  tokenStore.clear();
  sessionStore.clearCookieSession();
  localStorage.clear();
});

/** Email sign-in with the auth service, a directory, and `state` for factors. */
function serveSignIn(state: AuthState) {
  const calls = serveAuth(state);
  server.use(
    http.get('*/.well-known/runbook', () =>
      HttpResponse.json({ auth: { methods: ['email'], device_authorization_endpoint: null } }),
    ),
    // No live cookie until a sign-in succeeds.
    http.get('*/api/auth/get-session', () =>
      signedIn
        ? HttpResponse.json({
            session: { id: 'ses_1' },
            user: { id: 'au_1', twoFactorEnabled: state.totp },
          })
        : HttpResponse.json(null),
    ),
    http.post('*/api/auth/email-otp/send-verification-otp', () =>
      HttpResponse.json({ success: true }),
    ),
    http.post('*/api/auth/sign-in/email-otp', () => {
      signedIn = true;
      return HttpResponse.json({ token: 'x' });
    }),
    http.get('*/v1/me', () =>
      HttpResponse.json(
        state.security.mfa_partial
          ? {
              user: { id: 'usr_1', email: 'ada@example.test' },
              org: { id: 'org_1' },
              security: state.security,
            }
          : fullMe,
      ),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/runner', () => HttpResponse.json({ status: 'none' })),
    http.get('*/v1/me/profile', () => HttpResponse.json({ welcomed: true })),
  );
  let signedIn = false;
  return {
    calls,
    signIn: () => {
      signedIn = true;
    },
  };
}

async function signInWithEmail() {
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText('Email address'), 'ada@example.test');
  await user.click(screen.getByRole('button', { name: 'Continue with email' }));
  await user.type(await screen.findByLabelText('6-digit code'), '424242');
  await user.click(screen.getByRole('button', { name: 'Verify code' }));
  return user;
}

describe('sign-in with a second factor', () => {
  it('asks for the second factor after the email code, then goes on', async () => {
    stubWebAuthn();
    const state = defaultState({
      security: security({ mfa_required: true, mfa_partial: true, mfa_verified_at: null }),
    });
    const { calls } = serveSignIn(state);
    const router = createAppRouter(
      createMemoryHistory({ initialEntries: ['/sign-in?next=/tasks'] }),
    );
    render(<App router={router} />);
    const user = await signInWithEmail();
    expect(await screen.findByRole('heading', { name: 'Confirm it’s you' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Not now' })).toBeNull();
    await user.click(await screen.findByRole('button', { name: 'Use a backup code instead' }));
    await user.type(screen.getByLabelText('Backup code'), 'abcde-12345');
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/tasks'));
    expect(calls.map((c) => c.path)).toContain('verify-backup-code');
  });

  it('sends a person who must enroll to a setup screen they cannot skip', async () => {
    stubWebAuthn();
    const state = defaultState({
      passkeys: [],
      totp: false,
      security: security({
        mfa_required: true,
        mfa_partial: true,
        mfa_enrolled: false,
        mfa_verified_at: null,
        mfa_method: null,
      }),
    });
    serveSignIn(state);
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/sign-in'] }));
    render(<App router={router} />);
    const user = await signInWithEmail();
    expect(
      await screen.findByRole('heading', { name: 'Set up a second factor' }),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Not now' })).toBeNull();
    await user.click(screen.getByRole('button', { name: /passkey/i }));
    await user.click(await screen.findByRole('button', { name: 'Create passkey' }));
    await waitFor(() => expect(router.state.location.pathname).not.toBe('/sign-in'));
  });

  it('signs in with a passkey alone', async () => {
    const { get } = stubWebAuthn();
    const state = defaultState({ security: security({ mfa_method: 'passkey' }) });
    const { calls, signIn } = serveSignIn(state);
    server.use(
      http.post('*/api/auth/passkey/verify-authentication', () => {
        signIn();
        return HttpResponse.json({ status: true });
      }),
    );
    const router = createAppRouter(
      createMemoryHistory({ initialEntries: ['/sign-in?next=/tasks'] }),
    );
    render(<App router={router} />);
    await userEvent
      .setup()
      .click(await screen.findByRole('button', { name: 'Sign in with a passkey' }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/tasks'));
    expect(get).toHaveBeenCalledTimes(1);
    expect(calls.map((c) => c.path)).toContain('generate-authenticate-options');
    expect(sessionStore.isCookieSession()).toBe(true);
  });

  it('shows the gate in the app for a partial session that reloads', async () => {
    stubWebAuthn();
    sessionStore.setCookieSession();
    const state = defaultState({
      security: security({ mfa_required: true, mfa_partial: true, mfa_verified_at: null }),
    });
    const { signIn } = serveSignIn(state);
    signIn();
    const router = createAppRouter(
      createMemoryHistory({
        initialEntries: [
          '/settings/security?return_to=%2Fapi%2Fauth%2Foauth2%2Fauthorize%3Fclient_id%3Dc1',
        ],
      }),
    );
    render(<App router={router} />);
    expect(await screen.findByRole('heading', { name: 'Confirm it’s you' })).toBeInTheDocument();
    expect(screen.queryByRole('navigation', { name: 'Primary' })).toBeNull();
  });
});

describe('CLI device approval with MFA', () => {
  const lookup = {
    user_code: 'WDJB-MJHT',
    client_name: 'runbook CLI',
    client_name_verified: false,
    requester_ip: '127.0.0.1',
    requested_scopes: ['runbooks:read'],
    created_at_ms: Date.now() - 1_000,
    expires_at_ms: Date.now() + 600_000,
  };

  it('verifies a partial session before showing the approval', async () => {
    stubWebAuthn();
    sessionStore.setCookieSession();
    const state = defaultState({
      security: security({ mfa_required: true, mfa_partial: true, mfa_verified_at: null }),
    });
    const { signIn } = serveSignIn(state);
    signIn();
    server.use(http.post('*/v1/device/lookup', () => HttpResponse.json(lookup)));
    const router = createAppRouter(
      createMemoryHistory({ initialEntries: ['/device?user_code=WDJB-MJHT'] }),
    );
    render(<App router={router} />);
    expect(await screen.findByRole('heading', { name: 'Confirm it’s you' })).toBeInTheDocument();
    await userEvent.setup().click(await screen.findByRole('button', { name: 'Use a passkey' }));
    expect(await screen.findByRole('heading', { name: 'Device approval' })).toBeInTheDocument();
  });

  it('asks for a fresh check when approving, then approves with the same request', async () => {
    stubWebAuthn();
    sessionStore.setCookieSession();
    const state = defaultState({ security: security({ mfa_verified_at: Date.now() - 3_600_000 }) });
    const { signIn } = serveSignIn(state);
    signIn();
    const approvals: (string | null)[] = [];
    server.use(
      http.post('*/v1/device/lookup', () => HttpResponse.json(lookup)),
      http.post('*/v1/device/approve', ({ request }) => {
        approvals.push(request.headers.get('idempotency-key'));
        return approvals.length === 1
          ? HttpResponse.json(
              { status: 403, code: 'auth.mfa_step_up', fresh_minutes: 10 },
              { status: 403, headers: { 'Content-Type': 'application/problem+json' } },
            )
          : new HttpResponse(null, { status: 204 });
      }),
    );
    const router = createAppRouter(
      createMemoryHistory({ initialEntries: ['/device?user_code=WDJB-MJHT'] }),
    );
    render(<App router={router} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /approve/i }));
    const prompt = await screen.findByRole('dialog', { name: 'Confirm it’s you' });
    await user.type(within(prompt).getByLabelText('Code from your authenticator app'), '123456');
    await user.click(within(prompt).getByRole('button', { name: 'Verify' }));
    expect(await screen.findByRole('heading', { name: 'Device approved' })).toBeInTheDocument();
    expect(approvals).toHaveLength(2);
    expect(approvals[1]).toBe(approvals[0]);
  });
});
