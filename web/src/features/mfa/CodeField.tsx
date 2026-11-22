import { useEffect, useId, useRef } from 'react';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';

export interface CodeFieldProps {
  kind: 'totp' | 'backup';
  value: string;
  onChange(value: string): void;
  disabled?: boolean;
  error?: string | null;
  /** Move focus here when the field appears (the step's only input). */
  focusOnMount?: boolean;
  label?: string;
}

/**
 * One labelled field for a verification code (never six boxes), so password
 * managers and SMS/OS autofill work and screen readers announce one input.
 * Errors are announced through role="alert" and linked by aria-describedby.
 */
export function CodeField({
  kind,
  value,
  onChange,
  disabled,
  error,
  focusOnMount,
  label,
}: CodeFieldProps) {
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (focusOnMount) input.current?.focus();
  }, [focusOnMount]);
  const hintId = `${id}-hint`;
  const errorId = `${id}-error`;
  return (
    <div className="grid gap-1.5">
      <Label htmlFor={id}>
        {label ?? (kind === 'totp' ? 'Code from your authenticator app' : 'Backup code')}
      </Label>
      <Input
        id={id}
        name={kind === 'totp' ? 'totp' : 'backup-code'}
        type="text"
        autoComplete="one-time-code"
        inputMode={kind === 'totp' ? 'numeric' : 'text'}
        pattern={kind === 'totp' ? '[0-9]*' : undefined}
        maxLength={kind === 'totp' ? 6 : 11}
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
        ref={input}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        placeholder={kind === 'totp' ? '123456' : 'abcde-12345'}
        className="font-mono tracking-widest"
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? `${hintId} ${errorId}` : hintId}
      />
      <p id={hintId} className="text-xs text-fg-muted">
        {kind === 'totp'
          ? 'Six digits. The code changes every 30 seconds.'
          : 'One of the ten codes you saved when you set up your authenticator. Each works once.'}
      </p>
      {error ? (
        <p id={errorId} role="alert" className="text-sm text-fg">
          {error}
        </p>
      ) : null}
    </div>
  );
}
