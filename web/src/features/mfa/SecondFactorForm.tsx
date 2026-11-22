import { KeyRound } from 'lucide-react';
import { useState, type FormEvent } from 'react';
import { Button } from '../../ui/primitives/button';
import {
  authenticateWithPasskey,
  codeLooksValid,
  mfaErrorMessage,
  normalizeCode,
  verifyBackupCode,
  verifyTotp,
} from './authApi';
import { CodeField } from './CodeField';
import { useMySecurity, type MfaSecurityView } from './hooks';
import { passkeysSupported } from './webauthn';

export interface FactorOptions {
  passkey: boolean;
  authenticator: boolean;
  loading: boolean;
}

/**
 * Which factors this person can verify with now: ones they have enrolled
 * (`authenticator_enrolled` and `passkey_count` from `/v1/me/security`)
 * that the organization allows. While those load, or when the sign-in
 * service cannot say, both are offered.
 */
export function useFactorOptions(
  security: Pick<MfaSecurityView, 'allow_passkeys' | 'allow_authenticator'> | undefined,
): FactorOptions {
  const mine = useMySecurity();
  const facts = mine.data;
  const passkeyCount = facts?.passkey_count;
  const authenticator = facts?.authenticator_enrolled;
  const hasPasskey = typeof passkeyCount === 'number' ? passkeyCount > 0 : true;
  const hasTotp = typeof authenticator === 'boolean' ? authenticator : true;
  return {
    passkey: (security?.allow_passkeys ?? true) && hasPasskey && passkeysSupported(),
    authenticator: (security?.allow_authenticator ?? true) && hasTotp,
    loading: mine.isLoading,
  };
}

export interface SecondFactorFormProps {
  options: FactorOptions;
  onVerified(): void | Promise<void>;
  /** Moves focus to the code field when it is the only way in. */
  focusCode?: boolean;
}

/** Verify with a passkey, an authenticator code, or a backup code. */
export function SecondFactorForm({ options, onVerified, focusCode }: SecondFactorFormProps) {
  const [kind, setKind] = useState<'totp' | 'backup'>('totp');
  const [switched, setSwitched] = useState(false);
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<'passkey' | 'code' | null>(null);

  async function verifyPasskey() {
    setBusy('passkey');
    setError(null);
    try {
      await authenticateWithPasskey();
      await onVerified();
    } catch (err) {
      setError(mfaErrorMessage(err));
    } finally {
      setBusy(null);
    }
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const value = normalizeCode(code, kind);
    if (!codeLooksValid(value, kind)) {
      setError(
        kind === 'totp'
          ? 'Enter the 6-digit code from your authenticator app.'
          : 'Enter a backup code like abcde-12345.',
      );
      return;
    }
    setBusy('code');
    setError(null);
    try {
      await (kind === 'totp' ? verifyTotp(value) : verifyBackupCode(value));
      setCode('');
      await onVerified();
    } catch (err) {
      setError(mfaErrorMessage(err));
    } finally {
      setBusy(null);
    }
  }

  if (!options.passkey && !options.authenticator && !options.loading) {
    return (
      <p role="alert" className="text-sm text-fg">
        None of your factors is allowed by your organization right now. Ask an organization owner to
        reset your second factors, then set up an allowed one. If you belong to more than one
        organization, ask the operator who runs this installation instead.
      </p>
    );
  }

  return (
    <div className="grid gap-4">
      {options.passkey ? (
        <Button
          type="button"
          onClick={() => void verifyPasskey()}
          disabled={busy !== null}
          variant={options.authenticator ? 'secondary' : 'primary'}
        >
          <KeyRound aria-hidden="true" />
          {busy === 'passkey' ? 'Waiting for your passkey…' : 'Use a passkey'}
        </Button>
      ) : null}
      {options.passkey && options.authenticator ? (
        <p className="text-center text-xs text-fg-muted" aria-hidden="true">
          or
        </p>
      ) : null}
      {options.authenticator ? (
        <form className="grid gap-3" onSubmit={(event) => void submit(event)} noValidate>
          <CodeField
            key={kind}
            kind={kind}
            value={code}
            onChange={setCode}
            disabled={busy !== null}
            error={error}
            focusOnMount={switched || (focusCode && !options.passkey)}
          />
          <Button type="submit" disabled={busy !== null || code.trim().length === 0}>
            {busy === 'code' ? 'Checking…' : 'Verify'}
          </Button>
          <button
            type="button"
            className="justify-self-start text-sm text-accent underline-offset-4 hover:underline"
            onClick={() => {
              setKind(kind === 'totp' ? 'backup' : 'totp');
              setSwitched(true);
              setCode('');
              setError(null);
            }}
          >
            {kind === 'totp' ? 'Use a backup code instead' : 'Use your authenticator app instead'}
          </button>
        </form>
      ) : error ? (
        <p role="alert" className="text-sm text-fg">
          {error}
        </p>
      ) : null}
    </div>
  );
}
