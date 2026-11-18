import { useNavigate, useSearch } from '@tanstack/react-router';
import { useEffect } from 'react';
import { SignInForm } from '../features/auth/SignInForm';
import { BrandMark } from '../ui/layout/BrandMark';
import { detectCookieSession } from '../features/auth/session';

/** Only same-origin paths may be honoured as a post-sign-in destination. */
export function safeNext(next: string | undefined): string {
  if (next && next.startsWith('/') && !next.startsWith('//') && next !== '/sign-in') return next;
  return '/home';
}

export function SignInPage() {
  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as { reason?: string; next?: string; error?: string };
  // A browser that still holds a live sign-in cookie goes straight in. Not
  // after a refusal (`reason`): that would loop back to what refused it.
  useEffect(() => {
    if (search.reason) return;
    let cancelled = false;
    void detectCookieSession().then((live) => {
      if (live && !cancelled) void navigate({ to: safeNext(search.next) });
    });
    return () => {
      cancelled = true;
    };
  }, [navigate, search.reason, search.next]);
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
        reason={search.reason ?? null}
        oauthError={search.error ?? null}
        onSignedIn={() => void navigate({ to: safeNext(search.next) })}
      />
    </main>
  );
}
