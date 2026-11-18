import { useState, type FormEvent } from 'react';
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../../ui/primitives/select';
import { useOrgContext } from '../context/OrgContext';
import {
  friendlyAdminError,
  useCreateServiceToken,
  type ProjectRole,
  type TokenSecretView,
} from './hooks';
import { Check, Copy } from 'lucide-react';

export interface CreateServiceTokenDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
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
];

const EXPIRY_OPTIONS = [
  { label: '30 days', days: 30 },
  { label: '90 days (default)', days: 90 },
  { label: '1 year', days: 365 },
  { label: 'No expiry', days: 0 },
];

export function CreateServiceTokenDialog({ open, onOpenChange }: CreateServiceTokenDialogProps) {
  const { projects } = useOrgContext();
  const createMutation = useCreateServiceToken();

  const [name, setName] = useState('');
  const [project, setProject] = useState(projects[0]?.id ?? '');
  const [role, setRole] = useState<ProjectRole>('editor');
  const [selectedScopes, setSelectedScopes] = useState<string[]>([
    'runbooks:read',
    'executions:read',
    'executions:start',
  ]);
  const [expiryDays, setExpiryDays] = useState(90);
  const [error, setError] = useState<string | null>(null);
  const [createdSecret, setCreatedSecret] = useState<TokenSecretView | null>(null);
  const [copied, setCopied] = useState(false);

  function reset() {
    setName('');
    setProject(projects[0]?.id ?? '');
    setRole('editor');
    setSelectedScopes(['runbooks:read', 'executions:read', 'executions:start']);
    setExpiryDays(90);
    setError(null);
    setCreatedSecret(null);
    setCopied(false);
  }

  function handleClose(nextOpen: boolean) {
    if (!nextOpen) reset();
    onOpenChange(nextOpen);
  }

  function toggleScope(scopeId: string) {
    if (selectedScopes.includes(scopeId)) {
      setSelectedScopes(selectedScopes.filter((s) => s !== scopeId));
    } else {
      setSelectedScopes([...selectedScopes, scopeId]);
    }
  }

  async function handleCopy() {
    if (!createdSecret?.secret) return;
    await navigator.clipboard.writeText(createdSecret.secret);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    if (!name.trim()) {
      setError('Token name is required.');
      return;
    }
    if (!project) {
      setError('Please select a project for this service token.');
      return;
    }
    setError(null);

    const expires_at_ms = expiryDays > 0 ? Date.now() + expiryDays * 24 * 60 * 60 * 1000 : null;

    try {
      const result = await createMutation.mutateAsync({
        name: name.trim(),
        project,
        role,
        scopes: selectedScopes,
        expires_at_ms,
      });
      setCreatedSecret(result);
    } catch (err) {
      setError(friendlyAdminError(err));
    }
  }

  if (createdSecret) {
    return (
      <Dialog open={open} onOpenChange={handleClose}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Service token created</DialogTitle>
            <DialogDescription>
              Copy this token now. You will not be able to view it again once this dialog closes.
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-4 py-2">
            <div className="grid gap-1.5">
              <Label htmlFor="service-token-secret">Token secret</Label>
              <div className="flex items-center gap-2">
                <Input
                  id="service-token-secret"
                  readOnly
                  value={createdSecret.secret}
                  className="font-mono text-xs select-all bg-surface-raised"
                />
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  onClick={() => void handleCopy()}
                  aria-label="Copy token secret"
                >
                  {copied ? (
                    <Check className="size-4 text-verdict-pass-fg" />
                  ) : (
                    <Copy className="size-4" />
                  )}
                </Button>
              </div>
            </div>
          </div>

          <DialogFooter>
            <Button type="button" onClick={() => handleClose(false)}>
              Done
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Create service token</DialogTitle>
          <DialogDescription>
            Service tokens authenticate automated pipelines, CI/CD, and monitoring daemons within a
            project.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={(e) => void handleSubmit(e)} className="grid gap-4 py-2">
          <div className="grid gap-1.5">
            <Label htmlFor="service-token-name">Token name</Label>
            <Input
              id="service-token-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. GitHub Actions CI"
              required
            />
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div className="grid gap-1.5">
              <Label htmlFor="service-token-project">Project</Label>
              <Select value={project} onValueChange={setProject}>
                <SelectTrigger id="service-token-project">
                  <SelectValue placeholder="Select project" />
                </SelectTrigger>
                <SelectContent>
                  {projects.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.title}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="grid gap-1.5">
              <Label htmlFor="service-token-role">Project role</Label>
              <Select value={role} onValueChange={(v) => setRole(v as ProjectRole)}>
                <SelectTrigger id="service-token-role">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="viewer">Viewer</SelectItem>
                  <SelectItem value="operator">Operator</SelectItem>
                  <SelectItem value="editor">Editor</SelectItem>
                  <SelectItem value="admin">Admin</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="service-token-expiry">Expiration</Label>
            <Select value={String(expiryDays)} onValueChange={(val) => setExpiryDays(Number(val))}>
              <SelectTrigger id="service-token-expiry">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {EXPIRY_OPTIONS.map((opt) => (
                  <SelectItem key={opt.days} value={String(opt.days)}>
                    {opt.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-2 border-t border-border pt-3">
            <span className="text-xs font-medium text-fg">Scopes</span>
            <div className="grid max-h-40 gap-2 overflow-y-auto rounded-md border border-border p-2.5">
              {AVAILABLE_SCOPES.map((scope) => {
                const scopeId = `svc-scope-${scope.id.replace(':', '-')}`;
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
                      <span className="text-2xs text-fg-muted">{scope.desc}</span>
                    </label>
                  </div>
                );
              })}
            </div>
          </div>

          {error ? (
            <p role="alert" className="text-xs text-verdict-fail-fg font-medium">
              {error}
            </p>
          ) : null}

          <DialogFooter className="pt-2">
            <Button
              type="button"
              variant="secondary"
              onClick={() => handleClose(false)}
              disabled={createMutation.isPending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={createMutation.isPending}>
              {createMutation.isPending ? 'Creating...' : 'Create token'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
