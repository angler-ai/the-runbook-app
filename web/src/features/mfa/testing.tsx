// Test helpers for the MFA flows: a fake WebAuthn authenticator and MSW
// handlers for the auth service's factor routes.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { http, HttpResponse } from 'msw';
import type { ReactNode } from 'react';
import { vi } from 'vitest';
import { server } from '../../test/msw/server';
import { TestRouter } from '../../test/router';
import type { MfaSecurityView } from './hooks';

const bytes = (text: string) => new TextEncoder().encode(text).buffer;

/** Install navigator.credentials and PublicKeyCredential with recorded calls. */
export function stubWebAuthn() {
  vi.stubGlobal('PublicKeyCredential', function PublicKeyCredential() {});
  const create = vi.fn(async (_options: CredentialCreationOptions) => ({
    id: 'Y3JlZC0x',
    rawId: bytes('cred-1'),
    type: 'public-key',
    authenticatorAttachment: 'platform',
    getClientExtensionResults: () => ({}),
    response: {
      clientDataJSON: bytes('{"type":"webauthn.create"}'),
      attestationObject: bytes('attestation'),
      getTransports: () => ['internal'],
    },
  }));
  const get = vi.fn(async (_options: CredentialRequestOptions) => ({
    id: 'Y3JlZC0x',
    rawId: bytes('cred-1'),
    type: 'public-key',
    authenticatorAttachment: 'platform',
    getClientExtensionResults: () => ({}),
    response: {
      clientDataJSON: bytes('{"type":"webauthn.get"}'),
      authenticatorData: bytes('auth-data'),
      signature: bytes('signature'),
      userHandle: bytes('user'),
    },
  }));
  Object.defineProperty(navigator, 'credentials', {
    configurable: true,
    value: { create, get },
  });
  return { create, get };
}

export const registerOptions = {
  challenge: 'Y2hhbGxlbmdl',
  rp: { id: 'localhost', name: 'The Runbook' },
  user: { id: 'dXNlcg', name: 'ada@example.test', displayName: 'ada@example.test' },
  pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
  excludeCredentials: [{ id: 'b2xk', type: 'public-key' }],
  authenticatorSelection: { userVerification: 'required', residentKey: 'preferred' },
};

export const authenticateOptions = {
  challenge: 'YXV0aA',
  rpId: 'localhost',
  allowCredentials: [],
  userVerification: 'required',
};

export function security(overrides: Partial<MfaSecurityView> = {}): MfaSecurityView {
  return {
    mfa_required: false,
    mfa_partial: false,
    mfa_enrolled: true,
    mfa_verified_at: Date.now(),
    mfa_method: 'totp',
    allow_passkeys: true,
    allow_authenticator: true,
    fresh_minutes: 10,
    ...overrides,
  };
}

export interface AuthState {
  passkeys: { id: string; name: string | null; createdAt: string; lastUsedAt: string | null }[];
  totp: boolean;
  security: MfaSecurityView;
  sessions: {
    id: string;
    token: string;
    createdAt: number;
    expiresAt: number;
    userAgent: string;
    ipAddress: string;
  }[];
}

export interface Recorded {
  path: string;
  body: unknown;
}

/** Serve the auth service's MFA routes and /v1/me/security from `state`. */
export function serveAuth(state: AuthState) {
  const calls: Recorded[] = [];
  const record = async (request: Request, path: string) => {
    const text = request.method === 'POST' ? await request.text() : '';
    calls.push({ path, body: text ? JSON.parse(text) : undefined });
  };
  server.use(
    http.get('*/v1/me', () => HttpResponse.json({ user: { id: 'usr_1' }, org: { id: 'org_1' } })),
    http.get('*/v1/me/security', () =>
      HttpResponse.json({
        ...state.security,
        authenticator_enrolled: state.totp,
        passkey_count: state.passkeys.length,
      }),
    ),
    http.get('*/api/auth/get-session', () =>
      HttpResponse.json({
        session: { id: 'ses_1' },
        user: { id: 'au_1', email: 'ada@example.test' },
      }),
    ),
    http.get('*/api/auth/list-sessions', () => HttpResponse.json(state.sessions)),
    http.get('*/api/auth/passkey/list-user-passkeys', () => HttpResponse.json(state.passkeys)),
    http.get('*/api/auth/passkey/generate-register-options', async ({ request }) => {
      await record(request, 'generate-register-options');
      return HttpResponse.json(registerOptions);
    }),
    http.get('*/api/auth/passkey/generate-authenticate-options', async ({ request }) => {
      await record(request, 'generate-authenticate-options');
      return HttpResponse.json(authenticateOptions);
    }),
    http.post('*/api/auth/passkey/verify-registration', async ({ request }) => {
      await record(request, 'verify-registration');
      const body = calls.at(-1)!.body as { name?: string };
      state.passkeys.push({
        id: `pk_${state.passkeys.length + 1}`,
        name: body.name ?? null,
        createdAt: new Date().toISOString(),
        lastUsedAt: null,
      });
      state.security = { ...state.security, mfa_enrolled: true, mfa_partial: false };
      return HttpResponse.json({ status: true });
    }),
    http.post('*/api/auth/passkey/verify-authentication', async ({ request }) => {
      await record(request, 'verify-authentication');
      state.security = {
        ...state.security,
        mfa_partial: false,
        mfa_verified_at: Date.now(),
        mfa_method: 'passkey',
      };
      return HttpResponse.json({ status: true });
    }),
    http.post('*/api/auth/passkey/update-passkey', async ({ request }) => {
      await record(request, 'update-passkey');
      const body = calls.at(-1)!.body as { id: string; name: string };
      const item = state.passkeys.find((p) => p.id === body.id);
      if (item) item.name = body.name;
      return HttpResponse.json({ passkey: item });
    }),
    http.post('*/api/auth/passkey/delete-passkey', async ({ request }) => {
      await record(request, 'delete-passkey');
      const body = calls.at(-1)!.body as { id: string };
      state.passkeys = state.passkeys.filter((p) => p.id !== body.id);
      return HttpResponse.json({ status: true });
    }),
    http.post('*/api/auth/two-factor/enable', async ({ request }) => {
      await record(request, 'enable');
      return HttpResponse.json({
        totpURI:
          'otpauth://totp/The%20Runbook:ada%40example.test?secret=JBSWY3DPEHPK3PXP&issuer=The%20Runbook',
        backupCodes: ['aaaaa-11111', 'bbbbb-22222', 'ccccc-33333'],
      });
    }),
    http.post('*/api/auth/two-factor/verify-totp', async ({ request }) => {
      await record(request, 'verify-totp');
      const { code } = calls.at(-1)!.body as { code: string };
      if (code !== '123456') {
        return HttpResponse.json(
          { code: 'INVALID_CODE', message: 'Invalid code' },
          { status: 401 },
        );
      }
      if (!state.totp) {
        // Confirming a setup enrolls the app; it is not a step-up.
        state.totp = true;
        state.security = { ...state.security, mfa_enrolled: true };
        return HttpResponse.json({ status: true, enrolled: true });
      }
      state.security = {
        ...state.security,
        mfa_enrolled: true,
        mfa_partial: false,
        mfa_verified_at: Date.now(),
        mfa_method: 'totp',
      };
      return HttpResponse.json({ status: true });
    }),
    http.post('*/api/auth/two-factor/verify-backup-code', async ({ request }) => {
      await record(request, 'verify-backup-code');
      state.security = { ...state.security, mfa_partial: false, mfa_verified_at: Date.now() };
      return HttpResponse.json({ status: true });
    }),
    http.post('*/api/auth/two-factor/disable', async ({ request }) => {
      await record(request, 'disable');
      state.totp = false;
      return HttpResponse.json({ status: true });
    }),
    http.post('*/api/auth/two-factor/generate-backup-codes', async ({ request }) => {
      await record(request, 'generate-backup-codes');
      return HttpResponse.json({ status: true, backupCodes: ['zzzzz-99999', 'yyyyy-88888'] });
    }),
  );
  return calls;
}

export function defaultState(overrides: Partial<AuthState> = {}): AuthState {
  return {
    passkeys: [
      {
        id: 'pk_1',
        name: 'Work laptop',
        createdAt: '2026-11-01T10:00:00.000Z',
        lastUsedAt: new Date(Date.now() - 2 * 3_600_000).toISOString(),
      },
    ],
    totp: true,
    security: security(),
    sessions: [
      {
        id: 'ses_1',
        token: 'tok_1',
        createdAt: Date.now() - 3_600_000,
        expiresAt: Date.now() + 86_400_000,
        userAgent: 'Chrome on macOS',
        ipAddress: '127.0.0.1',
      },
    ],
    ...overrides,
  };
}

export function renderWithProviders(ui: ReactNode, options: { router?: boolean } = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    client,
    ui: (
      <QueryClientProvider client={client}>
        {options.router === false ? ui : <TestRouter>{ui}</TestRouter>}
      </QueryClientProvider>
    ),
  };
}
