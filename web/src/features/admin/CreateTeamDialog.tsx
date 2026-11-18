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
import { useOrgContext } from '../context/OrgContext';
import { friendlyAdminError, useCreateTeam } from './hooks';

export interface CreateTeamDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .trim()
    .replace(/[^\w\s-]/g, '')
    .replace(/[\s_-]+/g, '-')
    .replace(/^-+|-+$/g, '');
}

export function CreateTeamDialog({ open, onOpenChange }: CreateTeamDialogProps) {
  const { org } = useOrgContext();
  const createMutation = useCreateTeam(org?.id);

  const [title, setTitle] = useState('');
  const [slug, setSlug] = useState('');
  const [customSlug, setCustomSlug] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function reset() {
    setTitle('');
    setSlug('');
    setCustomSlug(false);
    setError(null);
  }

  function handleClose(nextOpen: boolean) {
    if (!nextOpen) reset();
    onOpenChange(nextOpen);
  }

  function handleTitleChange(val: string) {
    setTitle(val);
    if (!customSlug) {
      setSlug(slugify(val));
    }
  }

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    if (!title.trim() || !slug.trim()) {
      setError('Title and slug are required.');
      return;
    }
    setError(null);
    try {
      await createMutation.mutateAsync({
        title: title.trim(),
        slug: slug.trim(),
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
          <DialogTitle>Create team</DialogTitle>
          <DialogDescription>
            Teams allow grouping organization members for checklist and escalation targets.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={(e) => void handleSubmit(e)} className="grid gap-4 py-2">
          <div className="grid gap-1.5">
            <Label htmlFor="team-title">Team title</Label>
            <Input
              id="team-title"
              value={title}
              onChange={(e) => handleTitleChange(e.target.value)}
              placeholder="e.g. SRE Core"
              required
            />
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="team-slug">Team slug</Label>
            <Input
              id="team-slug"
              value={slug}
              onChange={(e) => {
                setCustomSlug(true);
                setSlug(e.target.value);
              }}
              placeholder="sre-core"
              required
            />
            <p className="text-2xs text-fg-muted">URL-safe unique identifier for the team.</p>
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
              {createMutation.isPending ? 'Creating...' : 'Create team'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
