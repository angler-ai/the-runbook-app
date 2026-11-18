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
import { Textarea } from '../../ui/primitives/textarea';
import { memoryErrorText, useCreateMemory, type MemoryEntry } from './hooks';

export interface CreateMemoryDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  runbookId?: string;
  projectId?: string;
  onCreated?(entry: MemoryEntry): void;
}

export function CreateMemoryDialog({
  open,
  onOpenChange,
  runbookId,
  projectId,
  onCreated,
}: CreateMemoryDialogProps) {
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [tagsInput, setTagsInput] = useState('');
  const [stepId, setStepId] = useState('');
  const [propose, setPropose] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const createMutation = useCreateMemory({ runbookId, projectId });

  const reset = () => {
    setTitle('');
    setBody('');
    setTagsInput('');
    setStepId('');
    setPropose(false);
    setError(null);
  };

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!title.trim() || !body.trim()) return;

    setError(null);
    const tags = tagsInput
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean);

    try {
      const created = await createMutation.mutateAsync({
        title: title.trim(),
        body_markdown: body.trim(),
        tags,
        step_id: stepId.trim() || undefined,
        propose: propose || undefined,
      });
      reset();
      onOpenChange(false);
      onCreated?.(created);
    } catch (err) {
      setError(memoryErrorText(err));
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) reset();
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <form onSubmit={handleSubmit} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>New memory entry</DialogTitle>
            <DialogDescription>
              Save context, run notes or knowledge for this runbook.
            </DialogDescription>
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
            <Label htmlFor="create-mem-title">Title</Label>
            <Input
              id="create-mem-title"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="e.g. Database failover timeout rule"
              required
            />
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="create-mem-body">Body (Markdown)</Label>
            <Textarea
              id="create-mem-body"
              value={body}
              onChange={(e) => setBody(e.target.value)}
              placeholder="Details and operational context..."
              rows={5}
              required
            />
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="create-mem-tags">Tags (comma-separated)</Label>
            <Input
              id="create-mem-tags"
              value={tagsInput}
              onChange={(e) => setTagsInput(e.target.value)}
              placeholder="e.g. postgres, failover"
            />
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="create-mem-step">Step ID (optional)</Label>
            <Input
              id="create-mem-step"
              value={stepId}
              onChange={(e) => setStepId(e.target.value)}
              placeholder="e.g. verify-cluster"
            />
          </div>

          <div className="flex items-center gap-2 pt-1">
            <Checkbox
              id="create-mem-propose"
              checked={propose}
              onCheckedChange={(checked) => setPropose(Boolean(checked))}
            />
            <Label
              htmlFor="create-mem-propose"
              className="cursor-pointer text-xs font-normal text-fg-muted"
            >
              Save as proposal for review
            </Label>
          </div>

          <DialogFooter className="gap-2 pt-2">
            <Button
              type="button"
              variant="secondary"
              onClick={() => {
                reset();
                onOpenChange(false);
              }}
            >
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={createMutation.isPending || !title.trim() || !body.trim()}
            >
              {createMutation.isPending ? 'Saving…' : propose ? 'Save proposal' : 'Save entry'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
