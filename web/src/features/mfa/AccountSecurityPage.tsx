import { Link } from '@tanstack/react-router';
import { KeyRound, ShieldCheck, Smartphone } from 'lucide-react';
import { useId, useState, type FormEvent, type ReactNode } from 'react';
import { ConfirmDialog } from '../../ui/actions/ConfirmDialog';
import { PageHeader } from '../../ui/layout/PageHeader';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { formatRelativeTime } from '../../lib/format';
import { isCookieSession } from '../auth/session';
import { useSessions } from '../settings/hooks';
import { AddPasskeyForm } from './AddPasskeyForm';
import { mfaErrorMessage, type PasskeyItem } from './authApi';
import { AuthenticatorSetup } from './AuthenticatorSetup';
import { BackupCodes } from './BackupCodes';
import {
  useDeletePasskey,
  useDisableTotp,
  useMySecurity,
  usePasskeys,
  useRefreshSecurity,
  useRegenerateBackupCodes,
  useRenamePasskey,
} from './hooks';
import { resumeAt, safeReturnTo } from './returnTo';
import { SessionsLocked, sessionsLocked } from './SessionsLocked';

function Section({
  title,
  description,
  icon,
  action,
  children,
}: {
  title: string;
  description: ReactNode;
  icon: ReactNode;
  action?: ReactNode;
  children?: ReactNode;
}) {
  const id = useId();
  return (
    <section
      aria-labelledby={id}
      className="grid gap-4 rounded-md border border-border bg-surface p-4"
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex gap-3">
          <span className="mt-0.5 text-fg-muted [&_svg]:size-5" aria-hidden="true">
            {icon}
          </span>
          <div className="grid gap-1">
            <h2 id={id} className="text-base font-semibold text-fg">
              {title}
            </h2>
            <div className="text-sm text-fg-muted">{description}</div>
          </div>
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}

function when(value: string | number | null | undefined): number | null {
  if (value === null || value === undefined) return null;
  const ms = typeof value === 'number' ? value : new Date(value).getTime();
  return Number.isFinite(ms) ? ms : null;
}

function RenamePasskeyDialog({
  passkey,
  onOpenChange,
}: {
  passkey: PasskeyItem | null;
  onOpenChange(open: boolean): void;
}) {
  const id = useId();
  const rename = useRenamePasskey();
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [lastId, setLastId] = useState<string | null>(null);
  if (passkey && passkey.id !== lastId) {
    setLastId(passkey.id);
    setName(passkey.name ?? '');
    setError(null);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!passkey || !name.trim()) return;
    try {
      await rename.mutateAsync({ id: passkey.id, name });
      onOpenChange(false);
    } catch (err) {
      setError(mfaErrorMessage(err));
    }
  }

  return (
    <Dialog open={passkey !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Rename passkey</DialogTitle>
          <DialogDescription>Choose a name you will recognize.</DialogDescription>
        </DialogHeader>
        <form className="grid gap-3" onSubmit={(event) => void submit(event)} noValidate>
          <div className="grid gap-1.5">
            <Label htmlFor={id}>Passkey name</Label>
            <Input
              id={id}
              value={name}
              maxLength={64}
              onChange={(event) => setName(event.target.value)}
              aria-invalid={error ? true : undefined}
              aria-describedby={error ? `${id}-error` : undefined}
            />
          </div>
          {error ? (
            <p id={`${id}-error`} role="alert" className="text-sm text-fg">
              {error}
            </p>
          ) : null}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="secondary" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={rename.isPending || !name.trim()}>
              {rename.isPending ? 'Saving…' : 'Save'}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Account › Security (J-3): passkeys, authenticator app, backup codes, recent sessions. */
export function AccountSecurityPage({ returnTo }: { returnTo?: string | null } = {}) {
  const resume = safeReturnTo(returnTo);
  const cookie = isCookieSession();
  const security = useMySecurity();
  const passkeys = usePasskeys();
  const sessions = useSessions();
  const refresh = useRefreshSecurity();
  const deletePasskey = useDeletePasskey();
  const disableTotp = useDisableTotp();

  const [adding, setAdding] = useState(false);
  const [settingUpApp, setSettingUpApp] = useState(false);
  const [renaming, setRenaming] = useState<PasskeyItem | null>(null);
  const [removing, setRemoving] = useState<PasskeyItem | null>(null);
  const [removingApp, setRemovingApp] = useState(false);
  const [confirmRegenerate, setConfirmRegenerate] = useState(false);
  const [newCodes, setNewCodes] = useState<string[] | null>(null);
  const regenerate = useRegenerateBackupCodes(setNewCodes);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const policy = security.data;
  // A pending setup is not enrollment: only a confirmed app counts.
  const totpOn = policy?.authenticator_enrolled === true;
  const passkeyList = passkeys.data ?? [];
  const noFactor =
    policy?.authenticator_enrolled === false && (policy.passkey_count ?? passkeyList.length) === 0;
  // Under required MFA the first factor is an allowed passkey (design §8):
  // starting an app setup then needs a check the account cannot give.
  const passkeyFirst = Boolean(policy?.mfa_required) && noFactor;
  const allowedFactors =
    (policy?.allow_passkeys !== false ? passkeyList.length : 0) +
    (policy?.allow_authenticator !== false && totpOn ? 1 : 0);
  const lastFactorLocked = Boolean(policy?.mfa_required) && allowedFactors <= 1;
  const lockedHint =
    'Your organization requires a second factor, so your only one cannot be removed. Add another factor first.';

  async function run(action: () => Promise<unknown>, done: string) {
    setActionError(null);
    setNotice(null);
    try {
      await action();
      setNotice(done);
    } catch (err) {
      setActionError(mfaErrorMessage(err));
    }
  }

  if (!cookie) {
    return (
      <main className="w-full">
        <PageHeader title="Security" description="Passkeys, authenticator app and backup codes." />
        <div role="status" className="rounded-md border border-border bg-surface p-6 text-sm">
          <h2 className="font-semibold text-fg">Sign in with email, Google or a passkey</h2>
          <p className="mt-1 text-fg-muted">
            You are signed in with a personal access token. Managing second factors needs a browser
            sign-in.
          </p>
        </div>
      </main>
    );
  }

  return (
    <main className="w-full">
      <PageHeader
        title="Security"
        description={
          <>
            Second factors protect your account if someone gets into your email. Sensitive actions
            ask for one again after {policy?.fresh_minutes ?? 10} minutes.{' '}
            <Link to="/settings" className="text-accent underline-offset-4 hover:underline">
              Back to account settings
            </Link>
          </>
        }
      />

      {resume ? (
        <div
          role="status"
          className="mb-4 flex flex-wrap items-center justify-between gap-3 rounded-md border border-border bg-surface px-4 py-3 text-sm"
        >
          <span>An app is waiting for you to finish authorizing it.</span>
          <Button type="button" size="sm" onClick={() => resumeAt(resume)}>
            Continue authorizing
          </Button>
        </div>
      ) : null}

      <div aria-live="polite" className="sr-only">
        {notice ?? ''}
      </div>
      {actionError ? (
        <p
          role="alert"
          className="mb-4 rounded-md border border-border bg-surface px-4 py-3 text-sm text-fg"
        >
          {actionError}
        </p>
      ) : notice ? (
        <p className="mb-4 rounded-md border border-border bg-surface px-4 py-3 text-sm text-fg">
          {notice}
        </p>
      ) : null}

      <div className="grid gap-6">
        <Section
          title={policy?.mfa_enrolled ? 'Two-step sign-in is on' : 'Two-step sign-in is off'}
          icon={<ShieldCheck />}
          description={
            policy?.mfa_required
              ? 'Your organization requires a second factor for your role.'
              : policy?.mfa_enrolled
                ? 'You will be asked for a second factor after signing in with email or Google.'
                : 'Add a passkey or an authenticator app to protect your account.'
          }
        />

        <Section
          title="Passkeys"
          icon={<KeyRound />}
          description={
            policy?.allow_passkeys === false
              ? 'Your organization does not allow passkeys.'
              : 'Sign in with your device’s fingerprint, face or PIN, or a security key. A passkey sign-in needs no email code.'
          }
          action={
            policy?.allow_passkeys !== false ? (
              <Button type="button" size="sm" onClick={() => setAdding(true)}>
                Add passkey
              </Button>
            ) : null
          }
        >
          {passkeys.isLoading ? (
            <p className="text-sm text-fg-muted">Loading passkeys…</p>
          ) : passkeys.isError ? (
            <p role="alert" className="text-sm text-fg">
              Could not load your passkeys.
            </p>
          ) : passkeyList.length === 0 ? (
            <p className="text-sm text-fg-muted">No passkeys yet.</p>
          ) : (
            <ul aria-label="Your passkeys" className="grid gap-2">
              {passkeyList.map((item) => {
                const created = when(item.createdAt);
                const used = when(item.lastUsedAt);
                const name = item.name || 'Unnamed passkey';
                return (
                  <li
                    key={item.id}
                    className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border p-3"
                  >
                    <div className="grid gap-0.5">
                      <span className="text-sm font-medium text-fg">{name}</span>
                      <span className="text-xs text-fg-muted">
                        {created ? `Added ${new Date(created).toLocaleDateString()} · ` : ''}
                        {used ? `Last used ${formatRelativeTime(used)}` : 'Not used to sign in yet'}
                      </span>
                    </div>
                    <div className="flex gap-2">
                      <Button
                        type="button"
                        variant="secondary"
                        size="sm"
                        aria-label={`Rename ${name}`}
                        onClick={() => setRenaming(item)}
                      >
                        Rename
                      </Button>
                      <Button
                        type="button"
                        variant="secondary"
                        size="sm"
                        aria-label={`Remove ${name}`}
                        disabled={lastFactorLocked || deletePasskey.isPending}
                        aria-describedby={lastFactorLocked ? 'last-factor-hint' : undefined}
                        onClick={() => setRemoving(item)}
                      >
                        Remove
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </Section>

        <Section
          title="Authenticator app"
          icon={<Smartphone />}
          description={
            policy?.allow_authenticator === false
              ? 'Your organization does not allow authenticator apps.'
              : totpOn
                ? 'On. Your app gives a 6-digit code when you sign in.'
                : 'Get a 6-digit code from an app on your phone, with backup codes for when you lose it.'
          }
          action={
            totpOn && !settingUpApp ? (
              <Button
                type="button"
                variant="secondary"
                size="sm"
                disabled={lastFactorLocked || disableTotp.isPending}
                aria-describedby={lastFactorLocked ? 'last-factor-hint' : undefined}
                onClick={() => setRemovingApp(true)}
              >
                Remove
              </Button>
            ) : policy?.allow_authenticator !== false && !settingUpApp ? (
              <Button
                type="button"
                size="sm"
                disabled={passkeyFirst}
                aria-describedby={passkeyFirst ? 'passkey-first-hint' : undefined}
                onClick={() => setSettingUpApp(true)}
              >
                Set up
              </Button>
            ) : null
          }
        >
          {passkeyFirst && policy?.allow_authenticator !== false ? (
            <p id="passkey-first-hint" className="text-sm text-fg-muted">
              {policy?.allow_passkeys !== false
                ? 'Your organization requires a second factor, so your first one must be a passkey. Add a passkey above, then you can add an authenticator app.'
                : 'Your organization requires a second factor but allows only authenticator apps, which cannot be your first factor. Ask an organization owner to allow passkeys while you set up, or to reset your account.'}
            </p>
          ) : null}
          {settingUpApp ? (
            <AuthenticatorSetup
              onCancel={() => setSettingUpApp(false)}
              onEnrolled={() => void refresh()}
              onDone={() => {
                setSettingUpApp(false);
                setNotice(
                  'Authenticator app set up. Sensitive actions will ask for a new code from it.',
                );
                void refresh();
              }}
            />
          ) : null}
          {totpOn && !settingUpApp ? (
            <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-4">
              <div className="grid gap-0.5">
                <h3 className="text-sm font-semibold text-fg">Backup codes</h3>
                <p className="text-sm text-fg-muted">
                  Ten one-time codes for when you cannot use your app. New codes replace the old
                  ones.
                </p>
              </div>
              <Button
                type="button"
                variant="secondary"
                size="sm"
                disabled={regenerate.isPending}
                onClick={() => setConfirmRegenerate(true)}
              >
                Regenerate backup codes
              </Button>
            </div>
          ) : null}
        </Section>
        {lastFactorLocked ? (
          <p id="last-factor-hint" className="-mt-3 text-xs text-fg-muted">
            {lockedHint}
          </p>
        ) : null}

        <Section
          title="Recent sessions"
          icon={<ShieldCheck />}
          description={
            <>
              Browsers signed in to your account.{' '}
              <Link to="/settings" className="text-accent underline-offset-4 hover:underline">
                Manage sessions
              </Link>
            </>
          }
        >
          {sessions.isLoading ? (
            <p className="text-sm text-fg-muted">Loading sessions…</p>
          ) : sessionsLocked(sessions.error) ? (
            <SessionsLocked onVerified={() => void sessions.refetch()} />
          ) : sessions.isError ? (
            <p role="alert" className="text-sm text-fg">
              Could not load your sessions.
            </p>
          ) : (
            <ul aria-label="Recent sessions" className="grid gap-2">
              {[...(sessions.data ?? [])]
                .sort((a, b) => (when(b.createdAt) ?? 0) - (when(a.createdAt) ?? 0))
                .slice(0, 10)
                .map((item) => (
                  <li key={item.id} className="grid gap-0.5 rounded-md border border-border p-3">
                    <span className="text-sm font-medium text-fg">
                      {item.userAgent || 'Web browser'}
                    </span>
                    <span className="text-xs text-fg-muted">
                      {[
                        when(item.createdAt) !== null
                          ? `Signed in ${formatRelativeTime(when(item.createdAt) as number)}`
                          : 'Signed in',
                        item.ipAddress,
                      ]
                        .filter(Boolean)
                        .join(' · ')}
                    </span>
                  </li>
                ))}
            </ul>
          )}
        </Section>
      </div>

      <Dialog open={adding} onOpenChange={setAdding}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add a passkey</DialogTitle>
            <DialogDescription>
              Name it, then follow your browser’s prompt to create it on this device or a security
              key.
            </DialogDescription>
          </DialogHeader>
          {adding ? (
            <AddPasskeyForm
              onCancel={() => setAdding(false)}
              onAdded={() => {
                setAdding(false);
                setNotice('Passkey added.');
              }}
            />
          ) : null}
        </DialogContent>
      </Dialog>

      <RenamePasskeyDialog
        passkey={renaming}
        onOpenChange={(open) => (open ? undefined : setRenaming(null))}
      />

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => (open ? undefined : setRemoving(null))}
        title={`Remove ${removing?.name || 'this passkey'}?`}
        description="You will no longer be able to sign in or confirm actions with it."
        confirmLabel="Remove passkey"
        destructive
        onConfirm={() => {
          const target = removing;
          setRemoving(null);
          if (target) void run(() => deletePasskey.mutateAsync(target.id), 'Passkey removed.');
        }}
      />

      <ConfirmDialog
        open={removingApp}
        onOpenChange={setRemovingApp}
        title="Remove your authenticator app?"
        description="Its codes and your backup codes stop working."
        confirmLabel="Remove authenticator"
        destructive
        onConfirm={() => {
          setRemovingApp(false);
          void run(() => disableTotp.mutateAsync(), 'Authenticator app removed.');
        }}
      />

      <ConfirmDialog
        open={confirmRegenerate}
        onOpenChange={setConfirmRegenerate}
        title="Regenerate backup codes?"
        description="Your current backup codes stop working as soon as new ones are made."
        confirmLabel="Regenerate"
        onConfirm={() => {
          setConfirmRegenerate(false);
          void run(() => regenerate.mutateAsync(), 'New backup codes made.');
        }}
      />

      <Dialog
        open={newCodes !== null}
        onOpenChange={(open) => (open ? undefined : setNewCodes(null))}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Your new backup codes</DialogTitle>
            <DialogDescription>The old codes no longer work.</DialogDescription>
          </DialogHeader>
          {newCodes ? <BackupCodes codes={newCodes} /> : null}
          <div className="flex justify-end">
            <Button type="button" onClick={() => setNewCodes(null)}>
              I saved my backup codes
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </main>
  );
}
