import { useMemo, useState, type FormEvent } from 'react';
import { useMe } from '../../api/hooks';
import { Button } from '../../ui/primitives/button';
import { Checkbox } from '../../ui/primitives/checkbox';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { useOrgContext } from '../context/OrgContext';
import { friendlySettingsError, useCreatePersonalToken, type TokenSecretView } from './hooks';

/** A token to replace: its name, scopes and projects prefill the form. */
export interface TokenTemplate {
  name: string;
  scopes: string[];
  projects: string[];
}

export interface CreatePersonalTokenDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  template?: TokenTemplate | null;
}

const AVAILABLE_SCOPES = [
  { id: 'runbooks:read', label: 'runbooks:read', desc: 'Read runbooks and versions' },
  { id: 'runbooks:write', label: 'runbooks:write', desc: 'Edit draft runbooks' },
  { id: 'runbooks:publish', label: 'runbooks:publish', desc: 'Publish runbook versions' },
  { id: 'executions:read', label: 'executions:read', desc: 'View executions and logs' },
  { id: 'executions:start', label: 'executions:start', desc: 'Start and rerun executions' },
  { id: 'executions:act', label: 'executions:act', desc: 'Complete checklists and reviews' },
  { id: 'executions:admin', label: 'executions:admin', desc: 'Pause, resume, and cancel runs' },
  { id: 'skills:read', label: 'skills:read', desc: 'Read project skills' },
  { id: 'tokens:manage', label: 'tokens:manage', desc: 'Manage personal and service tokens' },
];

const DEFAULT_SCOPES = ['runbooks:read', 'executions:read', 'executions:start', 'executions:act'];

const DAY_MS = 86_400_000;
const PRESETS = [7, 30, 90, 365];
const DEFAULT_DAYS = 90;
/** Room for the clock and the request's trip, so the server never sees a later time. */
const LIMIT_MARGIN_MS = 5 * 60_000;

export interface ExpiryOption {
  /** A preset's day count, or `limit` for "until the creating sign-in ends". */
  value: string;
  label: string;
}

export interface ExpiryChoices {
  options: ExpiryOption[];
  defaultValue: string;
  /** The latest expiry allowed, when the creating credential expires. */
  limitMs: number | null;
}

/**
 * The expiry presets this caller may pick. A personal token cannot outlive
 * the credential that creates it (`tokens.exceeds_caller` on
 * `expires_at_ms`): a browser session ends after 30 days without use, so the
 * presets past that end are left out, the largest remaining one is the
 * default, and "until the session ends" covers the gap.
 */
export function expiryChoices(nowMs: number, callerExpiresAtMs: number | null): ExpiryChoices {
  const limitMs = callerExpiresAtMs === null ? null : callerExpiresAtMs - LIMIT_MARGIN_MS;
  const fits = (days: number) => limitMs === null || nowMs + days * DAY_MS <= limitMs;
  const allowed = PRESETS.filter(fits);
  const options: ExpiryOption[] = allowed.map((days) => ({
    value: String(days),
    label: days === 365 ? '1 year' : `${days} days`,
  }));
  if (limitMs !== null && !fits(365) && limitMs > nowMs) {
    options.push({
      value: 'limit',
      label: `Until ${new Date(limitMs).toLocaleDateString()} (when this sign-in ends)`,
    });
  }
  const defaultValue = fits(DEFAULT_DAYS)
    ? String(DEFAULT_DAYS)
    : allowed.length > 0
      ? String(allowed[allowed.length - 1])
      : 'limit';
  return {
    options: options.map((option) =>
      option.value === String(DEFAULT_DAYS)
        ? { ...option, label: `${option.label} (default)` }
        : option,
    ),
    defaultValue,
    limitMs: limitMs !== null && !fits(365) ? limitMs : null,
  };
}

/** The chosen expiry, never past the creating credential's end. */
export function expiryMs(value: string, nowMs: number, choices: ExpiryChoices): number {
  const preset = nowMs + Number(value) * DAY_MS;
  if (choices.limitMs === null) return preset;
  return value === 'limit' ? choices.limitMs : Math.min(preset, choices.limitMs);
}

export function CreatePersonalTokenDialog({
  open,
  onOpenChange,
  template,
}: CreatePersonalTokenDialogProps) {
  const { projects } = useOrgContext();
  const me = useMe();
  const callerExpiry = me.data?.credential.expires_at_ms ?? null;
  // When the form was last reset (mount or close); the submit recomputes.
  const [shownAt, setShownAt] = useState(() => Date.now());
  const choices = useMemo(() => expiryChoices(shownAt, callerExpiry), [shownAt, callerExpiry]);
  const [name, setName] = useState(template?.name ?? '');
  const [expiry, setExpiry] = useState<string | null>(null);
  const expiryValue =
    expiry !== null && choices.options.some((option) => option.value === expiry)
      ? expiry
      : choices.defaultValue;
  const [selectedScopes, setSelectedScopes] = useState<string[]>(
    template?.scopes ?? DEFAULT_SCOPES,
  );
  const [selectedProjects, setSelectedProjects] = useState<string[]>(template?.projects ?? []);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<TokenSecretView | null>(null);
  const [copied, setCopied] = useState(false);

  const createMutation = useCreatePersonalToken();

  const reset = () => {
    setName(template?.name ?? '');
    setExpiry(null);
    setSelectedScopes(template?.scopes ?? DEFAULT_SCOPES);
    setSelectedProjects(template?.projects ?? []);
    setError(null);
    setCreated(null);
    setCopied(false);
    setShownAt(Date.now());
  };

  const handleOpenChange = (nextOpen: boolean) => {
    if (!nextOpen) reset();
    onOpenChange(nextOpen);
  };

  const toggleScope = (scopeId: string) => {
    setSelectedScopes((prev) =>
      prev.includes(scopeId) ? prev.filter((s) => s !== scopeId) : [...prev, scopeId],
    );
  };

  const toggleProject = (projectId: string) => {
    setSelectedProjects((prev) =>
      prev.includes(projectId) ? prev.filter((p) => p !== projectId) : [...prev, projectId],
    );
  };

  const handleCopy = async () => {
    if (!created?.secret) return;
    try {
      await navigator.clipboard.writeText(created.secret);
      setCopied(true);
    } catch {
      // clipboard write failed
    }
  };

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    setError(null);
    try {
      const now = Date.now();
      const expires_at_ms = expiryMs(expiryValue, now, expiryChoices(now, callerExpiry));
      const res = await createMutation.mutateAsync({
        name: name.trim(),
        scopes: selectedScopes,
        expires_at_ms,
        projects: selectedProjects.length > 0 ? selectedProjects : undefined,
      });
      setCreated(res);
    } catch (err) {
      setError(friendlySettingsError(err));
    }
  };

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{created ? 'Token generated' : 'Create personal token'}</DialogTitle>
          <DialogDescription>
            {created
              ? 'Make sure to copy your personal access token now. You will not be able to see it again.'
              : 'Generate a personal access token for CLI logins or scripts.'}
          </DialogDescription>
        </DialogHeader>

        {created ? (
          <div className="grid gap-4 py-2">
            <div className="grid gap-1.5">
              <Label htmlFor="created-token-secret">Token secret</Label>
              <div className="flex items-center gap-2">
                <Input
                  id="created-token-secret"
                  readOnly
                  value={created.secret}
                  className="font-mono text-xs select-all"
                />
                <Button type="button" variant="secondary" onClick={() => void handleCopy()}>
                  {copied ? 'Copied' : 'Copy'}
                </Button>
              </div>
            </div>
            <div className="rounded-md border border-border bg-surface p-3 text-xs text-fg-muted">
              <p className="font-semibold text-fg">Keep this token safe</p>
              <p className="mt-1">
                This token carries the scopes you selected and grants access on your behalf.
              </p>
            </div>
            <DialogFooter>
              <Button type="button" onClick={() => handleOpenChange(false)}>
                Done
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <form className="grid gap-4 py-2" onSubmit={(e) => void handleSubmit(e)}>
            <div className="grid gap-1.5">
              <Label htmlFor="token-name">Name</Label>
              <Input
                id="token-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="e.g. laptop-cli, deploy-script"
                disabled={createMutation.isPending}
                required
              />
            </div>

            <div className="grid gap-1.5">
              <Label htmlFor="token-expiry">Expiration</Label>
              <select
                id="token-expiry"
                value={expiryValue}
                onChange={(e) => setExpiry(e.target.value)}
                disabled={createMutation.isPending}
                aria-describedby={choices.limitMs !== null ? 'token-expiry-limit' : undefined}
                className="flex h-9 w-full rounded-md border border-border bg-surface px-3 py-1 text-sm text-fg"
              >
                {choices.options.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
              {choices.limitMs !== null ? (
                <p id="token-expiry-limit" className="text-xs text-fg-muted">
                  A token cannot outlive the sign-in that creates it, and this browser session ends
                  on {new Date(choices.limitMs).toLocaleDateString()}. For a longer-lived token,
                  sign out and in again, then create it.
                </p>
              ) : null}
            </div>

            <div className="grid gap-2">
              <span className="text-sm font-medium text-fg">Scopes</span>
              <div className="grid max-h-48 gap-2 overflow-y-auto rounded-md border border-border p-3">
                {AVAILABLE_SCOPES.map((scope) => {
                  const scopeId = `scope-${scope.id.replace(':', '-')}`;
                  return (
                    <div key={scope.id} className="flex items-start gap-2 text-xs text-fg">
                      <Checkbox
                        id={scopeId}
                        checked={selectedScopes.includes(scope.id)}
                        onCheckedChange={() => toggleScope(scope.id)}
                        disabled={createMutation.isPending}
                      />
                      <label htmlFor={scopeId} className="grid gap-0.5 cursor-pointer">
                        <span className="font-mono font-medium">{scope.label}</span>
                        <span className="text-fg-muted">{scope.desc}</span>
                      </label>
                    </div>
                  );
                })}
              </div>
            </div>

            {projects.length > 0 ? (
              <div className="grid gap-2">
                <span className="text-sm font-medium text-fg">Project restrictions (optional)</span>
                <p className="text-xs text-fg-muted">
                  Leave unticked to permit all projects you can access.
                </p>
                <div className="flex flex-wrap gap-2">
                  {projects.map((proj) => {
                    const projId = `token-proj-${proj.id}`;
                    return (
                      <div
                        key={proj.id}
                        className="flex items-center gap-1.5 text-xs text-fg rounded-md border border-border px-2 py-1"
                      >
                        <Checkbox
                          id={projId}
                          checked={selectedProjects.includes(proj.id)}
                          onCheckedChange={() => toggleProject(proj.id)}
                          disabled={createMutation.isPending}
                        />
                        <label htmlFor={projId} className="cursor-pointer">
                          {proj.title}
                        </label>
                      </div>
                    );
                  })}
                </div>
              </div>
            ) : null}

            {error ? (
              <p role="alert" className="text-sm text-fg">
                {error}
              </p>
            ) : null}

            <DialogFooter>
              <Button
                type="button"
                variant="secondary"
                disabled={createMutation.isPending}
                onClick={() => handleOpenChange(false)}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={createMutation.isPending || !name.trim() || selectedScopes.length === 0}
              >
                {createMutation.isPending ? 'Creating…' : 'Create token'}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
