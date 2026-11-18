// Better Auth database hooks (D45). Identity linking stays in Rust: before Better
// Auth creates a user, we ask Rust's internal `can-register {email}` whether this
// verified address may have an account (an existing directory user or a pending
// invitation). The answer comes only after the code (or Google) has proven the
// mailbox, so a refusal tells nobody anything they did not already know.
import { APIError } from 'better-auth/api';
import { type AuthConfig, domainAllowed } from './config';

export type Fetch = (input: Request | string | URL, init?: RequestInit) => Promise<Response>;

/** Header carrying the shared internal secret, both directions. */
export const INTERNAL_SECRET_HEADER = 'x-runbook-internal-secret';

const CAN_REGISTER_TIMEOUT_MS = 5000;

export const refuse = (code: string, message: string) => new APIError('FORBIDDEN', { code, message });

/**
 * Calls Rust's `POST can-register {"email"}`; expects `200 {"allowed": boolean}`.
 * Anything else, including a timeout or a network error, refuses: registration
 * fails closed.
 */
export async function canRegister(config: AuthConfig, email: string, fetchImpl: Fetch): Promise<boolean> {
  if (!config.canRegisterUrl) return false;
  try {
    const response = await fetchImpl(config.canRegisterUrl, {
      method: 'POST',
      headers: { 'content-type': 'application/json', [INTERNAL_SECRET_HEADER]: config.internalSecret },
      body: JSON.stringify({ email }),
      signal: AbortSignal.timeout(CAN_REGISTER_TIMEOUT_MS),
      redirect: 'manual',
    });
    if (response.status !== 200) return false;
    const body = (await response.json()) as { allowed?: unknown };
    return body.allowed === true;
  } catch {
    return false;
  }
}

interface HookUser {
  email?: unknown;
  emailVerified?: unknown;
}
interface HookSession {
  userId?: unknown;
}
interface HookContext {
  context?: { internalAdapter?: { findUserById(id: string): Promise<{ email: string } | null> } };
}

export function databaseHooks(config: AuthConfig, fetchImpl: Fetch) {
  return {
    user: {
      create: {
        async before(user: HookUser) {
          const email = String(user.email ?? '').toLowerCase();
          if (!email.includes('@')) throw refuse('auth.registration_refused', 'This address cannot sign in here.');
          if (!domainAllowed(config, email)) throw refuse('auth.domain_not_allowed', 'This email domain cannot sign in here.');
          // Email codes always verify; Google is filtered in app.ts. Belt and braces.
          if (user.emailVerified !== true) throw refuse('auth.email_unverified', 'Sign-in needs a verified email address.');
          if (!(await canRegister(config, email, fetchImpl))) {
            throw refuse('auth.registration_refused', 'This address has no account or invitation here. Ask an administrator to invite you.');
          }
        },
      },
    },
    session: {
      create: {
        // Covers every path to a session (an existing Google account included):
        // the domain restriction applies to signing in, not only to registering.
        async before(session: HookSession, ctx: HookContext | null) {
          const adapter = ctx?.context?.internalAdapter;
          if (!adapter || config.allowedDomains.length === 0) return;
          const owner = await adapter.findUserById(String(session.userId));
          if (!owner || !domainAllowed(config, owner.email)) throw refuse('auth.domain_not_allowed', 'This email domain cannot sign in here.');
        },
      },
    },
  };
}
