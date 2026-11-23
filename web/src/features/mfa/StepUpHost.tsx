import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';
import { queryKeys } from '../../api/query-keys';
import { registerStepUpHandler, type StepUpRequest } from '../../api/step-up';
import { sessionStore } from '../../api/token-store';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { isPartialMe, mfaKeys, useMySecurity, type MfaSecurityView } from './hooks';
import { SecondFactorForm, useFactorOptions } from './SecondFactorForm';

import { AddPasskeyForm } from './AddPasskeyForm';
import { AuthenticatorSetup } from './AuthenticatorSetup';
import { passkeysSupported } from './webauthn';

type Prompt = { kind: 'browser' | 'token'; request: StepUpRequest };

function securityOf(request: StepUpRequest): Partial<MfaSecurityView> | undefined {
  return (request.security as Partial<MfaSecurityView> | null) ?? undefined;
}

function VerifyBody({
  request,
  onVerified,
}: {
  request: StepUpRequest;
  onVerified(): Promise<void>;
}) {
  const fromRefusal = securityOf(request);
  const mine = useMySecurity();
  const security = { ...fromRefusal, ...mine.data };
  const options = useFactorOptions(
    security.allow_passkeys === undefined
      ? undefined
      : {
          allow_passkeys: security.allow_passkeys,
          allow_authenticator: security.allow_authenticator ?? true,
        },
  );
  const [setup, setSetup] = useState<'passkey' | 'authenticator' | 'verify-authenticator'>();

  if (setup === 'passkey')
    return <AddPasskeyForm inline onAdded={onVerified} onCancel={() => setSetup(undefined)} />;
  if (setup === 'authenticator')
    return (
      <AuthenticatorSetup
        inline
        onCancel={() => setSetup(undefined)}
        onDone={() => {
          setSetup('verify-authenticator');
        }}
      />
    );
  if (setup === 'verify-authenticator')
    return (
      <SecondFactorForm
        options={{ passkey: false, authenticator: true, loading: false }}
        onVerified={onVerified}
        focusCode
      />
    );
  if (mine.isLoading && security.mfa_enrolled == null)
    return (
      <p role="status" className="text-sm text-fg-muted">
        Checking available factors…
      </p>
    );

  if (security.mfa_enrolled === false) {
    const passkey = security.allow_passkeys === true && passkeysSupported();
    // The auth service requires an existing proof before starting TOTP when
    // sign-in itself requires MFA. Passkeys can bootstrap that session.
    const authenticator = security.allow_authenticator === true && !security.mfa_required;
    return (
      <div className="grid gap-3">
        <p className="text-sm text-fg-muted">Set up a second factor to continue this action.</p>
        {passkey || authenticator ? (
          <div className="flex flex-wrap gap-2">
            {passkey && (
              <Button type="button" onClick={() => setSetup('passkey')}>
                Set up a passkey
              </Button>
            )}
            {authenticator && (
              <Button
                type="button"
                variant={passkey ? 'secondary' : 'primary'}
                onClick={() => setSetup('authenticator')}
              >
                Set up an authenticator app
              </Button>
            )}
          </div>
        ) : (
          <p role="alert" className="text-sm">
            {security.allow_passkeys
              ? 'Open The Runbook in a browser that supports creating passkeys.'
              : 'Your organization’s policy prevents you from setting up your first factor. Ask an organization owner to allow passkey setup.'}
          </p>
        )}
      </div>
    );
  }
  if (
    options.passkey === false &&
    options.authenticator === false &&
    !options.loading &&
    (security.passkey_count ?? 0) > 0 &&
    security.allow_passkeys &&
    !passkeysSupported()
  ) {
    return (
      <p role="alert" className="text-sm">
        Open The Runbook in a browser that supports your enrolled passkey.
      </p>
    );
  }
  return <SecondFactorForm options={options} onVerified={onVerified} focusCode />;
}

/**
 * The inline second-factor prompt (J-3). Mounted once by the app; the API
 * client and the auth-service helpers wait on it whenever a request is
 * refused with `auth.mfa_step_up` or `auth.mfa_required`, then repeat the
 * request (same idempotency key) once the person has verified.
 */
export function StepUpHost() {
  const queryClient = useQueryClient();
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const resolver = useRef<((verified: boolean) => void) | null>(null);

  useEffect(() => {
    const unregister = registerStepUpHandler(async (request) => {
      const partialNow = request.code === 'auth.mfa_required';
      if (partialNow && (request.method === 'GET' || request.method === 'HEAD')) {
        // Page loads delegate to the sign-in gate. Keep mutation initiators
        // mounted while the dialog settles the original request.
        if (!isPartialMe(queryClient.getQueryData(queryKeys.me))) {
          void queryClient.invalidateQueries({ queryKey: queryKeys.me, exact: true });
        }
        return false;
      }
      const kind: Prompt['kind'] = sessionStore.isCookieSession() ? 'browser' : 'token';
      resolver.current?.(false);
      return new Promise<boolean>((resolve) => {
        resolver.current = resolve;
        setPrompt({ kind, request });
      });
    });
    return () => {
      unregister();
      // Nothing can answer a prompt once the host is gone.
      resolver.current?.(false);
      resolver.current = null;
    };
  }, [queryClient]);

  function close(verified: boolean) {
    resolver.current?.(verified);
    resolver.current = null;
    setPrompt(null);
  }

  async function verified() {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: mfaKeys.security }),
      queryClient.invalidateQueries({ queryKey: ['auth', 'sessions'] }),
    ]);
    close(true);
  }

  const minutes = prompt?.request.freshMinutes;
  return (
    <Dialog open={prompt !== null} onOpenChange={(open) => (open ? undefined : close(false))}>
      {prompt ? (
        <DialogContent aria-describedby="step-up-description">
          <DialogHeader>
            <DialogTitle>Confirm it’s you</DialogTitle>
            <DialogDescription id="step-up-description">
              {prompt.kind === 'token'
                ? 'This action needs a recent second-factor check, which a pasted access token cannot give. Sign in with email, Google or a passkey to do it.'
                : `This action needs a second-factor check${minutes ? ` from the last ${minutes} minutes` : ''}. When you confirm, it continues where it stopped.`}
            </DialogDescription>
          </DialogHeader>
          {prompt.kind === 'browser' && (
            <VerifyBody request={prompt.request} onVerified={verified} />
          )}
          <DialogFooter>
            <Button type="button" variant="secondary" onClick={() => close(false)}>
              {prompt.kind === 'token' ? 'OK' : 'Cancel'}
            </Button>
          </DialogFooter>
        </DialogContent>
      ) : null}
    </Dialog>
  );
}
