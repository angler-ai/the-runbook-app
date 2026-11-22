import { Link } from '@tanstack/react-router';
import { useId, useRef, useState, type FormEvent } from 'react';
import { ApiError } from '../../api/client';
import { Button } from '../../ui/primitives/button';
import { Checkbox } from '../../ui/primitives/checkbox';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import {
  useMySecurity,
  useOrgSecurity,
  usePreviewOrgSecurity,
  useUpdateOrgSecurity,
  type OrgSecurityPolicy,
  type PolicyPreview,
  type RequireMfa,
} from './hooks';
import { securityCodeMessage } from './messages';

const REQUIRE_OPTIONS: { value: RequireMfa; label: string; description: string }[] = [
  { value: 'off', label: 'Off', description: 'Members choose whether to use a second factor.' },
  {
    value: 'admins',
    label: 'Owners and admins',
    description: 'Owners and admins must use a second factor to sign in.',
  },
  { value: 'everyone', label: 'Everyone', description: 'Every member must use a second factor.' },
];

export function orgSecurityError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.code === 'auth.mfa_step_up')
      return 'Confirm it is you with a second factor, then save again.';
    if (error.code === 'request.invalid')
      return 'Allow at least one kind of factor, and pick a window between 5 and 60 minutes.';
    if (error.code === 'directory.concurrent_change')
      return 'Someone else changed these settings. Reload and try again.';
    if (error.code === 'auth.forbidden')
      return 'Only organization owners can change security settings.';
    return securityCodeMessage(error.code) ?? `The server refused that change (${error.code}).`;
  }
  return 'The change could not be saved. Try again.';
}

function PolicyForm({ orgId, initial }: { orgId: string; initial: OrgSecurityPolicy }) {
  const id = useId();
  const mine = useMySecurity();
  const update = useUpdateOrgSecurity(orgId);
  const previewPolicy = usePreviewOrgSecurity(orgId);
  const [policy, setPolicyState] = useState<OrgSecurityPolicy>(initial);
  // The reviewed preview of exactly what is about to be saved, if it stops anyone.
  const [review, setReview] = useState<PolicyPreview | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const revision = useRef(0);
  const proposalChanged = () => {
    revision.current += 1;
    setReview(null);
    setAcknowledged(false);
  };
  const setPolicy = (next: OrgSecurityPolicy) => {
    setPolicyState(next);
    proposalChanged();
  };
  const [minutesText, setMinutesText] = useState(String(initial.fresh_minutes));
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const minutes = Number(minutesText);
  const minutesValid = Number.isInteger(minutes) && minutes >= 5 && minutes <= 60;
  const noFactor = !policy.allow_passkeys && !policy.allow_authenticator;
  const turningOn = policy.require_mfa !== 'off';
  const ownerNotEnrolled = turningOn && mine.data?.mfa_enrolled === false;
  const changed =
    policy.require_mfa !== initial.require_mfa ||
    policy.allow_passkeys !== initial.allow_passkeys ||
    policy.allow_authenticator !== initial.allow_authenticator ||
    minutes !== initial.fresh_minutes;

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSaved(false);
    if (noFactor || !minutesValid || ownerNotEnrolled) return;
    setError(null);
    const proposed = { ...policy, fresh_minutes: minutes };
    const reviewing = revision.current;
    try {
      if (!review) {
        // Ask first who this would stop; save straight away only if nobody.
        const found = await previewPolicy.mutateAsync(proposed);
        if (revision.current !== reviewing) return;
        if (found.affected.length > 0) {
          setReview(found);
          return;
        }
      } else if (review.affected.length > 0 && !acknowledged) {
        return;
      }
      await update.mutateAsync(proposed);
      setReview(null);
      setAcknowledged(false);
      setSaved(true);
    } catch (err) {
      if (revision.current === reviewing) setError(orgSecurityError(err));
    }
  }

  return (
    <form className="grid gap-6" onSubmit={(event) => void submit(event)} noValidate>
      <fieldset className="grid gap-2">
        <legend className="mb-1 text-sm font-semibold text-fg">Require a second factor</legend>
        {REQUIRE_OPTIONS.map((option) => (
          <div
            key={option.value}
            className="flex items-start gap-3 rounded-md border border-border p-3 has-[:checked]:border-accent"
          >
            <input
              type="radio"
              id={`${id}-require-${option.value}`}
              name={`${id}-require`}
              value={option.value}
              checked={policy.require_mfa === option.value}
              onChange={() => setPolicy({ ...policy, require_mfa: option.value })}
              aria-describedby={`${id}-require-${option.value}-description`}
              className="mt-1 size-4 accent-[var(--color-accent)]"
            />
            <div className="grid gap-0.5">
              <label
                htmlFor={`${id}-require-${option.value}`}
                className="cursor-pointer text-sm font-medium text-fg"
              >
                {option.label}
              </label>
              <span
                id={`${id}-require-${option.value}-description`}
                className="text-xs text-fg-muted"
              >
                {option.description}
              </span>
            </div>
          </div>
        ))}
        <p id={`${id}-require-hint`} className="text-xs text-fg-muted">
          Members who must use a second factor and have none are asked to set one up the next time
          they sign in; until then they can reach nothing else. To turn this on you must have set up
          a second factor yourself.
        </p>
        {ownerNotEnrolled ? (
          <p
            role="alert"
            className="rounded-md border border-border bg-bg px-3 py-2 text-sm text-fg"
          >
            Set up a passkey or an authenticator app for your own account first, in{' '}
            <Link
              to="/settings/security"
              className="text-accent underline-offset-4 hover:underline"
            >
              Account › Security
            </Link>
            .
          </p>
        ) : null}
      </fieldset>

      <fieldset
        className="grid gap-3"
        aria-describedby={noFactor ? `${id}-factor-error` : undefined}
      >
        <legend className="mb-1 text-sm font-semibold text-fg">Allowed second factors</legend>
        <div className="flex items-center gap-3">
          <Checkbox
            id={`${id}-passkeys`}
            checked={policy.allow_passkeys}
            onCheckedChange={(checked) =>
              setPolicy({ ...policy, allow_passkeys: checked === true })
            }
          />
          <Label htmlFor={`${id}-passkeys`} className="text-sm">
            Passkeys
          </Label>
        </div>
        <div className="flex items-center gap-3">
          <Checkbox
            id={`${id}-authenticator`}
            checked={policy.allow_authenticator}
            onCheckedChange={(checked) =>
              setPolicy({ ...policy, allow_authenticator: checked === true })
            }
          />
          <Label htmlFor={`${id}-authenticator`} className="text-sm">
            Authenticator apps (with backup codes)
          </Label>
        </div>
        {noFactor ? (
          <p id={`${id}-factor-error`} role="alert" className="text-sm text-fg">
            Allow at least one kind of factor.
          </p>
        ) : null}
      </fieldset>

      <div className="grid max-w-xs gap-1.5">
        <Label htmlFor={`${id}-minutes`}>Fresh check window (minutes)</Label>
        <Input
          id={`${id}-minutes`}
          type="number"
          inputMode="numeric"
          min={5}
          max={60}
          step={1}
          value={minutesText}
          onChange={(event) => {
            setMinutesText(event.target.value);
            proposalChanged();
          }}
          aria-invalid={!minutesValid ? true : undefined}
          aria-describedby={`${id}-minutes-hint`}
        />
        <p
          id={`${id}-minutes-hint`}
          className={minutesValid ? 'text-xs text-fg-muted' : 'text-xs text-fg'}
          role={minutesValid ? undefined : 'alert'}
        >
          {minutesValid
            ? 'How recently someone must have used a second factor before sensitive actions: credentials, tokens, members and these settings. 5 to 60.'
            : 'Enter a whole number of minutes from 5 to 60.'}
        </p>
      </div>

      {review ? (
        <PolicyReview
          id={id}
          review={review}
          acknowledged={acknowledged}
          onAcknowledge={setAcknowledged}
        />
      ) : null}

      {error ? (
        <p role="alert" className="text-sm text-fg">
          {error}
        </p>
      ) : null}
      <p role="status" className="text-sm text-fg">
        {saved ? 'Security settings saved.' : ''}
      </p>
      <div className="flex flex-wrap gap-2">
        <Button
          type="submit"
          disabled={
            !changed ||
            update.isPending ||
            previewPolicy.isPending ||
            noFactor ||
            !minutesValid ||
            ownerNotEnrolled ||
            (review !== null && review.affected.length > 0 && !acknowledged)
          }
        >
          {update.isPending
            ? 'Saving…'
            : previewPolicy.isPending
              ? 'Checking members…'
              : review && review.affected.length > 0
                ? `Save and lock out ${review.affected.length} ${review.affected.length === 1 ? 'member' : 'members'}`
                : 'Save security settings'}
        </Button>
        {review ? (
          <Button
            type="button"
            variant="secondary"
            onClick={() => {
              setPolicy(initial);
              setMinutesText(String(initial.fresh_minutes));
            }}
          >
            Discard changes
          </Button>
        ) : null}
      </div>
    </form>
  );
}

const REASONS: Record<PolicyPreview['affected'][number]['reason'], string> = {
  factor_not_allowed: 'Has only factors this policy disallows',
  cannot_enroll: 'Has no factor and cannot set up an authenticator app first',
};

function memberLabel(member: { name: string; email: string | null }): string {
  return member.email ? `${member.name} (${member.email})` : member.name;
}

/** Who the proposed policy stops, from the dry run, before it is saved. */
function PolicyReview({
  id,
  review,
  acknowledged,
  onAcknowledge,
}: {
  id: string;
  review: PolicyPreview;
  acknowledged: boolean;
  onAcknowledge(value: boolean): void;
}) {
  const count = review.affected.length;
  return (
    <section
      aria-labelledby={`${id}-review-title`}
      className="grid gap-3 rounded-md border border-border bg-bg p-4 text-sm"
    >
      {count > 0 ? (
        <>
          <h3 id={`${id}-review-title`} className="font-semibold text-fg">
            {count === 1
              ? '1 member could not sign in under this policy'
              : `${count} members could not sign in under this policy`}
          </h3>
          <p className="text-fg-muted">
            They have no second factor this policy allows and cannot set one up themselves. After
            saving, reset their second factors in Members (or an operator does, for someone in
            several organizations) so they can enroll an allowed one.
          </p>
          <ul aria-label="Members locked out" className="grid gap-1.5">
            {review.affected.map((member) => (
              <li key={member.user_id} className="grid gap-0.5 rounded border border-border p-2">
                <span className="font-medium text-fg">{memberLabel(member)}</span>
                <span className="text-xs text-fg-muted">
                  {REASONS[member.reason]}
                </span>
              </li>
            ))}
          </ul>
        </>
      ) : (
        <h3 id={`${id}-review-title`} className="font-semibold text-fg">
          Nobody is locked out
        </h3>
      )}
      {count > 0 ? (
        <div className="flex items-start gap-3">
          <Checkbox
            id={`${id}-acknowledge`}
            checked={acknowledged}
            onCheckedChange={(checked) => onAcknowledge(checked === true)}
          />
          <Label htmlFor={`${id}-acknowledge`} className="text-sm">
            I understand these members cannot sign in until their factors are reset.
          </Label>
        </div>
      ) : null}
    </section>
  );
}

/** Settings › Organization › Security (owners only, J-3). */
export function OrgSecurityTab({
  orgId,
  isOwner,
}: {
  orgId: string | undefined;
  isOwner: boolean;
}) {
  const query = useOrgSecurity(orgId, isOwner);
  if (!isOwner) {
    return (
      <div role="status" className="rounded-md border border-border bg-surface p-6 text-sm">
        <h2 className="font-semibold text-fg">Only owners change security settings</h2>
        <p className="mt-1 text-fg-muted">Ask an organization owner to change the MFA policy.</p>
      </div>
    );
  }
  return (
    <section aria-labelledby="org-security-title" className="grid gap-4">
      <div>
        <h2 id="org-security-title" className="text-sm font-semibold text-fg">
          Multi-factor sign-in
        </h2>
        <p className="text-xs text-fg-muted">
          Who must use a second factor, which factors count, and how fresh a check sensitive actions
          need. Every change is audited.
        </p>
      </div>
      {query.isLoading || !orgId ? (
        <p className="text-sm text-fg-muted">Loading security settings…</p>
      ) : query.isError || !query.data ? (
        <p role="alert" className="text-sm text-fg">
          Could not load the security settings.
        </p>
      ) : (
        <div className="rounded-lg border border-border bg-surface p-4">
          <PolicyForm key={orgId} orgId={orgId} initial={query.data} />
        </div>
      )}
    </section>
  );
}
