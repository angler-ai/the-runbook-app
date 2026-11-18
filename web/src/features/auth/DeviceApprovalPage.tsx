import { useSearch } from '@tanstack/react-router';
import { useEffect, useState, type FormEvent } from 'react';
import { api, ApiError } from '../../api/client';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';

export interface DeviceLookupData {
  user_code: string;
  client_name: string;
  client_name_verified: boolean;
  requester_ip?: string | null;
  requested_scopes: string[];
  created_at_ms: number;
  expires_at_ms: number;
}

export interface DeviceApprovalPageProps {
  initialCode?: string;
}

export function DeviceApprovalPage({ initialCode: propCode }: DeviceApprovalPageProps = {}) {
  let searchCode = '';
  try {
    const search = useSearch({ strict: false }) as { user_code?: string; code?: string };
    searchCode = (search?.user_code ?? search?.code ?? '').trim().toUpperCase();
  } catch {
    // When rendered outside a RouterProvider (e.g. standalone component test)
  }
  const initialCode = (propCode ?? searchCode).trim().toUpperCase();


  const [code, setCode] = useState(initialCode);
  const [lookup, setLookup] = useState<DeviceLookupData | null>(null);
  const [busy, setBusy] = useState(Boolean(initialCode));
  const [error, setError] = useState<string | null>(null);
  const [decision, setDecision] = useState<'approved' | 'denied' | null>(null);

  async function performLookup(targetCode: string) {
    const clean = targetCode.trim().toUpperCase();
    if (!clean) return;
    setBusy(true);
    setError(null);
    setLookup(null);
    setDecision(null);
    try {
      const { data } = await api.POST('/v1/device/lookup', {
        params: {
          header: {
            'X-Runbook-CSRF': '1',
          },
        },
        body: { user_code: clean },
      });
      setLookup(data as unknown as DeviceLookupData);
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 400)) {
        setError('That confirmation code was not found or has expired. Check your terminal and try again.');
      } else {
        setError('Failed to look up confirmation code. Please try again.');
      }
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    if (!initialCode) return;
    let active = true;
    void api
      .POST('/v1/device/lookup', {
        params: {
          header: {
            'X-Runbook-CSRF': '1',
          },
        },
        body: { user_code: initialCode },
      })
      .then(({ data }) => {
        if (active) setLookup(data as unknown as DeviceLookupData);
      })
      .catch((err) => {
        if (active) {
          if (err instanceof ApiError && (err.status === 404 || err.status === 400)) {
            setError(
              'That confirmation code was not found or has expired. Check your terminal and try again.',
            );
          } else {
            setError('Failed to look up confirmation code. Please try again.');
          }
        }
      })
      .finally(() => {
        if (active) setBusy(false);
      });
    return () => {
      active = false;
    };
  }, [initialCode]);


  async function handleLookupSubmit(e: FormEvent) {
    e.preventDefault();
    void performLookup(code);
  }

  async function handleDecision(approve: boolean) {
    if (!lookup) return;
    setBusy(true);
    setError(null);
    try {
      await api.POST('/v1/device/approve', {
        params: {
          header: {
            'Idempotency-Key': crypto.randomUUID(),
            'X-Runbook-CSRF': '1',
          },
        },
        body: {
          user_code: lookup.user_code,
          approve,
          expires_at_ms: null,
          projects: null,
          scopes: null,
        },
      });
      setDecision(approve ? 'approved' : 'denied');
    } catch (err) {
      if (err instanceof ApiError) {
        setError(`The server refused that action (${err.code}).`);
      } else {
        setError('Failed to process approval decision.');
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="mx-auto grid min-h-dvh max-w-lg content-center gap-6 p-4">
      <div className="grid gap-1">
        <h1 className="text-xl font-semibold">Device approval</h1>
        <p className="text-fg-muted">
          Authorize a CLI or background tool to access The Runbook on your behalf.
        </p>
      </div>

      {decision === 'approved' ? (
        <div role="status" className="grid gap-3 rounded-md border border-border bg-surface p-4">
          <h2 className="text-base font-semibold text-fg">Device approved</h2>
          <p className="text-sm text-fg-muted">
            You have granted access to this device. You may close this tab and return to your terminal.
          </p>
        </div>
      ) : decision === 'denied' ? (
        <div role="status" className="grid gap-3 rounded-md border border-border bg-surface p-4">
          <h2 className="text-base font-semibold text-fg">Request denied</h2>
          <p className="text-sm text-fg-muted">
            The login request was denied. No access token was issued.
          </p>
          <Button
            type="button"
            variant="secondary"
            onClick={() => {
              setLookup(null);
              setDecision(null);
              setCode('');
            }}
          >
            Approve a different device
          </Button>
        </div>
      ) : lookup ? (
        <div className="grid gap-4 rounded-md border border-border bg-surface p-4">
          <div className="grid gap-1">
            <span className="font-mono text-xs uppercase text-fg-muted">Confirmation code</span>
            <span className="font-mono text-lg font-bold tracking-wider text-fg">{lookup.user_code}</span>
          </div>

          <div className="grid gap-1 border-t border-border pt-3">
            <span className="text-xs text-fg-muted">Client</span>
            <p className="text-sm font-medium text-fg">
              {lookup.client_name || 'CLI Client'}{' '}
              <span className="text-xs font-normal text-fg-muted">(claimed by client)</span>
            </p>
            {lookup.requester_ip ? (
              <p className="text-xs text-fg-muted">Request origin IP: {lookup.requester_ip}</p>
            ) : null}
          </div>

          <div className="grid gap-1.5 border-t border-border pt-3">
            <span className="text-xs text-fg-muted">Requested permissions</span>
            <div className="flex flex-wrap gap-1.5" aria-label="Requested permissions">
              {lookup.requested_scopes.map((scope) => (
                <span
                  key={scope}
                  className="rounded-md border border-border bg-bg px-2 py-0.5 font-mono text-xs text-fg"
                >
                  {scope}
                </span>
              ))}
            </div>
          </div>

          {error ? (
            <p role="alert" className="text-sm text-fg">
              {error}
            </p>
          ) : null}

          <div className="flex items-center gap-3 border-t border-border pt-3">
            <Button
              type="button"
              disabled={busy}
              onClick={() => void handleDecision(true)}
              className="flex-1"
            >
              {busy ? 'Approving…' : 'Approve device'}
            </Button>
            <Button
              type="button"
              variant="secondary"
              disabled={busy}
              onClick={() => void handleDecision(false)}
            >
              Deny
            </Button>
          </div>
        </div>
      ) : (
        <form className="grid gap-4 rounded-md border border-border bg-surface p-4" onSubmit={(e) => void handleLookupSubmit(e)}>
          <div className="grid gap-2">
            <Label htmlFor="device-user-code">Confirmation code</Label>
            <Input
              id="device-user-code"
              type="text"
              autoComplete="off"
              spellCheck={false}
              autoCapitalize="characters"
              placeholder="e.g. WDJB-MJHT"
              className="font-mono tracking-widest uppercase"
              value={code}
              onChange={(e) => setCode(e.target.value.toUpperCase())}
              disabled={busy}
            />
            <p className="text-xs text-fg-muted">
              Enter the 8-character confirmation code displayed in your terminal.
            </p>
          </div>

          {error ? (
            <p role="alert" className="text-sm text-fg">
              {error}
            </p>
          ) : null}

          <Button type="submit" disabled={busy || code.trim().length === 0}>
            {busy ? 'Looking up…' : 'Look up device'}
          </Button>
        </form>
      )}
    </main>
  );
}
