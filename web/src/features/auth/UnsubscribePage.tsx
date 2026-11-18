import { useState } from 'react';
import { apiRuntime } from '../../api/client';

/**
 * The /unsubscribe landing page from a notification email link. Opening
 * the link asks once (mail scanners prefetch links); the button performs
 * the POST that turns the person's mail off.
 */
export function UnsubscribePage({ token }: { token?: string }) {
  const [state, setState] = useState<'idle' | 'working' | 'done' | 'invalid'>('idle');

  async function unsubscribe() {
    if (!token) return;
    setState('working');
    try {
      // The client returns an error rather than throwing it.
      const { error } = await apiRuntime.client.POST('/v1/unsubscribe', {
        params: {
          header: {
            'Idempotency-Key': apiRuntime.idempotencyKeyFor(`unsubscribe:${token}`),
          },
        },
        body: { token },
      });
      setState(error ? 'invalid' : 'done');
    } catch {
      setState('invalid');
    }
  }

  // A link without a token is invalid from the start.
  const shown = token ? state : 'invalid';

  return (
    <main className="mx-auto grid min-h-dvh max-w-md content-center gap-4 p-4">
      <h1 className="text-xl font-semibold">Notification email</h1>
      {shown === 'done' ? (
        <p role="status">
          You are unsubscribed. You will not get notification email from The Runbook. You can turn
          it back on in Settings.
        </p>
      ) : shown === 'invalid' ? (
        <p role="alert">
          This unsubscribe link is not valid. Sign in and change notification email in Settings
          instead.
        </p>
      ) : (
        <>
          <p>
            Stop notification email for the person this link was sent to? You can turn it back on in
            Settings at any time.
          </p>
          <div>
            <button
              type="button"
              className="rounded-md bg-accent px-3 py-2 font-medium text-accent-fg disabled:opacity-50"
              disabled={state === 'working' || !token}
              onClick={() => void unsubscribe()}
            >
              {state === 'working' ? 'Unsubscribing…' : 'Unsubscribe'}
            </button>
          </div>
        </>
      )}
    </main>
  );
}
