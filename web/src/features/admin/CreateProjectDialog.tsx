import { useState, type FormEvent } from 'react';
import { useNavigate } from '@tanstack/react-router';
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
import { friendlyAdminError, useCreateProject } from './hooks';

export interface CreateProjectDialogProps {
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

export function CreateProjectDialog({ open, onOpenChange }: CreateProjectDialogProps) {
  const { org } = useOrgContext();
  const navigate = useNavigate();
  const createMutation = useCreateProject(org?.id);

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
      void navigate({ to: '/p/$project', params: { project: slug.trim() } });
    } catch (err) {
      setError(friendlyAdminError(err));
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>New project</DialogTitle>
          <DialogDescription>
            Projects partition runbooks, executions, skills, and access within{' '}
            {org?.title ?? 'the organization'}.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={(e) => void handleSubmit(e)} className="grid gap-4 py-2">
          <div className="grid gap-1.5">
            <Label htmlFor="project-title">Project title</Label>
            <Input
              id="project-title"
              value={title}
              onChange={(e) => handleTitleChange(e.target.value)}
              placeholder="e.g. Infrastructure"
              required
            />
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="project-slug">Project slug</Label>
            <Input
              id="project-slug"
              value={slug}
              onChange={(e) => {
                setCustomSlug(true);
                setSlug(e.target.value);
              }}
              placeholder="infrastructure"
              required
            />
            <p className="text-2xs text-fg-muted">URL-safe unique identifier for the project.</p>
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
              {createMutation.isPending ? 'Creating...' : 'Create project'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
