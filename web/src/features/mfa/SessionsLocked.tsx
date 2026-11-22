import { useState } from 'react';
import { requestStepUp } from '../../api/step-up';
import { Button } from '../../ui/primitives/button';
import { AuthApiError } from './authApi';

/** The sessions list is refused until this session has verified a factor. */
export function sessionsLocked(error: unknown): boolean {
  return error instanceof AuthApiError && error.code === 'auth.mfa_required';
}

/**
 * Shown instead of the sessions list for an enrolled account whose session
 * has not verified a factor (it chose "Not now" at sign-in): one button
 * opens the second-factor prompt, then the list loads.
 */
export function SessionsLocked({ onVerified }: { onVerified(): void }) {
  const [busy, setBusy] = useState(false);
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border bg-surface p-4 text-sm">
      <p className="text-fg-muted">Confirm it’s you with a second factor to see your sessions.</p>
      <Button
        type="button"
        size="sm"
        variant="secondary"
        disabled={busy}
        onClick={() => {
          setBusy(true);
          void requestStepUp({ code: 'auth.mfa_step_up' }, { method: 'GET' }).then((verified) => {
            setBusy(false);
            if (verified) onVerified();
          });
        }}
      >
        Confirm it’s you
      </Button>
    </div>
  );
}
