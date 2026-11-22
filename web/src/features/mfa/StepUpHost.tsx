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

type Prompt =
  | { kind: 'verify'; request: StepUpRequest }
  | { kind: 'enroll'; request: StepUpRequest }
  | { kind: 'token'; request: StepUpRequest };

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
  const mine = useMySecurity(!fromRefusal);
  const security = fromRefusal?.allow_passkeys !== undefined ? fromRefusal : mine.data;
  const options = useFactorOptions(
    security
      ? {
          allow_passkeys: security.allow_passkeys ?? true,
          allow_authenticator: security.allow_authenticator ?? true,
        }
      : undefined,
  );
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
      if (partialNow) {
        // The session became partial (policy changed): the shell's gate takes over.
        if (!isPartialMe(queryClient.getQueryData(queryKeys.me))) {
          void queryClient.invalidateQueries({ queryKey: queryKeys.me, exact: true });
        }
        // Page loads are not actions: let the gate ask, not a dialog per query.
        if (request.method === 'GET' || request.method === 'HEAD') return false;
      }
      const kind: Prompt['kind'] = !sessionStore.isCookieSession()
        ? 'token'
        : partialNow && securityOf(request)?.mfa_enrolled === false
          ? 'enroll'
          : 'verify';
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
            <DialogTitle>
              {prompt.kind === 'enroll' ? 'Set up a second factor' : 'Confirm it’s you'}
            </DialogTitle>
            <DialogDescription id="step-up-description">
              {prompt.kind === 'token'
                ? 'This action needs a recent second-factor check, which a pasted access token cannot give. Sign in with email, Google or a passkey to do it.'
                : prompt.kind === 'enroll'
                  ? 'Your organization now requires a second factor. Set one up, then try again.'
                  : `This action needs a second-factor check${minutes ? ` from the last ${minutes} minutes` : ''}. When you confirm, it continues where it stopped.`}
            </DialogDescription>
          </DialogHeader>
          {prompt.kind === 'verify' ? (
            <VerifyBody request={prompt.request} onVerified={verified} />
          ) : null}
          <DialogFooter>
            {prompt.kind === 'enroll' ? (
              <Button
                type="button"
                onClick={() => {
                  close(false);
                  void queryClient.invalidateQueries({ queryKey: queryKeys.me, exact: true });
                }}
              >
                Set up now
              </Button>
            ) : null}
            <Button type="button" variant="secondary" onClick={() => close(false)}>
              {prompt.kind === 'verify' ? 'Cancel' : prompt.kind === 'enroll' ? 'Not now' : 'OK'}
            </Button>
          </DialogFooter>
        </DialogContent>
      ) : null}
    </Dialog>
  );
}
