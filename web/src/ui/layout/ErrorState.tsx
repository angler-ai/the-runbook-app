import { AlertCircle } from 'lucide-react';
import { Button } from '../primitives/button';
import { cn } from '../primitives/cn';
import { MonoRef } from '../status/MonoRef';

export interface ErrorStateProps {
  title?: string;
  message?: string;
  code?: string;
  requestId?: string;
  onRetry?: () => void;
  className?: string;
}

export function ErrorState({
  title = 'Something went wrong',
  message,
  code,
  requestId,
  onRetry,
  className,
}: ErrorStateProps) {
  return (
    <div
      role="alert"
      data-slot="error-state"
      className={cn(
        'flex flex-col items-center justify-center text-center p-6 sm:p-8 border border-border rounded-lg bg-surface max-w-md mx-auto',
        className,
      )}
    >
      <div className="flex size-10 items-center justify-center rounded-full bg-status-failed-bg text-status-failed-fg mb-3">
        <AlertCircle className="size-5" aria-hidden="true" />
      </div>

      <h3 className="text-sm font-semibold text-fg">{title}</h3>

      {message && <p className="mt-1 text-xs text-fg-muted max-w-sm">{message}</p>}

      {(code || requestId) && (
        <div className="mt-4 pt-3 border-t border-border w-full flex flex-col gap-1.5 text-2xs text-fg-muted items-center">
          {code && (
            <div className="flex items-center gap-1.5">
              <span>Code:</span>
              <MonoRef>{code}</MonoRef>
            </div>
          )}
          {requestId && (
            <div className="flex items-center gap-1.5">
              <span>Request ID:</span>
              <MonoRef>{requestId}</MonoRef>
            </div>
          )}
        </div>
      )}

      {onRetry && (
        <Button variant="secondary" size="sm" onClick={onRetry} className="mt-4">
          Retry
        </Button>
      )}
    </div>
  );
}
