import { useNavigate, useSearch } from '@tanstack/react-router';
import { QueryClientContext } from '@tanstack/react-query';
import { useCallback, useContext, useEffect, useState } from 'react';
import { createApiRuntime } from '../api/client';
import { SignInForm } from '../features/auth/SignInForm';
import { BrandMark } from '../ui/layout/BrandMark';
import { detectCookieSession, isCookieSession, signOut } from '../features/auth/session';
import { fetchMySecurity, type MfaSecurityView } from '../features/mfa/hooks';
import { MfaGate } from '../features/mfa/MfaGate';

/** Only same-origin paths may be honoured as a post-sign-in destination. */
export function safeNext(next: string | undefined): string {
  if (next && next.startsWith('/') && !next.startsWith('//') && next !== '/sign-in') return next;
  return '/home';
}

/**
 * After the first factor: does this session still need a second one? A
 * partial session must verify (or enroll); an enrolled person whose
 * organization does not require MFA is asked too, and may skip.
 */
export async function secondFactorNeeded(): Promise<MfaSecurityView | null> {
  if (!isCookieSession()) return null;
  try {
    // A throwaway client: no session-expired redirect, no step-up prompt.
    const probe = createApiRuntime({ getToken: () => null, onUnauthorized: null });
    const security = await fetchMySecurity(probe.client);
    if (security.mfa_partial) return security;
    if (security.mfa_enrolled && security.mfa_verified_at === null) return security;
    return null;
  } catch {
    // The shell checks again and shows the gate if the session is partial.
    return null;
  }
}

export function SignInPage() {
  const navigate = useNavigate();
  const queryClient = useContext(QueryClientContext);
  const search = useSearch({ strict: false }) as { reason?: string; next?: string; error?: string };
  const [gate, setGate] = useState<MfaSecurityView | null>(null);
  const [signingOut, setSigningOut] = useState(false);
  const [reason, setReason] = useState<string | null>(search.reason ?? null);

  const continueSignIn = useCallback(async () => {
    const needed = await secondFactorNeeded();
    if (needed) setGate(needed);
    else void navigate({ to: safeNext(search.next) });
  }, [navigate, search.next]);

  // A browser that still holds a live sign-in cookie goes straight in (after
  // any second factor). Not after a refusal (`reason`): that would loop back
  // to what refused it.
  useEffect(() => {
    if (search.reason) return;
    let cancelled = false;
    void detectCookieSession().then((live) => {
      if (live && !cancelled) void continueSignIn();
    });
    return () => {
      cancelled = true;
    };
  }, [continueSignIn, search.reason]);

  if (gate) {
    return (
      <MfaGate
        security={gate}
        signingOut={signingOut}
        onDone={() => {
          queryClient?.clear();
          void navigate({ to: safeNext(search.next) });
        }}
        onSignOut={() => {
          setSigningOut(true);
          void signOut().then(() => {
            setSigningOut(false);
            setGate(null);
            setReason('mfa-signed-out');
          });
        }}
      />
    );
  }

  return (
    <main className="mx-auto grid min-h-dvh max-w-md content-center gap-4 p-4">
      <div className="grid gap-1">
        <div className="mb-4 flex items-center gap-2">
          <BrandMark className="size-8" />
          <span className="text-lg font-semibold text-fg">The Runbook</span>
        </div>
        <h1 className="text-xl font-semibold">Sign in</h1>
        <p className="text-fg-muted">Sign in to your organization to continue.</p>
      </div>
      <SignInForm
        reason={reason}
        oauthError={search.error ?? null}
        onSignedIn={() => void continueSignIn()}
      />
    </main>
  );
}
