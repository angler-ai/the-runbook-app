import { ChevronRight, KeyRound } from 'lucide-react';
import { QueryClientContext } from '@tanstack/react-query';
import { useContext, useEffect, useState, type FormEvent } from 'react';
import { api } from '../../api/client';
import { mfaErrorMessage } from '../mfa/authApi';
import { passkeysSupported } from '../mfa/webauthn';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import {
  NARROW_TOKEN_SUGGESTION,
  OPTIONAL_SCOPES_NOTE,
  attemptSignIn,
  isSecurePasteContext,
  sendVerificationOtp,
  signInWithPasskey,
  startGoogleSignIn,
  verifyEmailOtp,
  type MeView,
} from './session';

export const SIGN_IN_REASONS: Record<string, string> = {
  'session-expired':
    'Your session ended because the token was revoked or expired. Sign in again to continue.',
  'signed-out':
    'You have signed out. That token was revoked, so paste a different one to sign in again.',
  'revoke-failed':
    'You have signed out here, but the server could not be reached to revoke that token. It may still be valid; revoke it with runbook token revoke if that matters.',
  'mfa-signed-out': 'You have signed out. Sign in again when you have your second factor.',
};

export interface SignInFormProps {
  /** Defaults to the current page origin (F96); tests pass it explicitly. */
  secureContext?: boolean;
  /** Why the router sent the user here, from the `reason` search param. */
  reason?: string | null;
  /** Error from OAuth redirect or auth service. */
  oauthError?: string | null;
  /** Optional override for methods (useful in tests). */
  methods?: string[];
  onSignedIn(me: MeView): void;
}

/**
 * The CLI command for a browser token (walkthrough #13): a code block that
 * wraps between words, never inside one, with a copy button.
 */
export function TokenCommand() {
  const [copied, setCopied] = useState<'idle' | 'copied' | 'failed'>('idle');
  async function copy() {
    try {
      await navigator.clipboard.writeText(NARROW_TOKEN_SUGGESTION);
      setCopied('copied');
    } catch {
      setCopied('failed');
    }
  }
  return (
    <div
      data-testid="token-command"
      className="flex flex-wrap items-start gap-2 rounded-md border border-border bg-surface p-2"
    >
      <pre className="m-0 min-w-0 flex-1 basis-56">
        <code className="font-mono text-xs text-fg whitespace-pre-wrap break-normal">
          {/* Each word unbreakable, so lines break only at spaces (not after
              "--"). Assistive tech reads the plain text instead of the pieces. */}
          <span className="sr-only">{NARROW_TOKEN_SUGGESTION}</span>
          <span aria-hidden="true" data-slot="command-words">
            {NARROW_TOKEN_SUGGESTION.split(' ').map((word, index) => (
              <span key={index}>
                {index > 0 ? ' ' : null}
                <span className="whitespace-nowrap">{word}</span>
              </span>
            ))}
          </span>
        </code>
      </pre>
      <Button type="button" variant="secondary" size="sm" onClick={() => void copy()}>
        {copied === 'copied'
          ? 'Copied'
          : copied === 'failed'
            ? 'Copy failed: select it'
            : 'Copy command'}
      </Button>
    </div>
  );
}

export function SignInForm({
  secureContext,
  reason,
  oauthError,
  methods: methodsProp,
  onSignedIn,
}: SignInFormProps) {
  const secure = secureContext ?? isSecurePasteContext(globalThis.location?.href ?? '');
  const [asyncMethods, setAsyncMethods] = useState<string[]>([]);

  useEffect(() => {
    if (methodsProp) return;
    let active = true;
    void api
      .GET('/.well-known/runbook')
      .then(({ data }) => {
        if (active && data?.auth?.methods) {
          setAsyncMethods(data.auth.methods);
        }
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [methodsProp]);

  const enabledMethods = methodsProp ?? asyncMethods;
  const hasEmail = enabledMethods.includes('email');
  const hasGoogle = enabledMethods.includes('google');
  const otherMethods = hasEmail || hasGoogle;

  // Token state
  const [token, setToken] = useState('');
  const [tokenError, setTokenError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // Email OTP state
  const [emailStep, setEmailStep] = useState<'email' | 'code'>('email');
  const [email, setEmail] = useState('');
  const [otp, setOtp] = useState('');
  const [emailError, setEmailError] = useState<string | null>(null);
  const [resendTimer, setResendTimer] = useState(0);

  const [passkeyError, setPasskeyError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const queryClient = useContext(QueryClientContext);
  const reasonMessage = reason ? (SIGN_IN_REASONS[reason] ?? null) : null;

  // 60-second resend countdown
  useEffect(() => {
    if (resendTimer <= 0) return;
    const interval = setInterval(() => {
      setResendTimer((prev) => (prev > 0 ? prev - 1 : 0));
    }, 1000);
    return () => clearInterval(interval);
  }, [resendTimer]);

  async function handleSendEmailCode(event?: FormEvent) {
    event?.preventDefault();
    if (!email.trim()) return;
    setBusy(true);
    setEmailError(null);
    const result = await sendVerificationOtp(email);
    setBusy(false);
    if (!result.ok) {
      setEmailError(result.message);
      return;
    }
    setEmailStep('code');
    setResendTimer(60);
  }

  async function handleVerifyOtp(event?: FormEvent) {
    event?.preventDefault();
    if (otp.trim().length !== 6) return;
    setBusy(true);
    setEmailError(null);
    const result = await verifyEmailOtp(email, otp);
    setBusy(false);
    if (!result.ok) {
      setEmailError(result.message);
      return;
    }
    queryClient?.clear();
    onSignedIn(result.me);
  }

  async function handleResendCode() {
    if (resendTimer > 0) return;
    setBusy(true);
    setEmailError(null);
    const result = await sendVerificationOtp(email);
    setBusy(false);
    if (!result.ok) {
      setEmailError(result.message);
      return;
    }
    setResendTimer(60);
  }

  async function handleGoogleSignIn() {
    setBusy(true);
    setEmailError(null);
    const err = await startGoogleSignIn('/');
    setBusy(false);
    if (err && !err.ok) {
      setEmailError(err.message);
    }
  }

  async function handlePasskeySignIn() {
    setBusy(true);
    setEmailError(null);
    setPasskeyError(null);
    try {
      const me = await signInWithPasskey();
      queryClient?.clear();
      onSignedIn(me);
    } catch (err) {
      setPasskeyError(mfaErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function submitToken(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setTokenError(null);
    const result = await attemptSignIn(token, { secureContext: secure });
    setBusy(false);
    setToken('');
    if (!result.ok) {
      setTokenError(result.refusal.message);
      return;
    }
    queryClient?.clear();
    if (!result.persisted) {
      setNotice('Signed in for this tab only: this browser would not keep the token for reloads.');
    }
    onSignedIn(result.me);
  }

  const tokenForm = (
    <form className="grid gap-4" onSubmit={(event) => void submitToken(event)} noValidate>
      <div className="grid gap-2">
        <Label htmlFor="runbook-token">Personal access token</Label>
        <Input
          id="runbook-token"
          type="text"
          autoComplete="off"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          data-1p-ignore=""
          data-lpignore="true"
          data-bwignore=""
          data-form-type="other"
          className="font-mono [-webkit-text-security:disc]"
          disabled={!secure || busy}
          value={token}
          onChange={(event) => setToken(event.target.value)}
          placeholder="trb_pat_…"
          aria-invalid={tokenError ? true : undefined}
          aria-describedby={tokenError ? 'runbook-token-error' : 'runbook-token-hint'}
        />
        <div id="runbook-token-hint" className="grid gap-2 text-fg-muted">
          <p>Use a short-lived token made for the browser:</p>
          <TokenCommand />
          <p>
            {OPTIONAL_SCOPES_NOTE} Tokens that can manage tokens, webhooks or the project are
            refused here.
          </p>
        </div>
      </div>
      {tokenError ? (
        <p id="runbook-token-error" role="alert" className="text-base">
          {tokenError}
        </p>
      ) : null}
      {notice ? (
        <p role="status" className="text-base">
          {notice}
        </p>
      ) : null}
      <Button type="submit" disabled={!secure || busy || token.trim().length === 0}>
        {busy ? 'Signing in…' : otherMethods ? 'Sign in with token' : 'Sign in'}
      </Button>
    </form>
  );

  return (
    <div className="grid gap-4">
      {reasonMessage ? (
        <p role="status" className="rounded-md border border-border bg-surface px-3 py-2 text-base">
          {reasonMessage}
        </p>
      ) : null}
      {oauthError ? (
        <p role="alert" className="rounded-md border border-border bg-surface px-3 py-2 text-base">
          Sign-in error: {oauthError}. Try again or sign in with another method.
        </p>
      ) : null}
      {!secure ? (
        <p role="alert" className="rounded-md border border-border bg-surface px-3 py-2 text-base">
          This page is not on https or localhost, so the token field is disabled: pasting a token
          here could expose it in transit. Open the web app over https, or on this machine.
        </p>
      ) : null}

      {/* Email code sign-in flow */}
      {hasEmail ? (
        <div className="grid gap-3 rounded-md border border-border bg-surface p-4">
          <h2 className="text-sm font-semibold text-fg">Email code sign-in</h2>
          {emailStep === 'email' ? (
            <form className="grid gap-3" onSubmit={(e) => void handleSendEmailCode(e)} noValidate>
              <div className="grid gap-1.5">
                <Label htmlFor="sign-in-email">Email address</Label>
                <Input
                  id="sign-in-email"
                  type="email"
                  autoComplete="email"
                  spellCheck={false}
                  value={email}
                  disabled={busy}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="name@example.com"
                  aria-invalid={emailError ? true : undefined}
                />
              </div>
              {emailError ? (
                <p role="alert" className="text-sm text-fg">
                  {emailError}
                </p>
              ) : null}
              <Button type="submit" disabled={busy || email.trim().length === 0}>
                {busy ? 'Sending code…' : 'Continue with email'}
              </Button>
            </form>
          ) : (
            <form className="grid gap-3" onSubmit={(e) => void handleVerifyOtp(e)} noValidate>
              <p className="text-xs text-fg-muted">
                We sent a 6-digit code to <strong className="font-medium text-fg">{email}</strong>.
              </p>
              <div className="grid gap-1.5">
                <Label htmlFor="sign-in-otp">6-digit code</Label>
                <Input
                  id="sign-in-otp"
                  type="text"
                  maxLength={6}
                  pattern="[0-9]*"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  value={otp}
                  disabled={busy}
                  onChange={(e) => setOtp(e.target.value)}
                  placeholder="123456"
                  className="font-mono tracking-widest"
                  aria-invalid={emailError ? true : undefined}
                />
              </div>
              {emailError ? (
                <p role="alert" className="text-sm text-fg">
                  {emailError}
                </p>
              ) : null}
              <Button type="submit" disabled={busy || otp.trim().length !== 6}>
                {busy ? 'Verifying…' : 'Verify code'}
              </Button>
              <div className="flex flex-wrap items-center justify-between gap-2 pt-1 text-xs">
                <button
                  type="button"
                  disabled={busy || resendTimer > 0}
                  onClick={() => void handleResendCode()}
                  className="text-accent underline disabled:text-fg-muted disabled:no-underline"
                >
                  {resendTimer > 0 ? `Resend code in ${resendTimer}s` : 'Resend code'}
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setEmailStep('email');
                    setOtp('');
                    setEmailError(null);
                  }}
                  className="text-fg-muted underline hover:text-fg"
                >
                  Use a different email
                </button>
              </div>
            </form>
          )}
        </div>
      ) : null}

      {/* Google OAuth button */}
      {hasGoogle ? (
        <div className="grid gap-2">
          <Button
            type="button"
            variant="secondary"
            disabled={busy}
            onClick={() => void handleGoogleSignIn()}
            className="w-full"
          >
            Continue with Google
          </Button>
        </div>
      ) : null}

      {/* J-3: a passkey is both factors at once. */}
      {otherMethods && passkeysSupported() ? (
        <div className="grid gap-2">
          <Button
            type="button"
            variant="secondary"
            disabled={busy}
            onClick={() => void handlePasskeySignIn()}
            className="w-full"
          >
            <KeyRound aria-hidden="true" />
            Sign in with a passkey
          </Button>
          {passkeyError ? (
            <p role="alert" className="text-sm text-fg">
              {passkeyError}
            </p>
          ) : null}
        </div>
      ) : null}

      {/* A pasted personal access token: the only way in when no other
          method is configured, else a developers' option kept folded. */}
      {otherMethods ? (
        <details
          className="group rounded-lg border border-border"
          open={tokenError ? true : undefined}
        >
          <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-sm text-fg-muted hover:text-fg [&::-webkit-details-marker]:hidden">
            <ChevronRight
              className="size-4 shrink-0 transition-transform group-open:rotate-90"
              aria-hidden="true"
            />
            Sign in with an access token (developers)
          </summary>
          <div className="border-t border-border p-4">{tokenForm}</div>
        </details>
      ) : (
        tokenForm
      )}
    </div>
  );
}
