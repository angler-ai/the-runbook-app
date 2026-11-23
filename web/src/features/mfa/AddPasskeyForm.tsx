import { useId, useState, type FormEvent } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { mfaErrorMessage } from './authApi';
import { useAddPasskey } from './hooks';
import { passkeysSupported } from './webauthn';

/** A default name people can keep: the browser and OS, roughly. */
export function suggestPasskeyName(userAgent = globalThis.navigator?.userAgent ?? ''): string {
  const os = /iPhone|iPad/.test(userAgent)
    ? 'iPhone'
    : /Mac OS X/.test(userAgent)
      ? 'Mac'
      : /Android/.test(userAgent)
        ? 'Android'
        : /Windows/.test(userAgent)
          ? 'Windows'
          : /Linux/.test(userAgent)
            ? 'Linux'
            : '';
  const browser = /Edg\//.test(userAgent)
    ? 'Edge'
    : /Firefox\//.test(userAgent)
      ? 'Firefox'
      : /Chrome\//.test(userAgent)
        ? 'Chrome'
        : /Safari\//.test(userAgent)
          ? 'Safari'
          : '';
  return [browser, os].filter(Boolean).join(' on ') || 'Passkey';
}

export function AddPasskeyForm({
  onAdded,
  onCancel,
  inline = false,
}: {
  onAdded(): void | Promise<void>;
  onCancel?(): void;
  inline?: boolean;
}) {
  const id = useId();
  const [name, setName] = useState(() => suggestPasskeyName());
  const [error, setError] = useState<string | null>(null);
  const add = useAddPasskey(!inline);

  if (!passkeysSupported()) {
    return (
      <p role="alert" className="text-sm text-fg">
        This browser cannot create passkeys. Use an authenticator app, or a browser with passkey
        support.
      </p>
    );
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setError(null);
    try {
      await add.mutateAsync(name);
      await onAdded();
    } catch (err) {
      setError(mfaErrorMessage(err));
    }
  }

  return (
    <form className="grid gap-3" onSubmit={(event) => void submit(event)} noValidate>
      <div className="grid gap-1.5">
        <Label htmlFor={id}>Passkey name</Label>
        <Input
          id={id}
          value={name}
          maxLength={64}
          onChange={(event) => setName(event.target.value)}
          disabled={add.isPending}
          aria-describedby={`${id}-hint${error ? ` ${id}-error` : ''}`}
          aria-invalid={error ? true : undefined}
        />
        <p id={`${id}-hint`} className="text-xs text-fg-muted">
          So you can tell your passkeys apart, for example “Work laptop”.
        </p>
      </div>
      {error ? (
        <p id={`${id}-error`} role="alert" className="text-sm text-fg">
          {error}
        </p>
      ) : null}
      <div className="flex flex-wrap gap-2">
        <Button type="submit" disabled={add.isPending}>
          {add.isPending ? 'Waiting for your device…' : 'Create passkey'}
        </Button>
        {onCancel ? (
          <Button type="button" variant="secondary" onClick={onCancel} disabled={add.isPending}>
            Cancel
          </Button>
        ) : null}
      </div>
    </form>
  );
}
