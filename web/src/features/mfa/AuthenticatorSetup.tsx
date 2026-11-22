import { useState, type FormEvent } from 'react';
import { Button } from '../../ui/primitives/button';
import {
  AuthApiError,
  codeLooksValid,
  enableTotp,
  mfaErrorMessage,
  normalizeCode,
  verifyTotp,
  type TotpSetup,
} from './authApi';
import { BackupCodes } from './BackupCodes';
import { CodeField } from './CodeField';
import { QrCode } from './QrCode';

/** The base32 secret from an otpauth URI, grouped in fours for typing. */
export function manualKey(uri: string): string {
  try {
    const secret = new URL(uri).searchParams.get('secret') ?? '';
    return secret.replace(/(.{4})/g, '$1 ').trim();
  } catch {
    return '';
  }
}

export interface AuthenticatorSetupProps {
  /** Called once the code is confirmed and the backup codes were shown. */
  onDone(): void;
  /**
   * Called right after the code is confirmed. The app is enrolled, but the
   * session is not upgraded: the next sensitive action asks for a new code
   * or another allowed factor.
   */
  onEnrolled?(): void | Promise<void>;
  onCancel?(): void;
}

/** A setup lasts ten minutes and belongs to the browser session that started it. */
export const SETUP_LIFETIME_MS = 10 * 60_000;

/**
 * The confirmation found no active setup: it expired, another browser
 * session replaced it, or a passkey was added meanwhile.
 */
function setupEnded(error: unknown): boolean {
  return (
    error instanceof AuthApiError &&
    (error.code === 'auth.mfa_setup_changed' ||
      (error.code === 'auth.invalid_code' && error.status === 400))
  );
}

const ENDED =
  'This setup ended: setups last 10 minutes and belong to the browser that started them. Start again to get a new QR code.';

/**
 * Set up an authenticator app: scan the QR code (drawn locally) or type the
 * key, confirm with a code, then save the backup codes shown once.
 */
export function AuthenticatorSetup({ onDone, onEnrolled, onCancel }: AuthenticatorSetupProps) {
  const [setup, setSetup] = useState<TotpSetup | null>(null);
  const [startedAt, setStartedAt] = useState(0);
  const [step, setStep] = useState<'start' | 'scan' | 'codes'>('start');
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  function restart(message: string) {
    setSetup(null);
    setCode('');
    setStep('start');
    setError(message);
  }

  async function start() {
    setBusy(true);
    setError(null);
    try {
      setSetup(await enableTotp());
      setStartedAt(Date.now());
      setStep('scan');
    } catch (err) {
      setError(mfaErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function confirm(event: FormEvent) {
    event.preventDefault();
    const value = normalizeCode(code, 'totp');
    if (!codeLooksValid(value, 'totp')) {
      setError('Enter the 6-digit code your app shows for The Runbook.');
      return;
    }
    if (Date.now() - startedAt > SETUP_LIFETIME_MS) {
      restart(ENDED);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // `{status: true, enrolled: true}`: enrolled, not a step-up.
      await verifyTotp(value);
      setStep('codes');
      await onEnrolled?.();
    } catch (err) {
      if (setupEnded(err)) restart(ENDED);
      else setError(mfaErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  if (step === 'start') {
    return (
      <div className="grid gap-3">
        <p className="text-sm text-fg-muted">
          Use an app such as 1Password, Google Authenticator or Authy. You will scan a QR code, then
          type the code the app shows.
        </p>
        {error ? (
          <p role="alert" className="text-sm text-fg">
            {error}
          </p>
        ) : null}
        <div className="flex flex-wrap gap-2">
          <Button type="button" onClick={() => void start()} disabled={busy}>
            {busy ? 'Starting…' : 'Set up authenticator app'}
          </Button>
          {onCancel ? (
            <Button type="button" variant="secondary" onClick={onCancel} disabled={busy}>
              Cancel
            </Button>
          ) : null}
        </div>
      </div>
    );
  }

  if (step === 'scan' && setup) {
    const key = manualKey(setup.totpURI);
    return (
      <form className="grid gap-4" onSubmit={(event) => void confirm(event)} noValidate>
        <ol className="grid list-decimal gap-4 pl-5 text-sm text-fg">
          <li className="grid gap-2">
            <span>Scan this QR code with your authenticator app.</span>
            <QrCode value={setup.totpURI} label="QR code for your authenticator app" />
          </li>
          <li className="grid gap-1">
            <span>Can’t scan it? Enter this key in the app instead:</span>
            <code
              data-testid="totp-manual-key"
              className="w-fit rounded border border-border bg-bg px-2 py-1 font-mono text-sm break-all text-fg"
            >
              {key}
            </code>
          </li>
          <li className="grid gap-2">
            <span>Type the code the app shows.</span>
            <CodeField
              kind="totp"
              value={code}
              onChange={setCode}
              disabled={busy}
              error={error}
              label="Code from your authenticator app"
            />
          </li>
        </ol>
        <div className="flex flex-wrap gap-2">
          <Button type="submit" disabled={busy || code.trim().length === 0}>
            {busy ? 'Checking…' : 'Confirm code'}
          </Button>
          {onCancel ? (
            <Button type="button" variant="secondary" onClick={onCancel} disabled={busy}>
              Cancel
            </Button>
          ) : null}
        </div>
      </form>
    );
  }

  return (
    <div className="grid gap-4">
      <div role="status" className="grid gap-1 text-sm">
        <p className="text-fg">Your authenticator app is set up.</p>
        <p className="text-fg-muted">
          Setting it up does not count as confirming it is you. The next sensitive action will ask
          for a new code from the app.
        </p>
      </div>
      <BackupCodes codes={setup?.backupCodes ?? []} />
      <Button type="button" onClick={onDone} className="justify-self-start">
        I saved my backup codes
      </Button>
    </div>
  );
}
