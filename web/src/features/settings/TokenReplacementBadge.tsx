import { ShieldAlert } from 'lucide-react';
import { useId } from 'react';
import { Button } from '../../ui/primitives/button';

/**
 * A token issued without a fresh second-factor check (`fresh_mfa_at_issue`
 * false or missing on an older server) while the organization requires MFA
 * for it: ordinary scoped calls work, sensitive ones answer
 * `auth.mfa_token_replacement_required`. A later step-up does not change a
 * token's issuance facts, so the fix is a replacement.
 */
export function TokenReplacementBadge({
  guidance,
  onReplace,
}: {
  guidance: string;
  onReplace?: () => void;
}) {
  const id = useId();
  return (
    <div className="grid gap-1.5 pt-1">
      <span
        className="inline-flex w-fit items-center gap-1 rounded border border-border bg-bg px-1.5 py-0.5 text-2xs font-medium text-fg"
        aria-describedby={id}
      >
        <ShieldAlert aria-hidden="true" className="size-3" />
        Created without a second-factor check
      </span>
      <p id={id} className="max-w-prose text-xs text-fg-muted">
        Your organization requires a second factor, so sensitive actions with this token are
        refused. {guidance}
      </p>
      {onReplace ? (
        <Button type="button" size="sm" variant="secondary" className="w-fit" onClick={onReplace}>
          Replace token
        </Button>
      ) : null}
    </div>
  );
}
