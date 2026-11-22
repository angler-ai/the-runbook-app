import { KeyRound, Smartphone } from 'lucide-react';
import { type ReactNode } from 'react';
import { BrandMark } from '../../ui/layout/BrandMark';
import { AddPasskeyForm } from './AddPasskeyForm';
import { useRefreshSecurity, type MfaSecurityView } from './hooks';
import { resumeAt, safeReturnTo } from './returnTo';
import { SecondFactorForm, useFactorOptions } from './SecondFactorForm';
import { passkeysSupported } from './webauthn';

export interface MfaGateProps {
  security: MfaSecurityView;
  /** Where to go once verified or enrolled (when there is no `returnTo`). */
  onDone(): void;
  onSignOut(): void;
  /** An OAuth authorization to resume (validated as a local path here). */
  returnTo?: string | null;
  signingOut?: boolean;
}

function GateFrame({
  title,
  intro,
  resuming,
  children,
}: {
  title: string;
  intro: ReactNode;
  /** An app authorization waits on this step. */
  resuming?: boolean;
  children: ReactNode;
}) {
  return (
    <main className="mx-auto grid min-h-dvh max-w-md content-center gap-4 p-4">
      <div className="grid gap-1">
        <div className="mb-4 flex items-center gap-2">
          <BrandMark className="size-8" />
          <span className="text-lg font-semibold text-fg">The Runbook</span>
        </div>
        <h1 className="text-xl font-semibold">{title}</h1>
        <div className="text-fg-muted">{intro}</div>
      </div>
      {resuming ? (
        <p role="status" className="rounded-md border border-border bg-surface px-3 py-2 text-sm">
          An app is waiting for you to finish authorizing it. You go back to it once this step is
          done.
        </p>
      ) : null}
      {children}
    </main>
  );
}

/**
 * The step after the first sign-in factor (J-3): verify a second factor, or,
 * when the organization requires one and the person has none, set one up.
 * Neither can be skipped while the session is partial; signing out is the
 * only other way out. An enrolled person whose organization does not require
 * MFA may continue without it (sensitive actions will ask later).
 */
export function MfaGate({ security, onDone, onSignOut, returnTo, signingOut }: MfaGateProps) {
  const refresh = useRefreshSecurity();
  const options = useFactorOptions(security);
  const resume = safeReturnTo(returnTo);
  const required = security.mfa_partial;

  async function finish() {
    await refresh();
    if (resume) resumeAt(resume);
    else onDone();
  }

  const signOut = (
    <p className="flex flex-wrap items-center gap-x-4 gap-y-2 pt-2 text-sm">
      {!required ? (
        <button
          type="button"
          className="text-accent underline-offset-4 hover:underline"
          onClick={() => void finish()}
        >
          Not now
        </button>
      ) : null}
      <button
        type="button"
        disabled={signingOut}
        className="text-fg-muted underline-offset-4 hover:text-fg hover:underline"
        onClick={onSignOut}
      >
        {signingOut ? 'Signing out…' : 'Sign out'}
      </button>
    </p>
  );

  if (security.mfa_enrolled) {
    return (
      <GateFrame
        resuming={Boolean(resume)}
        title="Confirm it’s you"
        intro={
          required
            ? 'Your organization requires a second factor. Use your passkey or authenticator app to finish signing in.'
            : 'Use your passkey or authenticator app to finish signing in. If you skip this, sensitive actions will ask for it.'
        }
      >
        <div className="rounded-md border border-border bg-surface p-4">
          <SecondFactorForm options={options} onVerified={finish} focusCode />
        </div>
        {signOut}
      </GateFrame>
    );
  }

  // Required MFA starts with an allowed passkey (design §8): an app setup
  // started now would need a check this account cannot give yet. The app
  // can be added from Account › Security once the passkey exists.
  const supported = passkeysSupported();
  const allowApp = security.allow_authenticator;

  return (
    <GateFrame
      resuming={Boolean(resume)}
      title="Set up a second factor"
      intro="Your organization requires a second factor to sign in. Set one up now to continue; you can add more later in Account › Security."
    >
      {!security.allow_passkeys ? (
        <div
          role="alert"
          className="grid gap-2 rounded-md border border-border bg-surface p-4 text-sm"
        >
          <p className="font-semibold text-fg">You cannot set up a factor yourself yet</p>
          <p className="text-fg-muted">
            Your organization allows only authenticator apps, and an authenticator app cannot be the
            first factor on an account that must use one. Ask an organization owner to allow
            passkeys (at least while you set up), then sign in again. If you belong to more than one
            organization, ask the operator who runs this installation.
          </p>
        </div>
      ) : !supported ? (
        <p role="alert" className="rounded-md border border-border bg-surface p-4 text-sm">
          Your first second factor must be a passkey, and this browser cannot create one. Open The
          Runbook in a browser with passkey support, on https with a domain name or on localhost.
        </p>
      ) : (
        <section
          aria-label="Create a passkey"
          className="grid gap-3 rounded-md border border-border bg-surface p-4"
        >
          <div className="flex items-start gap-3">
            <KeyRound aria-hidden="true" className="mt-0.5 size-5 text-fg-muted" />
            <p className="text-sm text-fg-muted">
              Start with a passkey: your device’s fingerprint, face or PIN, or a security key.
              {allowApp ? (
                <>
                  {' '}
                  <Smartphone aria-hidden="true" className="inline size-4" /> You can add an
                  authenticator app afterwards in Account › Security.
                </>
              ) : null}
            </p>
          </div>
          <AddPasskeyForm onAdded={finish} />
        </section>
      )}
      {signOut}
    </GateFrame>
  );
}
