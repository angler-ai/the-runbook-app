import { createOTP } from '@better-auth/utils/otp';
import { generateRandomString, symmetricDecrypt, symmetricEncrypt } from 'better-auth/crypto';
import { passkey } from '@better-auth/passkey';
import { APIError, createAuthMiddleware, getSessionFromCtx } from 'better-auth/api';
import { setSessionCookie } from 'better-auth/cookies';
import { twoFactor } from 'better-auth/plugins';
import type { Auth, AppDeps } from './app';
import { bindMfaGeneration } from './mfa-generation';
import { clientKey, sha256Hex, totpSetupId } from './limits';
import { INTERNAL_SECRET_HEADER } from './hooks';

export const MFA_ROUTES = new Set([
  'GET /passkey/generate-register-options',
  'POST /passkey/verify-registration',
  'GET /passkey/generate-authenticate-options',
  'POST /passkey/verify-authentication',
  'GET /passkey/list-user-passkeys',
  'POST /passkey/update-passkey',
  'POST /passkey/delete-passkey',
  'POST /two-factor/enable',
  'POST /two-factor/disable',
  'POST /two-factor/verify-totp',
  'POST /two-factor/verify-backup-code',
  'POST /two-factor/generate-backup-codes',
]);

export const PASSKEY_ASSERTION = '/passkey/verify-authentication';
const methods = {
  '/two-factor/verify-totp': 'totp',
  '/two-factor/verify-backup-code': 'backup_code',
  '/passkey/verify-registration': 'passkey',
} as const;

const setupLifetimeMs = 600_000;
const totpOptions = { period: 30, digits: 6 } as const;
const backupPattern = /^[a-zA-Z0-9]{5}-[a-zA-Z0-9]{5}$/;
async function backupHash(code: string, secret: string): Promise<string> {
  const encoder = new TextEncoder();
  const key = await crypto.subtle.importKey('raw', encoder.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  const digest = await crypto.subtle.sign('HMAC', key, encoder.encode(`runbook-backup-code\n${code}`));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export function mfaPlugins(deps: AppDeps) {
  const origin = deps.config.origin!;
  const secret = deps.config.secret!;
  return [
    twoFactor({
      allowPasswordless: true,
      totpOptions,
      backupCodeOptions: {
        amount: 10,
        storeBackupCodes: {
          // The plugin compares and consumes the submitted value with a CAS.
          // Compare digests and persist digests on both creation and consumption.
          async encrypt(json) {
            const codes = JSON.parse(json) as string[];
            return JSON.stringify(await Promise.all(codes.map((code) => backupPattern.test(code) ? backupHash(code, secret) : code)));
          },
          async decrypt(json) { return json; },
        },
      },
    }),
    passkey({
      rpID: new URL(origin).hostname,
      rpName: 'The Runbook',
      origin,
      registration: {
        async afterVerification({ verification, ctx, context }) {
          if (!verification.registrationInfo?.userVerified) throw new APIError('FORBIDDEN', { code: 'auth.passkey_uv_required', message: 'Verify your identity on the authenticator.' });
          const session = ctx.context.session?.session;
          if (!session || context !== session.id) {
            throw new APIError('FORBIDDEN', { code: 'auth.mfa_setup_changed', message: 'Start passkey setup again.' });
          }
          await bindMfaGeneration((session as { mfaGeneration?: number }).mfaGeneration ?? 0);
        },
      },
      authentication: {
        async afterVerification({ verification, ctx, clientData }) {
          if (!verification.authenticationInfo.userVerified) throw new APIError('FORBIDDEN', { code: 'auth.passkey_uv_required', message: 'Verify your identity on the authenticator.' });
          const credential = await ctx.context.adapter.findOne<{ id: string; userId: string }>({
            model: 'passkey', where: [{ field: 'credentialID', value: clientData.id }],
          });
          const previous = await getSessionFromCtx(ctx);
          if (previous && previous.user.id !== credential?.userId) {
            throw new APIError('FORBIDDEN', { code: 'auth.passkey_account_mismatch', message: 'Use a passkey from the signed-in account.' });
          }
          if (!credential) throw new APIError('UNAUTHORIZED', { code: 'auth.passkey_assertion_failed', message: 'Could not verify this passkey.' });
          const user = await ctx.context.internalAdapter.findUserById(credential.userId);
          if (!user?.emailVerified) throw new APIError('UNAUTHORIZED', { code: 'auth.passkey_assertion_failed', message: 'Could not verify this passkey.' });
          const now = Date.now();
          const policy = await policyForFacts(deps, {
            auth_user_id: user.id, email: user.email, email_verified: true,
            session_id: previous?.session.id ?? `passkey:${credential.id}`, expires_at: now + 60_000,
            mfa_enrolled: true, mfa_method: 'passkey', mfa_verified_at: now,
          });
          if (!policy) throw new APIError('SERVICE_UNAVAILABLE', { code: 'auth.unavailable', message: 'The organization security policy could not be checked.' });
          if (!policy.allow_passkeys) throw new APIError('FORBIDDEN', { code: 'auth.mfa_factor_disallowed', message: 'Your organization does not allow this factor.' });
          await ctx.context.adapter.update({
            model: 'passkey', where: [{ field: 'id', value: credential.id }], update: { lastUsedAt: new Date() },
          });
        },
      },
    }),
    {
      id: 'runbook-mfa',
      schema: {
        oauthAccessToken: { fields: {
          mfaVerifiedAt: { type: 'date' as const, required: false, input: false },
          mfaMethod: { type: 'string' as const, required: false, input: false },
        } },
        oauthRefreshToken: { fields: {
          mfaVerifiedAt: { type: 'date' as const, required: false, input: false },
          mfaMethod: { type: 'string' as const, required: false, input: false },
        } },
        passkey: { fields: {
          lastUsedAt: { type: 'date' as const, required: false },
          mfaGeneration: { type: 'number' as const, required: false, input: false },
        } },
      },
      hooks: {
        after: [{
          matcher: (ctx: { path?: string }) => ctx.path === PASSKEY_ASSERTION,
          handler: createAuthMiddleware(async (ctx) => {
            const previous = ctx.context.session?.session;
            const signedIn = ctx.context.newSession?.session;
            if (previous && signedIn && previous.token !== signedIn.token) {
              await ctx.context.internalAdapter.deleteSession(previous.token);
            }
          }),
        }, {
          matcher: (ctx: { path?: string }) => Object.hasOwn(methods, ctx.path ?? ''),
          handler: createAuthMiddleware(async (ctx) => {
            const returned = ctx.context.returned;
            if (!returned || returned instanceof APIError || returned instanceof Response && !returned.ok) return;
            const enrollmentSession = ctx.context.newSession;
            const current = enrollmentSession ?? ctx.context.session;
            if (!current?.session) return;
            const method = methods[ctx.path as keyof typeof methods];
            if (method === 'totp') {
              const code = (ctx.body as { code: string }).code;
              const digest = await backupHash(code, secret);
              const key = `mfa-used:${await sha256Hex(`${current.user.id}\n${digest}`)}`;
              if (!(await deps.store.claim(key, Date.now(), 0, 1, 90_000))) {
                throw new APIError('UNAUTHORIZED', { code: 'auth.mfa_code_reused', message: 'Wait for your authenticator to show a new code.' });
              }
            }
            await bindMfaGeneration((current.session as { mfaGeneration?: number }).mfaGeneration ?? 0);
            const replacement = await ctx.context.internalAdapter.createSession(current.user.id, false, {
              ...current.session, mfaVerifiedAt: new Date(), mfaMethod: method,
            });
            if (!replacement) throw new APIError('INTERNAL_SERVER_ERROR');
            await setSessionCookie(ctx, { session: replacement, user: current.user });
            await ctx.context.internalAdapter.deleteSession(current.session.token);
            if (method === 'passkey' || method === 'backup_code') {
              await deliverMfaNotifications(deps, current.user.id);
            }
            // Verify endpoints may return the old token. Browser callers use
            // the signed cookie and need only the success status.
            return ctx.json({ status: true });
          }),
        }],
      },
    } satisfies import('better-auth').BetterAuthPlugin,
  ] as const;
}

async function authenticatorEnrolled(auth: Auth, userId: string): Promise<boolean> {
  const { adapter } = await auth.$context;
  const factor = await adapter.findOne<{ verified: boolean }>({ model: 'twoFactor', where: [{ field: 'userId', value: userId }] });
  return factor?.verified === true;
}

export interface MfaFactors {
  auth_user_id: string;
  email: string;
  passkeys: number;
  authenticator: boolean;
}

export async function mfaEnrolled(auth: Auth, user: { id: string }): Promise<boolean> {
  if (await authenticatorEnrolled(auth, user.id)) return true;
  const { adapter } = await auth.$context;
  return (await adapter.count({ model: 'passkey', where: [{ field: 'userId', value: user.id }] })) > 0;
}

type LiveSession = NonNullable<Awaited<ReturnType<Auth['api']['getSession']>>>;

export async function sessionFacts(auth: Auth, { session, user }: LiveSession) {
  return {
    auth_user_id: user.id,
    email: user.email,
    email_verified: user.emailVerified === true,
    ...(typeof user.name === 'string' && user.name.trim() !== '' ? { name: user.name.trim() } : {}),
    ...(typeof user.image === 'string' && user.image.startsWith('https://') ? { image: user.image } : {}),
    session_id: session.id,
    expires_at: new Date(session.expiresAt).getTime(),
    mfa_verified_at: session.mfaVerifiedAt ? new Date(session.mfaVerifiedAt).getTime() : null,
    mfa_method: session.mfaMethod ?? null,
    mfa_enrolled: await mfaEnrolled(auth, user),
  };
}

interface SecurityPolicy {
  mfa_required: boolean;
  mfa_partial: boolean;
  allow_passkeys: boolean;
  allow_authenticator: boolean;
  fresh_minutes: number;
}

export async function securityPolicy(auth: Auth, deps: AppDeps, current: LiveSession): Promise<SecurityPolicy | null> {
  return policyForFacts(deps, await sessionFacts(auth, current));
}

async function policyForFacts(deps: AppDeps, facts: Record<string, unknown>): Promise<SecurityPolicy | null> {
  if (!deps.config.canRegisterUrl) return null;
  const url = new URL('/internal/mfa-policy', deps.config.canRegisterUrl);
  try {
    const response = await deps.fetch(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json', [INTERNAL_SECRET_HEADER]: deps.config.internalSecret },
      body: JSON.stringify(facts),
      signal: AbortSignal.timeout(5000),
      redirect: 'manual',
    });
    if (!response.ok) return null;
    const value = await response.json() as SecurityPolicy;
    return typeof value.mfa_required === 'boolean' && typeof value.mfa_partial === 'boolean'
      && typeof value.allow_passkeys === 'boolean' && typeof value.allow_authenticator === 'boolean'
      && Number.isInteger(value.fresh_minutes) && value.fresh_minutes >= 5 && value.fresh_minutes <= 60 ? value : null;
  } catch {
    return null;
  }
}

export function allowedProofTime(session: LiveSession['session'], policy: SecurityPolicy): number | null {
  const allowed = session.mfaMethod === 'passkey' ? policy.allow_passkeys
    : (session.mfaMethod === 'totp' || session.mfaMethod === 'backup_code') && policy.allow_authenticator;
  const time = session.mfaVerifiedAt ? new Date(session.mfaVerifiedAt).getTime() : 0;
  return allowed && time > 0 && time <= Date.now() ? time : null;
}

function freshAllowedProof(session: LiveSession['session'], policy: SecurityPolicy): boolean {
  const time = allowedProofTime(session, policy);
  return time !== null && Date.now() - time <= policy.fresh_minutes * 60_000;
}

function enrollmentStepUp(policy: SecurityPolicy) {
  return Response.json({
    code: 'auth.mfa_step_up', message: 'Verify an existing factor before changing security settings.',
    fresh_minutes: policy.fresh_minutes,
  }, { status: 403 });
}

const problem = (status: number, code: string, message: string) => Response.json({ code, message }, { status });

export async function prepareMfaRequest(auth: Auth, deps: AppDeps, path: string, request: Request): Promise<Request | Response> {
  if (path === '/passkey/generate-authenticate-options' || path === PASSKEY_ASSERTION) return request;
  const current = await auth.api.getSession({ headers: request.headers, query: { disableRefresh: true } });
  if (!current) return problem(401, 'auth.no_session', 'Sign in first.');
  const { session, user } = current;
  const policy = await securityPolicy(auth, deps, current);
  if (!policy) return problem(503, 'auth.unavailable', 'The organization security policy could not be checked.');
  if (path === '/passkey/list-user-passkeys') {
    if (policy.mfa_partial || await mfaEnrolled(auth, user) && allowedProofTime(session, policy) === null) {
      return problem(403, 'auth.mfa_required', 'Verify a second factor before listing passkeys.');
    }
    return request;
  }
  const pendingId = totpSetupId(session.id);
  if (path === '/two-factor/verify-totp' || path === '/two-factor/verify-backup-code') {
    if (!policy.allow_authenticator) return problem(403, 'auth.mfa_factor_disallowed', 'Your organization does not allow this factor.');
    const body = await request.json() as { code?: unknown };
    const isBackup = path.endsWith('backup-code');
    if (typeof body.code !== 'string' || !(isBackup ? backupPattern : /^\d{6}$/).test(body.code)) {
      return problem(400, 'auth.invalid_code', 'Enter a valid verification code.');
    }
    const enrolled = await authenticatorEnrolled(auth, user.id);
    const context = await auth.$context;
    const setup = !isBackup && !enrolled ? await context.internalAdapter.findVerificationValue(pendingId) : null;
    const pending = setup && setup.expiresAt.getTime() > Date.now() ? setup : null;
    if (!enrolled && !pending) return problem(400, 'auth.invalid_code', 'No verified authenticator or active setup was found.');
    const fresh = freshAllowedProof(session, policy);
    if (pending && !fresh && (policy.mfa_required || await mfaEnrolled(auth, user))) return enrollmentStepUp(policy);
    const code = isBackup ? await backupHash(body.code, deps.config.secret!) : body.code;
    const headers = new Headers(request.headers);
    headers.delete('content-length');
    const address = clientKey(request.headers.get(deps.clientIpHeader));
    const factorKey = isBackup ? 'backup' : 'totp';
    const bucket = address === 'unknown' ? session.id : await sha256Hex(address);
    const key = pending ? `mfa-setup-attempt:${session.id}` : `mfa-attempt:${user.id}:${factorKey}:${bucket}`;
    const now = Date.now();
    const addressAllowed = await deps.store.claim(key, now, 0, 5, 600_000, 900_000);
    const accountAllowed = pending || !addressAllowed || await deps.store.claim(
      `mfa-account-attempt:${user.id}:${factorKey}`, now, 0, 10, 600_000, 900_000,
    );
    if (!addressAllowed || !accountAllowed) {
      const auditKey = accountAllowed ? `mfa-lock-audit:${user.id}` : `mfa-lock-audit:account:${user.id}:${factorKey}`;
      if (await deps.store.claim(auditKey, now, 0, 1, 900_000)) {
        await deps.store.auditMfa(user.id, 'mfa.locked', 'system');
      }
      return Response.json({ code: 'auth.mfa_locked', message: 'Too many verification attempts. Try again later.' }, { status: 429, headers: { 'retry-after': '900' } });
    }
    if (pending) {
      const factor = JSON.parse(pending.value) as { secret: string; backupCodes: string };
      const secret = await symmetricDecrypt({ data: factor.secret, key: context.secretConfig });
      if (!(await createOTP(secret, totpOptions).verify(code))) return problem(401, 'auth.invalid_code', 'Enter a valid verification code.');
      const digest = await backupHash(code, deps.config.secret!);
      const key = `mfa-used:${await sha256Hex(`${user.id}\n${digest}`)}`;
      if (!(await deps.store.claim(key, Date.now(), 0, 1, 90_000))) {
        return problem(401, 'auth.mfa_code_reused', 'Wait for your authenticator to show a new code.');
      }
      const consumed = await context.internalAdapter.consumeVerificationValue(pendingId);
      if (consumed?.id !== pending.id || !(await deps.store.confirmTotp(user.id, session.id, factor, fresh))) {
        return problem(409, 'auth.mfa_setup_changed', 'Start a new authenticator setup.');
      }
      await deliverMfaNotifications(deps, user.id);
      return Response.json({ status: true, enrolled: true });
    }
    return new Request(request.url, { method: 'POST', headers, body: JSON.stringify({ code }) });
  }
  const needsFresh = (path === '/two-factor/enable' && policy.mfa_required) || await mfaEnrolled(auth, user);
  if (needsFresh && !freshAllowedProof(session, policy)) return enrollmentStepUp(policy);
  if (path === '/passkey/delete-passkey' || path === '/two-factor/disable') {
    let id: string | null = null;
    if (path === '/passkey/delete-passkey') {
      const body = await request.json() as { id?: unknown };
      if (typeof body.id !== 'string' || !body.id || body.id.length > 256) return problem(400, 'auth.invalid_request', 'A passkey ID is required.');
      id = body.id;
    }
    if (!(await deps.store.removeMfa(user.id, id, { passkeys: policy.allow_passkeys, totp: policy.allow_authenticator, required: policy.mfa_required }))) {
      return problem(409, 'auth.mfa_removal_refused', 'Keep at least one allowed factor when your organization requires MFA.');
    }
    await deliverMfaNotifications(deps, user.id);
    return Response.json({ status: true });
  }
  if (['/passkey/generate-register-options', '/passkey/verify-registration', '/two-factor/enable'].includes(path)) {
    if (path === '/two-factor/enable' ? !policy.allow_authenticator : !policy.allow_passkeys) {
      return problem(403, 'auth.mfa_factor_disallowed', 'Your organization does not allow this factor.');
    }
  }
  if (path === '/passkey/generate-register-options') {
    const url = new URL(request.url);
    url.searchParams.set('context', session.id);
    return new Request(url, request);
  }
  if (path === '/two-factor/enable') {
    if (await authenticatorEnrolled(auth, user.id)) return problem(409, 'auth.mfa_already_enrolled', 'An authenticator is already enrolled.');
    const context = await auth.$context;
    const secret = generateRandomString(32);
    const backupCodes = Array.from({ length: 10 }, () => {
      const code = generateRandomString(10, 'a-z', 'A-Z', '0-9');
      return `${code.slice(0, 5)}-${code.slice(5)}`;
    });
    const factor = {
      secret: await symmetricEncrypt({ key: context.secretConfig, data: secret }),
      backupCodes: JSON.stringify(await Promise.all(backupCodes.map((code) => backupHash(code, deps.config.secret!)))),
    };
    if (!(await deps.store.startTotp(user.id, session.id, factor, freshAllowedProof(session, policy), new Date(Date.now() + setupLifetimeMs)))) {
      return problem(409, 'auth.mfa_setup_changed', 'Start a new authenticator setup.');
    }
    await deliverMfaNotifications(deps, user.id);
    return Response.json({ method: 'totp', totpURI: createOTP(secret, totpOptions).url(context.appName, user.email), backupCodes });
  }
  if (path === '/two-factor/generate-backup-codes') {
    if (!policy.allow_authenticator) return problem(403, 'auth.mfa_factor_disallowed', 'Your organization does not allow this factor.');
    const headers = new Headers(request.headers);
    headers.delete('content-length');
    return new Request(request.url, { method: 'POST', headers, body: '{}' });
  }
  if (path === '/passkey/verify-registration') {
    const { response, name } = await request.json() as { response?: unknown; name?: unknown };
    const headers = new Headers(request.headers);
    headers.delete('content-length');
    return new Request(request.url, { method: 'POST', headers, body: JSON.stringify({ response, name }) });
  }
  return request;
}

/** The plugin's defaults ask for preferred UV. Ask browsers to require it. */
export async function requirePasskeyUv(path: string, response: Response): Promise<Response> {
  if (!response.ok || !['/passkey/generate-register-options', '/passkey/generate-authenticate-options'].includes(path)) return response;
  const options = await response.json() as Record<string, unknown>;
  if (path === '/passkey/generate-register-options') {
    options.authenticatorSelection = { ...options.authenticatorSelection as object, userVerification: 'required' };
  } else {
    options.userVerification = 'required';
  }
  const headers = new Headers(response.headers);
  headers.delete('content-length');
  return Response.json(options, { status: response.status, headers });
}

export async function deliverMfaNotifications(deps: Pick<AppDeps, 'config' | 'transport' | 'store'>, userId?: string) {
  const { config, store, transport } = deps;
  if (!transport || !config.email) return;
  const subject = 'Your Runbook security settings changed';
  const descriptions: Record<string, string> = {
    started: 'Authenticator setup was started on your Runbook account.',
    enrolled: 'A security factor was added to your Runbook account.',
    removed: 'A security factor was removed from your Runbook account.',
    reset: 'The security factors and sign-in sessions on your Runbook account were reset.',
    used: 'A backup code was used on your Runbook account.',
    regenerated: 'The backup codes on your Runbook account were replaced.',
  };

  try {
    for (const event of await store.pendingMfaNotifications(userId)) {
      const text = `${descriptions[event.notification] ?? 'The security settings on your Runbook account changed.'} Review your account security at ${config.origin}/settings/security.`;
      try {
        await transport.send({ from: config.email.from, to: event.email, subject, text, html: '' }, '', 'notification');
        await store.markMfaNotified(event.id);
      } catch {
        console.error('Runbook could not deliver a queued MFA security notification.');
      }
    }
  } catch {
    console.error('Runbook could not read queued MFA security notifications.');
  }
}
