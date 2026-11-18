import { useState, type FormEvent } from 'react';
import { Button } from '../../ui/primitives/button';
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
  useCreateInvitation,
  type InvitationProjectGrant,
  type OrganizationRole,
  type ProjectRole,
} from './hooks';
import { Plus, X } from 'lucide-react';

export interface InviteMemberDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
}

export function InviteMemberDialog({ open, onOpenChange }: InviteMemberDialogProps) {
  const { org, projects } = useOrgContext();
  const createMutation = useCreateInvitation(org?.id);

  const [email, setEmail] = useState('');
  const [name, setName] = useState('');
  const [role, setRole] = useState<OrganizationRole>('member');
  const [grants, setGrants] = useState<InvitationProjectGrant[]>([]);
  const [selectedProject, setSelectedProject] = useState<string>('');
  const [selectedRole, setSelectedRole] = useState<ProjectRole>('editor');
  const [error, setError] = useState<string | null>(null);

  function reset() {
    setEmail('');
    setName('');
    setRole('member');
    setGrants([]);
    setSelectedProject('');
    setSelectedRole('editor');
    setError(null);
  }

  function handleClose(nextOpen: boolean) {
    if (!nextOpen) reset();
    onOpenChange(nextOpen);
  }

  function handleAddGrant() {
    if (!selectedProject) return;
    if (grants.some((g) => g.project === selectedProject)) return;
    setGrants([...grants, { project: selectedProject, role: selectedRole }]);
    setSelectedProject('');
  }

  function handleRemoveGrant(projectRef: string) {
    setGrants(grants.filter((g) => g.project !== projectRef));
  }

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    if (!email.trim()) {
      setError('Please provide an email address.');
      return;
    }
    setError(null);
    try {
      await createMutation.mutateAsync({
        email: email.trim(),
        ...(name.trim() ? { name: name.trim() } : {}),
        role,
        projects: grants.length > 0 ? grants : undefined,
      });
      handleClose(false);
    } catch (err) {
      setError(friendlyAdminError(err));
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Invite member</DialogTitle>
          <DialogDescription>
            Invite a colleague to {org?.title ?? 'the organization'} by email address.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={(e) => void handleSubmit(e)} className="grid gap-4 py-2">
          <div className="grid gap-1.5">
            <Label htmlFor="invite-email">Email address</Label>
            <Input
              id="invite-email"
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="colleague@example.com"
              required
            />
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="invite-name">Name (optional)</Label>
            <Input
              id="invite-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Alex Rivera"
              maxLength={100}
              autoComplete="off"
            />
            <p className="text-xs text-fg-muted">
              The name they appear under. They can change it later.
            </p>
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="invite-role">Organization role</Label>
            <Select value={role} onValueChange={(val) => setRole(val as OrganizationRole)}>
              <SelectTrigger id="invite-role">
                <SelectValue placeholder="Select role" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="member">Member</SelectItem>
                <SelectItem value="admin">Admin</SelectItem>
                <SelectItem value="owner">Owner</SelectItem>
              </SelectContent>
            </Select>
            <p className="text-xs text-fg-muted" data-testid="invite-role-hint">
              {role === 'member'
                ? 'Sees only the projects you grant below.'
                : 'Manages members, teams, projects and tokens. Sees a project’s runbooks and runs only with a project role, so grant the projects they work in below.'}
            </p>
          </div>

          {projects.length > 0 ? (
            <div className="grid gap-2 border-t border-border pt-3">
              <span className="text-xs font-medium text-fg">Project grants (optional)</span>
              {grants.length > 0 ? (
                <div className="grid gap-1.5">
                  {grants.map((g) => {
                    const proj = projects.find((p) => p.id === g.project || p.slug === g.project);
                    return (
                      <div
                        key={g.project}
                        className="flex items-center justify-between rounded border border-border bg-surface-raised px-2.5 py-1 text-xs"
                      >
                        <span className="font-medium text-fg">{proj?.title ?? g.project}</span>
                        <div className="flex items-center gap-2">
                          <span className="rounded bg-surface px-1.5 py-0.5 text-2xs text-fg-muted font-mono">
                            {g.role}
                          </span>
                          <button
                            type="button"
                            onClick={() => handleRemoveGrant(g.project)}
                            className="text-fg-muted hover:text-fg"
                            aria-label={`Remove grant for ${proj?.title ?? g.project}`}
                          >
                            <X className="size-3.5" />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              ) : null}

              <div className="flex items-center gap-2">
                <Select value={selectedProject} onValueChange={setSelectedProject}>
                  <SelectTrigger className="flex-1 text-xs">
                    <SelectValue placeholder="Pick a project" />
                  </SelectTrigger>
                  <SelectContent>
                    {projects
                      .filter(
                        (p) => !grants.some((g) => g.project === p.id || g.project === p.slug),
                      )
                      .map((p) => (
                        <SelectItem key={p.id} value={p.slug}>
                          {p.title}
                        </SelectItem>
                      ))}
                  </SelectContent>
                </Select>
                <Select
                  value={selectedRole}
                  onValueChange={(val) => setSelectedRole(val as ProjectRole)}
                >
                  <SelectTrigger className="w-28 text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="viewer">Viewer</SelectItem>
                    <SelectItem value="operator">Operator</SelectItem>
                    <SelectItem value="editor">Editor</SelectItem>
                    <SelectItem value="admin">Admin</SelectItem>
                  </SelectContent>
                </Select>
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  onClick={handleAddGrant}
                  disabled={!selectedProject}
                  aria-label="Add project grant"
                >
                  <Plus className="size-3.5" />
                </Button>
              </div>
            </div>
          ) : null}

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
              {createMutation.isPending ? 'Sending...' : 'Send invitation'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
