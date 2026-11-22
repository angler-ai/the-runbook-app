// Second-factor calls to the auth service (`/api/auth`, auth/README.md
// "Multi-factor authentication"). Its errors are `{code, message}` JSON; a
// stale session gets `403 auth.mfa_step_up`, which goes through the same
// step-up prompt as the core API's refusals and is retried once.
import { ApiError } from '../../api/client';
import { readStepUpPrincipal, requestStepUp } from '../../api/step-up';
import { securityCodeMessage } from './messages';
import {
  credentialToJSON,
  toCreationOptions,
  toRequestOptions,
  type CreationOptionsJSON,
  type RequestOptionsJSON,
} from './webauthn';

export class AuthApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly body: Record<string, unknown>;

  constructor(status: number, code: string, message: string, body: Record<string, unknown> = {}) {
    super(message);
    this.name = 'AuthApiError';
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

interface AuthRequestOptions {
  method?: 'GET' | 'POST';
  body?: unknown;
  /**
   * Answer `auth.mfa_step_up` with the prompt and retry once (default true).
   * A POST refused with `auth.mfa_required` (session routes, for an enrolled
   * account that has not verified a factor) gets the same prompt.
   */
  stepUp?: boolean;
}

async function send(path: string, method: 'GET' | 'POST', body: unknown): Promise<Response> {
  const headers: Record<string, string> = { 'x-runbook-csrf': '1' };
  if (method === 'POST') {
    headers['content-type'] = 'application/json';
    headers.origin = globalThis.location?.origin ?? '';
  }
  return fetch(`/api/auth${path}`, {
    method,
    credentials: 'same-origin',
    headers,
    ...(method === 'POST' ? { body: JSON.stringify(body ?? {}) } : {}),
  });
}

async function errorOf(response: Response): Promise<AuthApiError> {
  let body: Record<string, unknown> = {};
  try {
    const parsed: unknown = await response.json();
    if (parsed && typeof parsed === 'object') body = parsed as Record<string, unknown>;
  } catch {
    // Non-JSON error: status only.
  }
  const code = typeof body.code === 'string' ? body.code : `http.${response.status}`;
  const message = typeof body.message === 'string' ? body.message : response.statusText;
  return new AuthApiError(response.status, code, message, body);
}

export async function authRequest<T>(path: string, options: AuthRequestOptions = {}): Promise<T> {
  const method = options.method ?? (options.body === undefined ? 'GET' : 'POST');
  let response: Response;
  try {
    response = await send(path, method, options.body);
  } catch {
    throw new AuthApiError(0, 'network', 'Could not reach the sign-in service. Try again.');
  }
  if (!response.ok) {
    const error = await errorOf(response);
    if (
      response.url &&
      !response.redirected &&
      new URL(response.url).origin === globalThis.location.origin &&
      error.status === 403 &&
      typeof error.body.message === 'string' &&
      (error.code === 'auth.mfa_step_up' ||
        (error.code === 'auth.mfa_required' && method === 'POST')) &&
      options.stepUp !== false
    ) {
      const origin = globalThis.location.origin;
      const principal = await readStepUpPrincipal(origin);
      if (principal && await requestStepUp(error, { method }) && principal === await readStepUpPrincipal(origin)) {
        return authRequest<T>(path, { ...options, stepUp: false });
      }
    }
    throw error;
  }
  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

/** What people read for an auth-service or WebAuthn failure. */
export function mfaErrorMessage(error: unknown): string {
  if (error instanceof AuthApiError) {
    switch (error.code) {
      case 'auth.invalid_code':
      case 'INVALID_CODE':
      case 'INVALID_TWO_FACTOR_AUTHENTICATION':
      case 'INVALID_BACKUP_CODE':
        return 'That code is not right. Check it and try again.';
      case 'network':
        return error.message;
      default: {
        const known = securityCodeMessage(error.code);
        if (known) return known;
        return error.status === 401
          ? 'That did not work. Check it and try again.'
          : error.message || 'Something went wrong. Try again.';
      }
    }
  }
  if (error instanceof ApiError) {
    return securityCodeMessage(error.code) ?? `The server refused that (${error.code}).`;
  }
  if (error instanceof DOMException) {
    if (error.name === 'NotAllowedError' || error.name === 'AbortError') {
      return 'The passkey request was cancelled or timed out.';
    }
    if (error.name === 'InvalidStateError') {
      return 'This device already holds a passkey for your account.';
    }
    if (error.name === 'SecurityError') {
      return 'Passkeys need this site on https with a domain name, or on localhost. An IP address does not work.';
    }
  }
  return error instanceof Error && error.message
    ? error.message
    : 'Something went wrong. Try again.';
}

// --- Passkeys -------------------------------------------------------------

export interface PasskeyItem {
  id: string;
  name?: string | null;
  createdAt?: string | number | null;
  lastUsedAt?: string | number | null;
  deviceType?: string | null;
  backedUp?: boolean | null;
}

export const listPasskeys = () => authRequest<PasskeyItem[]>('/passkey/list-user-passkeys');

export async function registerPasskey(name: string): Promise<void> {
  const options = await authRequest<CreationOptionsJSON>('/passkey/generate-register-options');
  const credential = await navigator.credentials.create({ publicKey: toCreationOptions(options) });
  if (!credential) throw new DOMException('No passkey was created.', 'NotAllowedError');
  await authRequest('/passkey/verify-registration', {
    body: {
      response: credentialToJSON(credential as PublicKeyCredential),
      name: name.trim() || undefined,
    },
  });
}

/**
 * Sign in with a passkey, or verify one for the signed-in account. Either
 * way the auth service sets a new session cookie that counts as verified.
 */
export async function authenticateWithPasskey(): Promise<void> {
  const options = await authRequest<RequestOptionsJSON>('/passkey/generate-authenticate-options', {
    stepUp: false,
  });
  const credential = await navigator.credentials.get({ publicKey: toRequestOptions(options) });
  if (!credential) throw new DOMException('No passkey was used.', 'NotAllowedError');
  await authRequest('/passkey/verify-authentication', {
    body: { response: credentialToJSON(credential as PublicKeyCredential) },
    stepUp: false,
  });
}

export const renamePasskey = (id: string, name: string) =>
  authRequest('/passkey/update-passkey', { body: { id, name: name.trim() } });

export const deletePasskey = (id: string) =>
  authRequest('/passkey/delete-passkey', { body: { id } });

// --- Authenticator app and backup codes -----------------------------------

export interface TotpSetup {
  totpURI: string;
  backupCodes: string[];
}

export const enableTotp = () => authRequest<TotpSetup>('/two-factor/enable', { body: {} });

export const disableTotp = () => authRequest('/two-factor/disable', { body: {} });

export const regenerateBackupCodes = () =>
  authRequest<{ status: boolean; backupCodes: string[] }>('/two-factor/generate-backup-codes', {
    body: {},
  });

/** Codes are checked as typed: digits for the app, `xxxxx-xxxxx` for a backup code. */
export function normalizeCode(raw: string, kind: 'totp' | 'backup'): string {
  const trimmed = raw.replace(/\s+/g, '');
  if (kind === 'totp') return trimmed;
  const compact = trimmed.replace(/-/g, '');
  return compact.length === 10 ? `${compact.slice(0, 5)}-${compact.slice(5)}` : trimmed;
}

export function codeLooksValid(code: string, kind: 'totp' | 'backup'): boolean {
  return kind === 'totp' ? /^\d{6}$/.test(code) : /^[a-zA-Z0-9]{5}-[a-zA-Z0-9]{5}$/.test(code);
}

export const verifyTotp = (code: string) =>
  authRequest('/two-factor/verify-totp', { body: { code }, stepUp: false });

export const verifyBackupCode = (code: string) =>
  authRequest('/two-factor/verify-backup-code', { body: { code }, stepUp: false });
