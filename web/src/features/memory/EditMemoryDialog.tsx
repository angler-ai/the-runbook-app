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
import { Textarea } from '../../ui/primitives/textarea';
import { memoryErrorText, usePatchMemory, type MemoryEntry } from './hooks';

export interface EditMemoryDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  runbookId?: string;
  projectId?: string;
  entry: MemoryEntry;
  onUpdated?(entry: MemoryEntry): void;
  onReload?(): void;
}

interface EditMemoryFormProps {
  runbookId?: string;
  projectId?: string;
  entry: MemoryEntry;
  onClose(): void;
  onUpdated?(entry: MemoryEntry): void;
  onReload?(): void;
}

function EditMemoryForm({
  runbookId,
  projectId,
  entry,
  onClose,
  onUpdated,
  onReload,
}: EditMemoryFormProps) {
  const [title, setTitle] = useState(entry.title);
  const [body, setBody] = useState(entry.body_markdown);
  const [tagsInput, setTagsInput] = useState((entry.tags ?? []).join(', '));
  const [stepId, setStepId] = useState(entry.step_id ?? '');
  const [error, setError] = useState<string | null>(null);

  const patchMutation = usePatchMemory({ runbookId, projectId });

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!title.trim() || !body.trim()) return;

    setError(null);
    const tags = tagsInput
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean);

    try {
      const updated = await patchMutation.mutateAsync({
        memoryId: entry.id,
        revision: entry.revision,
        patch: {
          action: 'edit',
          entry: {
            title: title.trim(),
            body_markdown: body.trim(),
            tags,
            step_id: stepId.trim() || undefined,
            expires_at: entry.expires_at,
          },
        },
      });
      onClose();
      onUpdated?.(updated);
    } catch (err) {
      const msg = memoryErrorText(err);
      setError(msg);
      if (msg === 'This entry changed since you opened it.') {
        onReload?.();
      }
    }
  };

  return (
    <form onSubmit={handleSubmit} className="grid gap-4">
      <DialogHeader>
        <DialogTitle>Edit memory entry</DialogTitle>
        <DialogDescription>Update the content or metadata for this memory entry.</DialogDescription>
      </DialogHeader>

      {error ? (
        <p
          role="alert"
          className="rounded-md border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger"
        >
          {error}
        </p>
      ) : null}

      <div className="grid gap-1.5">
        <Label htmlFor="edit-mem-title">Title</Label>
        <Input
          id="edit-mem-title"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="Title"
          required
        />
      </div>

      <div className="grid gap-1.5">
        <Label htmlFor="edit-mem-body">Body (Markdown)</Label>
        <Textarea
          id="edit-mem-body"
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Body..."
          rows={6}
          required
        />
      </div>

      <div className="grid gap-1.5">
        <Label htmlFor="edit-mem-tags">Tags (comma-separated)</Label>
        <Input
          id="edit-mem-tags"
          value={tagsInput}
          onChange={(e) => setTagsInput(e.target.value)}
          placeholder="e.g. redis, cache"
        />
      </div>

      <div className="grid gap-1.5">
        <Label htmlFor="edit-mem-step">Step ID (optional)</Label>
        <Input
          id="edit-mem-step"
          value={stepId}
          onChange={(e) => setStepId(e.target.value)}
          placeholder="e.g. step-1"
        />
      </div>

      <DialogFooter className="gap-2 pt-2">
        <Button type="button" variant="secondary" onClick={onClose}>
          Cancel
        </Button>
        <Button type="submit" disabled={patchMutation.isPending || !title.trim() || !body.trim()}>
          {patchMutation.isPending ? 'Saving…' : 'Save changes'}
        </Button>
      </DialogFooter>
    </form>
  );
}

export function EditMemoryDialog({
  open,
  onOpenChange,
  runbookId,
  projectId,
  entry,
  onUpdated,
  onReload,
}: EditMemoryDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        {open ? (
          <EditMemoryForm
            key={`${entry.id}:${entry.revision}`}
            runbookId={runbookId}
            projectId={projectId}
            entry={entry}
            onClose={() => onOpenChange(false)}
            onUpdated={onUpdated}
            onReload={onReload}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
