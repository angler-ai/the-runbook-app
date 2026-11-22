import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api, ApiError } from '../../api/client';
import { sessionStore, tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { AccountSecurityPage } from './AccountSecurityPage';
import { MfaGate } from './MfaGate';
import { OrgSecurityTab } from './OrgSecurityTab';
import { resumeAt, safeReturnTo } from './returnTo';
import type * as ReturnTo from './returnTo';
import { StepUpHost } from './StepUpHost';
import {
  defaultState,
  renderWithProviders,
  security,
  serveAuth,
  stubWebAuthn,
  type AuthState,
} from './testing';
import { base64urlToBuffer, bufferToBase64url } from './webauthn';

vi.mock('./returnTo', async (importOriginal) => ({
  ...(await importOriginal<typeof ReturnTo>()),
  resumeAt: vi.fn(),
}));

beforeEach(() => {
  sessionStore.setCookieSession();
});

afterEach(() => {
  tokenStore.clear();
  sessionStore.clearCookieSession();
  vi.mocked(resumeAt).mockClear();
});

function renderSecurityPage(state: AuthState, returnTo?: string) {
  const calls = serveAuth(state);
  const { ui, client } = renderWithProviders(
    <>
      <AccountSecurityPage returnTo={returnTo ?? null} />
      <StepUpHost />
    </>,
  );
  return { calls, client, ...render(ui) };
}

describe('Account › Security', () => {
  it('lists passkeys with their last use, the authenticator and recent sessions', async () => {
    stubWebAuthn();
    const { container } = renderSecurityPage(defaultState());
    const list = await screen.findByRole('list', { name: 'Your passkeys' });
    expect(within(list).getByText('Work laptop')).toBeInTheDocument();
    expect(within(list).getByText(/last used 2h ago/i)).toBeInTheDocument();
    expect(await screen.findByText(/your app gives a 6-digit code/i)).toBeInTheDocument();
    expect(
      within(await screen.findByRole('list', { name: 'Recent sessions' })).getByText(
        'Chrome on macOS',
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Two-step sign-in is on' })).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('adds a named passkey through WebAuthn with required user verification', async () => {
    const { create } = stubWebAuthn();
    const state = defaultState();
    const { calls } = renderSecurityPage(state);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Add passkey' }));
    const dialog = await screen.findByRole('dialog', { name: 'Add a passkey' });
    const name = within(dialog).getByLabelText('Passkey name');
    await user.clear(name);
    await user.type(name, 'Security key');
    await user.click(within(dialog).getByRole('button', { name: 'Create passkey' }));
    await waitFor(() => expect(calls.map((c) => c.path)).toContain('verify-registration'));
    const options = create.mock.calls[0]![0].publicKey!;
    expect(options.authenticatorSelection?.userVerification).toBe('required');
    expect(new Uint8Array(options.challenge as ArrayBuffer)).toEqual(
      new Uint8Array(base64urlToBuffer('Y2hhbGxlbmdl')),
    );
    const sent = calls.find((c) => c.path === 'verify-registration')!.body as {
      name: string;
      response: { rawId: string; response: { attestationObject: string } };
    };
    expect(sent.name).toBe('Security key');
    expect(sent.response.rawId).toBe(bufferToBase64url(new TextEncoder().encode('cred-1')));
    expect(sent.response.response.attestationObject).toBeTruthy();
    expect(
      await within(await screen.findByRole('list', { name: 'Your passkeys' })).findByText(
        'Security key',
      ),
    ).toBeInTheDocument();
  });

  it('renames a passkey', async () => {
    stubWebAuthn();
    const { calls } = renderSecurityPage(defaultState());
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Rename Work laptop' }));
    const dialog = await screen.findByRole('dialog', { name: 'Rename passkey' });
    const field = within(dialog).getByLabelText('Passkey name');
    await user.clear(field);
    await user.type(field, 'Old laptop');
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(calls.find((c) => c.path === 'update-passkey')?.body).toEqual({
        id: 'pk_1',
        name: 'Old laptop',
      }),
    );
  });

  it('explains why removing the last factor is refused', async () => {
    stubWebAuthn();
    renderSecurityPage(defaultState());
    // After serveAuth's handlers, so this one answers first.
    server.use(
      http.post('*/api/auth/passkey/delete-passkey', () =>
        HttpResponse.json(
          { code: 'auth.mfa_removal_refused', message: 'Keep at least one allowed factor.' },
          { status: 409 },
        ),
      ),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Remove Work laptop' }));
    await user.click(await screen.findByRole('button', { name: 'Remove passkey' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      /organization requires a second factor, so you cannot remove your last allowed one/i,
    );
  });

  it('disables removing the only factor while the organization requires MFA', async () => {
    stubWebAuthn();
    renderSecurityPage(
      defaultState({
        totp: false,
        security: security({ mfa_required: true, mfa_method: 'passkey' }),
      }),
    );
    const remove = await screen.findByRole('button', { name: 'Remove Work laptop' });
    await waitFor(() => expect(remove).toBeDisabled());
    expect(remove).toHaveAccessibleDescription(/your only one cannot be removed/i);
  });

  it('sets up an authenticator app with a local QR code, the manual key and backup codes', async () => {
    stubWebAuthn();
    const fetchSpy = vi.spyOn(globalThis, 'fetch');
    const state = defaultState({ totp: false });
    const { calls } = renderSecurityPage(state);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Set up' }));
    await user.click(screen.getByRole('button', { name: 'Set up authenticator app' }));
    expect(
      await screen.findByRole('img', { name: 'QR code for your authenticator app' }),
    ).toBeInTheDocument();
    expect(screen.getByTestId('totp-manual-key')).toHaveTextContent('JBSW Y3DP EHPK 3PXP');
    // Nothing but our own origin was asked to draw it.
    for (const [input] of fetchSpy.mock.calls) {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
      expect(new URL(url, window.location.href).origin).toBe(window.location.origin);
    }
    const code = screen.getByLabelText('Code from your authenticator app');
    expect(code).toHaveAttribute('autocomplete', 'one-time-code');
    expect(code).toHaveAttribute('inputmode', 'numeric');
    await user.type(code, '000000');
    await user.click(screen.getByRole('button', { name: 'Confirm code' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/that code is not right/i);
    expect(code).toHaveAttribute('aria-invalid', 'true');
    await user.clear(code);
    await user.type(code, '123456');
    await user.click(screen.getByRole('button', { name: 'Confirm code' }));
    const codes = await screen.findByRole('list', { name: 'Backup codes' });
    expect(
      within(codes)
        .getAllByRole('listitem')
        .map((li) => li.textContent),
    ).toEqual(['aaaaa-11111', 'bbbbb-22222', 'ccccc-33333']);
    await user.click(screen.getByRole('button', { name: 'Copy codes' }));
    expect(await navigator.clipboard.readText()).toBe('aaaaa-11111\nbbbbb-22222\nccccc-33333');
    await user.click(screen.getByRole('button', { name: 'I saved my backup codes' }));
    expect(
      calls.find((c) => c.path === 'verify-totp' && (c.body as { code: string }).code === '123456'),
    ).toBeTruthy();
    expect(await screen.findByText(/on\. your app gives a 6-digit code/i)).toBeInTheDocument();
  });

  it('treats a confirmed setup as enrollment, so the next sensitive action asks for a new code', async () => {
    stubWebAuthn();
    const state = defaultState({
      passkeys: [],
      totp: false,
      security: security({ mfa_enrolled: false, mfa_verified_at: null, mfa_method: null }),
    });
    const { calls } = renderSecurityPage(state);
    server.use(
      http.post('*/api/auth/two-factor/generate-backup-codes', () =>
        state.security.mfa_verified_at
          ? HttpResponse.json({ status: true, backupCodes: ['zzzzz-99999'] })
          : HttpResponse.json(
              { code: 'auth.mfa_step_up', message: 'Verify first.', fresh_minutes: 10 },
              { status: 403 },
            ),
      ),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Set up' }));
    await user.click(screen.getByRole('button', { name: 'Set up authenticator app' }));
    await user.type(await screen.findByLabelText('Code from your authenticator app'), '123456');
    await user.click(screen.getByRole('button', { name: 'Confirm code' }));
    expect(await screen.findByText(/does not count as confirming it is you/i)).toBeInTheDocument();
    expect(state.security.mfa_verified_at).toBeNull();
    await user.click(screen.getByRole('button', { name: 'I saved my backup codes' }));
    await user.click(await screen.findByRole('button', { name: 'Regenerate backup codes' }));
    await user.click(await screen.findByRole('button', { name: 'Regenerate' }));
    const prompt = await screen.findByRole('dialog', { name: 'Confirm it’s you' });
    await user.type(within(prompt).getByLabelText('Code from your authenticator app'), '123456');
    await user.click(within(prompt).getByRole('button', { name: 'Verify' }));
    expect(await screen.findByRole('dialog', { name: 'Your new backup codes' })).toBeInTheDocument();
    expect(calls.filter((c) => c.path === 'verify-totp')).toHaveLength(2);
  });

  it('starts over when the setup expired or another browser replaced it', async () => {
    stubWebAuthn();
    renderSecurityPage(defaultState({ totp: false }));
    server.use(
      http.post('*/api/auth/two-factor/verify-totp', () =>
        HttpResponse.json(
          { code: 'auth.mfa_setup_changed', message: 'Start a new authenticator setup.' },
          { status: 409 },
        ),
      ),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Set up' }));
    await user.click(screen.getByRole('button', { name: 'Set up authenticator app' }));
    await user.type(await screen.findByLabelText('Code from your authenticator app'), '123456');
    await user.click(screen.getByRole('button', { name: 'Confirm code' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/setups last 10 minutes/i);
    expect(screen.getByRole('button', { name: 'Set up authenticator app' })).toBeInTheDocument();
    expect(screen.queryByTestId('totp-manual-key')).not.toBeInTheDocument();
  });

  it('regenerates backup codes only after confirmation', async () => {
    stubWebAuthn();
    const { calls, client } = renderSecurityPage(defaultState());
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Regenerate backup codes' }));
    expect(
      await screen.findByRole('alertdialog', { name: 'Regenerate backup codes?' }),
    ).toBeInTheDocument();
    expect(calls.some((c) => c.path === 'generate-backup-codes')).toBe(false);
    await user.click(screen.getByRole('button', { name: 'Regenerate' }));
    const dialog = await screen.findByRole('dialog', { name: 'Your new backup codes' });
    expect(within(dialog).getByText('zzzzz-99999')).toBeInTheDocument();
    await user.click(within(dialog).getByRole('button', { name: 'I saved my backup codes' }));
    expect(screen.queryByText('zzzzz-99999')).not.toBeInTheDocument();
    const cached = client.getMutationCache().getAll().map((mutation) => mutation.state.data);
    expect(JSON.stringify(cached)).not.toContain('zzzzz-99999');
  });

  it('asks for a fresh check inline when the auth service wants one, then carries on', async () => {
    const { create } = stubWebAuthn();
    let refused = false;
    const { calls } = renderSecurityPage(defaultState());
    server.use(
      http.get('*/api/auth/passkey/generate-register-options', () => {
        if (!refused) {
          refused = true;
          return HttpResponse.json(
            { code: 'auth.mfa_step_up', message: 'Verify an existing factor.', fresh_minutes: 10 },
            { status: 403 },
          );
        }
        return undefined;
      }),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Add passkey' }));
    await user.click(await screen.findByRole('button', { name: 'Create passkey' }));
    const prompt = await screen.findByRole('dialog', { name: 'Confirm it’s you' });
    expect(prompt).toHaveTextContent(/from the last 10 minutes/);
    await user.type(within(prompt).getByLabelText('Code from your authenticator app'), '123456');
    await user.click(within(prompt).getByRole('button', { name: 'Verify' }));
    await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(calls.map((c) => c.path)).toContain('verify-registration'));
  });

  it('asks for a second factor before listing sessions', async () => {
    stubWebAuthn();
    const state = defaultState();
    renderSecurityPage(state);
    let verified = false;
    server.use(
      http.get('*/api/auth/list-sessions', () =>
        verified
          ? HttpResponse.json(state.sessions)
          : HttpResponse.json(
              { code: 'auth.mfa_required', message: 'Verify a second factor first.' },
              { status: 403 },
            ),
      ),
      http.post('*/api/auth/passkey/verify-authentication', () => {
        verified = true;
        return HttpResponse.json({ status: true });
      }),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Confirm it’s you' }));
    const prompt = await screen.findByRole('dialog', { name: 'Confirm it’s you' });
    await user.click(await within(prompt).findByRole('button', { name: 'Use a passkey' }));
    expect(
      within(await screen.findByRole('list', { name: 'Recent sessions' })).getByText(
        'Chrome on macOS',
      ),
    ).toBeInTheDocument();
  });

  it('offers to resume an interrupted app authorization at a local path only', async () => {
    stubWebAuthn();
    renderSecurityPage(defaultState(), '/api/auth/oauth2/authorize?client_id=c1&state=s');
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Continue authorizing' }));
    expect(resumeAt).toHaveBeenCalledWith('/api/auth/oauth2/authorize?client_id=c1&state=s');
  });

  it('asks token sessions to sign in with a browser method', () => {
    sessionStore.clearCookieSession();
    tokenStore.set('trb_pat_x');
    const { ui } = renderWithProviders(<AccountSecurityPage />, { router: false });
    render(ui);
    expect(screen.getByRole('status')).toHaveTextContent(/signed in with a personal access token/i);
  });
});

describe('safeReturnTo', () => {
  it.each([
    ['/api/auth/oauth2/authorize?x=1', '/api/auth/oauth2/authorize?x=1'],
    ['/api/auth/oauth/consent?a=b#c', '/api/auth/oauth/consent?a=b#c'],
    ['https://evil.test/', null],
    ['//evil.test/', null],
    ['/\\evil.test', null],
    ['/ok\n/x', null],
    ['javascript:alert(1)', null],
    ['/.//invalid.test/login', null],
    ['/..//invalid.test', null],
    ['/%2e//invalid.test', null],
    ['/x/..//invalid.test', null],
    ['/%2finvalid.test', null],
    ['/%255cinvalid.test', null],
    ['/ok%0a/x', null],
    ['', null],
    [42, null],
  ])('%s -> %s', (input, expected) => {
    expect(safeReturnTo(input, 'https://runbooks.example.test')).toBe(expected);
  });
});

describe('MfaGate (second factor after sign-in)', () => {
  function renderGate(state: AuthState, props: Partial<Parameters<typeof MfaGate>[0]> = {}) {
    const calls = serveAuth(state);
    const onDone = vi.fn();
    const onSignOut = vi.fn();
    const { ui } = renderWithProviders(
      <MfaGate security={state.security} onDone={onDone} onSignOut={onSignOut} {...props} />,
      { router: false },
    );
    return { calls, onDone, onSignOut, ...render(ui) };
  }

  it('verifies with an authenticator code and cannot be skipped while required', async () => {
    stubWebAuthn();
    const { calls, onDone, onSignOut, container } = renderGate(
      defaultState({
        security: security({ mfa_required: true, mfa_partial: true, mfa_verified_at: null }),
      }),
    );
    expect(screen.getByRole('heading', { name: 'Confirm it’s you' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Not now' })).not.toBeInTheDocument();
    const user = userEvent.setup();
    const field = await screen.findByLabelText('Code from your authenticator app');
    await user.type(field, '111111');
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('alert')).toHaveTextContent(/that code is not right/i);
    await user.clear(field);
    await user.type(field, '123456');
    await user.keyboard('{Enter}');
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    expect(calls.filter((c) => c.path === 'verify-totp')).toHaveLength(2);
    await user.click(screen.getByRole('button', { name: 'Sign out' }));
    expect(onSignOut).toHaveBeenCalled();
    await expectNoAxeViolations(container);
  });

  it('accepts a backup code in either form', async () => {
    stubWebAuthn();
    const { calls, onDone } = renderGate(
      defaultState({ security: security({ mfa_required: true, mfa_partial: true }) }),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Use a backup code instead' }));
    const field = screen.getByLabelText('Backup code');
    expect(field).toHaveAttribute('autocomplete', 'one-time-code');
    expect(field).toHaveFocus();
    await user.type(field, 'ABCDE12345');
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    expect(calls.find((c) => c.path === 'verify-backup-code')?.body).toEqual({
      code: 'ABCDE-12345',
    });
  });

  it('verifies with a passkey', async () => {
    const { get } = stubWebAuthn();
    const { calls, onDone } = renderGate(
      defaultState({ security: security({ mfa_required: true, mfa_partial: true }) }),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Use a passkey' }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    expect(get.mock.calls[0]![0].publicKey!.userVerification).toBe('required');
    const body = calls.find((c) => c.path === 'verify-authentication')!.body as {
      response: { response: { signature: string } };
    };
    expect(body.response.response.signature).toBe(
      bufferToBase64url(new TextEncoder().encode('signature')),
    );
  });

  it('lets an enrolled person skip when the organization does not require MFA', async () => {
    stubWebAuthn();
    const { onDone } = renderGate(defaultState({ security: security({ mfa_verified_at: null }) }));
    await userEvent.setup().click(await screen.findByRole('button', { name: 'Not now' }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
  });

  it('requires a passkey first when the organization requires MFA, then resumes the app authorization', async () => {
    stubWebAuthn();
    const state = defaultState({
      passkeys: [],
      totp: false,
      security: security({
        mfa_required: true,
        mfa_partial: true,
        mfa_enrolled: false,
        mfa_verified_at: null,
      }),
    });
    const { onDone, calls, container } = renderGate(state, {
      returnTo: '/api/auth/oauth/consent?client_id=c1',
    });
    expect(screen.getByRole('heading', { name: 'Set up a second factor' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Not now' })).not.toBeInTheDocument();
    // An app setup would need a check this account cannot give yet.
    expect(screen.queryByRole('button', { name: /authenticator app/i })).not.toBeInTheDocument();
    expect(screen.getByText(/add an authenticator app afterwards/i)).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent(/an app is waiting for you/i);
    await expectNoAxeViolations(container);
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Create passkey' }));
    await waitFor(() =>
      expect(resumeAt).toHaveBeenCalledWith('/api/auth/oauth/consent?client_id=c1'),
    );
    expect(onDone).not.toHaveBeenCalled();
    expect(calls.some((c) => c.path === 'enable')).toBe(false);
  });

  it('explains an authenticator-only organization and points to an owner', async () => {
    stubWebAuthn();
    const { calls } = renderGate(
      defaultState({
        passkeys: [],
        totp: false,
        security: security({
          mfa_required: true,
          mfa_partial: true,
          mfa_enrolled: false,
          mfa_verified_at: null,
          allow_passkeys: false,
        }),
      }),
    );
    expect(screen.getByRole('alert')).toHaveTextContent(
      /allows only authenticator apps.*ask an organization owner to allow passkeys/i,
    );
    expect(screen.queryByRole('button', { name: 'Create passkey' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Sign out' })).toBeInTheDocument();
    expect(calls).toEqual([]);
  });

  it('enrolls a passkey when that is the only allowed factor', async () => {
    stubWebAuthn();
    const state = defaultState({
      passkeys: [],
      totp: false,
      security: security({
        mfa_required: true,
        mfa_partial: true,
        mfa_enrolled: false,
        allow_authenticator: false,
      }),
    });
    const { onDone, calls } = renderGate(state);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Create passkey' }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    expect(calls.map((c) => c.path)).toEqual(['generate-register-options', 'verify-registration']);
  });
});

describe('Step-up prompt for core API refusals', () => {
  function renderHost() {
    const { ui } = renderWithProviders(<StepUpHost />, { router: false });
    return render(ui);
  }

  it('retries the refused request with the same idempotency key after a passkey check', async () => {
    stubWebAuthn();
    serveAuth(defaultState());
    const keys: (string | null)[] = [];
    server.use(
      http.post('*/v1/orgs/:org/users/:user/mfa/reset', ({ request }) => {
        keys.push(request.headers.get('idempotency-key'));
        if (keys.length === 1) {
          return HttpResponse.json(
            { status: 403, code: 'auth.mfa_step_up', fresh_minutes: 15 },
            { status: 403, headers: { 'Content-Type': 'application/problem+json' } },
          );
        }
        return HttpResponse.json({ reset: true });
      }),
    );
    renderHost();
    const pending = api.POST('/v1/orgs/{org}/users/{user}/mfa/reset', {
      params: { path: { org: 'org_1', user: 'usr_2' } },
    });
    const prompt = await screen.findByRole('dialog', { name: 'Confirm it’s you' });
    expect(prompt).toHaveTextContent(/last 15 minutes/);
    await userEvent
      .setup()
      .click(await within(prompt).findByRole('button', { name: 'Use a passkey' }));
    const { data } = await pending;
    expect(data).toEqual({ reset: true });
    expect(keys).toHaveLength(2);
    expect(keys[1]).toBe(keys[0]);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('reports the refusal when the person cancels', async () => {
    stubWebAuthn();
    serveAuth(defaultState());
    server.use(
      http.put('*/v1/orgs/:org/security', () =>
        HttpResponse.json(
          { status: 403, code: 'auth.mfa_step_up', fresh_minutes: 10 },
          { status: 403, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    renderHost();
    const pending = api
      .PUT('/v1/orgs/{org}/security', {
        params: { path: { org: 'org_1' } },
        body: {
          require_mfa: 'off',
          allow_passkeys: true,
          allow_authenticator: true,
          fresh_minutes: 10,
        },
      })
      .catch((e: unknown) => e);
    const prompt = await screen.findByRole('dialog', { name: 'Confirm it’s you' });
    await userEvent.setup().click(within(prompt).getByRole('button', { name: 'Cancel' }));
    const failure = await pending;
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).code).toBe('auth.mfa_step_up');
  });

  it('tells token sessions to sign in instead of prompting', async () => {
    sessionStore.clearCookieSession();
    tokenStore.set('trb_pat_x');
    server.use(
      http.post('*/v1/tokens', () =>
        HttpResponse.json(
          { status: 403, code: 'auth.mfa_step_up', fresh_minutes: 10 },
          { status: 403, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    renderHost();
    const pending = api
      .POST('/v1/tokens', {
        params: { header: { 'Idempotency-Key': 'k1' } },
        body: { kind: 'personal' } as never,
      })
      .catch((e: unknown) => e);
    const prompt = await screen.findByRole('dialog', { name: 'Confirm it’s you' });
    expect(prompt).toHaveTextContent(/pasted access token cannot give/i);
    await userEvent.setup().click(within(prompt).getByRole('button', { name: 'OK' }));
    expect(await pending).toBeInstanceOf(ApiError);
  });

  it('does not prompt for page loads refused because the session became partial', async () => {
    server.use(
      http.get('*/v1/orgs', () =>
        HttpResponse.json(
          { status: 403, code: 'auth.mfa_required', security: security({ mfa_partial: true }) },
          { status: 403, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    renderHost();
    const failure = await api.GET('/v1/orgs', {}).catch((e: unknown) => e);
    expect((failure as ApiError).code).toBe('auth.mfa_required');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

describe('Settings › Organization › Security', () => {
  function renderTab(
    policy = {
      require_mfa: 'off',
      allow_passkeys: true,
      allow_authenticator: true,
      fresh_minutes: 10,
    },
    mine = security(),
    preview: { affected?: unknown[] } = {},
  ) {
    const puts: unknown[] = [];
    const previews: unknown[] = [];
    server.use(
      http.get('*/v1/me/security', () => HttpResponse.json(mine)),
      http.get('*/v1/orgs/:org/security', () => HttpResponse.json(policy)),
      http.put('*/v1/orgs/:org/security', async ({ request }) => {
        const body = (await request.json()) as Record<string, unknown>;
        if (new URL(request.url).searchParams.get('dry_run') === 'true') {
          previews.push(body);
          return HttpResponse.json({
            dry_run: true,
            policy: body,
            affected: preview.affected ?? [],
          });
        }
        puts.push(body);
        return HttpResponse.json(body);
      }),
    );
    const { ui } = renderWithProviders(<OrgSecurityTab orgId="org_1" isOwner />);
    return { puts, previews, ...render(ui) };
  }

  it('saves the policy', async () => {
    const { puts, container } = renderTab();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('radio', { name: 'Everyone' }));
    await user.click(screen.getByRole('checkbox', { name: /authenticator apps/i }));
    const minutes = screen.getByLabelText('Fresh check window (minutes)');
    await user.clear(minutes);
    await user.type(minutes, '30');
    await user.click(screen.getByRole('button', { name: 'Save security settings' }));
    await waitFor(() =>
      expect(puts).toEqual([
        {
          require_mfa: 'everyone',
          allow_passkeys: true,
          allow_authenticator: false,
          fresh_minutes: 30,
        },
      ]),
    );
    expect(await screen.findByText('Security settings saved.')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('lists the members a policy would lock out and saves only after confirmation', async () => {
    const { puts, previews, container } = renderTab(undefined, security(), {
      affected: [
        {
          user_id: 'usr_2',
          name: 'Grace',
          email: 'grace@example.test',
          reason: 'factor_not_allowed',
        },
      ],
    });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('radio', { name: 'Everyone' }));
    await user.click(screen.getByRole('checkbox', { name: /authenticator apps/i }));
    await user.click(screen.getByRole('button', { name: 'Save security settings' }));
    const locked = await screen.findByRole('list', { name: 'Members locked out' });
    expect(within(locked).getByText('Grace (grace@example.test)')).toBeInTheDocument();
    expect(within(locked).getByText(/only factors this policy disallows/i)).toBeInTheDocument();
    expect(previews).toHaveLength(1);
    expect(puts).toEqual([]);
    const save = screen.getByRole('button', { name: 'Save and lock out 1 member' });
    expect(save).toBeDisabled();
    await user.click(screen.getByRole('checkbox', { name: /cannot sign in until/i }));
    await user.click(save);
    await waitFor(() =>
      expect(puts).toEqual([
        {
          require_mfa: 'everyone',
          allow_passkeys: true,
          allow_authenticator: false,
          fresh_minutes: 10,
        },
      ]),
    );
    expect(await screen.findByText('Security settings saved.')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('discards a policy preview if the proposal changes while it is pending', async () => {
    const { puts } = renderTab();
    let release = () => {};
    let markStarted = () => {};
    const pending = new Promise<void>((resolve) => { release = resolve; });
    const started = new Promise<void>((resolve) => { markStarted = resolve; });
    server.use(
      http.put('*/v1/orgs/:org/security', async ({ request }) => {
        const policy = await request.json();
        markStarted();
        await pending;
        return HttpResponse.json({
          dry_run: true,
          policy,
          affected: [{ user_id: 'usr_2', name: 'Grace', email: null, reason: 'cannot_enroll' }],
        });
      }),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('radio', { name: 'Everyone' }));
    const save = screen.getByRole('button', { name: 'Save security settings' });
    await user.click(save);
    await started;
    await user.click(screen.getByRole('checkbox', { name: 'Passkeys' }));
    release();
    await waitFor(() => expect(save).toBeEnabled());
    expect(screen.queryByRole('list', { name: 'Members locked out' })).not.toBeInTheDocument();
    expect(puts).toEqual([]);
  });

  it('checks again after the proposal changes', async () => {
    const { previews, puts } = renderTab(undefined, security(), {
      affected: [
        {
          user_id: 'usr_2',
          name: 'Grace',
          email: null,
          reason: 'cannot_enroll',
        },
      ],
    });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('radio', { name: 'Everyone' }));
    await user.click(screen.getByRole('checkbox', { name: 'Passkeys' }));
    await user.click(screen.getByRole('button', { name: 'Save security settings' }));
    expect(await screen.findByText(/cannot set up an authenticator app first/i)).toBeInTheDocument();
    await user.click(screen.getByRole('radio', { name: 'Owners and admins' }));
    expect(screen.queryByRole('list', { name: 'Members locked out' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Save security settings' }));
    await screen.findByRole('list', { name: 'Members locked out' });
    expect(previews).toHaveLength(2);
    expect(puts).toEqual([]);
  });

  it('explains that the owner must enroll before requiring MFA', async () => {
    renderTab(
      undefined,
      security({ mfa_enrolled: false, mfa_verified_at: null, mfa_method: null }),
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole('radio', { name: 'Owners and admins' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      /set up a passkey or an authenticator app for your own account first/i,
    );
    expect(screen.getByRole('button', { name: 'Save security settings' })).toBeDisabled();
  });

  it('keeps at least one factor and a valid window', async () => {
    renderTab();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('checkbox', { name: 'Passkeys' }));
    await user.click(screen.getByRole('checkbox', { name: /authenticator apps/i }));
    expect(await screen.findByText('Allow at least one kind of factor.')).toBeInTheDocument();
    const minutes = screen.getByLabelText('Fresh check window (minutes)');
    await user.clear(minutes);
    await user.type(minutes, '90');
    expect(screen.getByText(/whole number of minutes from 5 to 60/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save security settings' })).toBeDisabled();
  });
});
