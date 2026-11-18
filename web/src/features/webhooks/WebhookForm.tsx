import { useState } from 'react';
import { AlertCircle, Check, Copy, KeyRound, Loader2 } from 'lucide-react';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { EventPicker } from './EventPicker';
import { eventsOfSelection, selectionOf } from './events';
import {
  friendlyWebhookError,
  useCreateWebhook,
  useUpdateWebhook,
  type WebhookView,
} from './hooks';

export interface WebhookFormProps {
  projectId: string;
  /** Present when editing; absent creates a new subscription. */
  webhook?: WebhookView;
  /** Runbooks the filter can name (empty selection means every runbook). */
  runbooks: { runbook_id: string; slug: string; title?: string | null }[];
  onDone(webhookId?: string): void;
  onCancel(): void;
}

export function WebhookForm({ projectId, webhook, runbooks, onDone, onCancel }: WebhookFormProps) {
  const [name, setName] = useState(webhook?.name ?? '');
  const [url, setUrl] = useState(webhook?.url ?? '');
  const [selected, setSelected] = useState<ReadonlySet<string>>(
    () => new Set(webhook ? selectionOf(webhook.events) : []),
  );
  const [runbookIds, setRunbookIds] = useState<ReadonlySet<string>>(
    () => new Set(webhook?.runbooks ?? []),
  );
  const [tagsInput, setTagsInput] = useState((webhook?.tags ?? []).join(', '));
  const [enabled, setEnabled] = useState(webhook?.enabled ?? true);
  const [error, setError] = useState<string | null>(null);
  const [createdSecret, setCreatedSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const createMutation = useCreateWebhook(projectId);
  const updateMutation = useUpdateWebhook(projectId);
  const pending = createMutation.isPending || updateMutation.isPending;

  async function handleSubmit(event: React.FormEvent) {
    event.preventDefault();
    setError(null);
    if (!name.trim()) {
      setError('Please provide a webhook name.');
      return;
    }
    if (!url.trim().startsWith('https://')) {
      setError('Webhook URL must be a valid URL starting with https://.');
      return;
    }
    if (selected.size === 0) {
      setError('Pick at least one event, or use Select all.');
      return;
    }
    const body = {
      name: name.trim(),
      url: url.trim(),
      events: eventsOfSelection(selected),
      runbooks: [...runbookIds],
      tags: tagsInput
        .split(',')
        .map((t) => t.trim())
        .filter(Boolean),
      enabled,
    };
    try {
      if (webhook) {
        await updateMutation.mutateAsync({ id: webhook.id, body });
        onDone(webhook.id);
      } else {
        const created = await createMutation.mutateAsync(body);
        setCreatedSecret(created.secret);
      }
    } catch (err) {
      setError(friendlyWebhookError(err));
    }
  }

  if (createdSecret) {
    return (
      <div className="space-y-4">
        <div className="rounded-lg border border-verdict-warn-border bg-verdict-warn-bg p-3 text-xs text-verdict-warn-fg flex items-start gap-2">
          <KeyRound className="size-4 shrink-0 mt-0.5" aria-hidden="true" />
          <span>
            Copy your signing secret now — it is shown only once, and deliveries are signed with it
            from the first one.
          </span>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="webhook-signing-secret" className="text-xs">
            Signing secret
          </Label>
          <div className="flex items-center gap-2">
            <Input
              id="webhook-signing-secret"
              readOnly
              value={createdSecret}
              className="font-mono text-xs select-all bg-surface-raised"
            />
            <Button
              type="button"
              size="sm"
              variant="secondary"
              aria-label="Copy signing secret"
              onClick={() => {
                void navigator.clipboard?.writeText(createdSecret).then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 2000);
                });
              }}
            >
              {copied ? (
                <Check className="size-3.5 text-verdict-pass-fg" aria-hidden="true" />
              ) : (
                <Copy className="size-3.5" aria-hidden="true" />
              )}
              <span>{copied ? 'Copied' : 'Copy'}</span>
            </Button>
          </div>
        </div>
        <Button type="button" onClick={() => onDone()}>
          Done
        </Button>
      </div>
    );
  }

  return (
    <form onSubmit={(e) => void handleSubmit(e)} className="grid gap-4">
      {error ? (
        <div
          role="alert"
          className="rounded-lg border border-verdict-fail-border bg-verdict-fail-bg p-3 text-xs text-verdict-fail-fg flex items-center gap-2"
        >
          <AlertCircle className="size-4 shrink-0" aria-hidden="true" />
          <span>{error}</span>
        </div>
      ) : null}
      <div className="space-y-1.5">
        <Label htmlFor="webhook-name" className="text-xs">
          Name
        </Label>
        <Input
          id="webhook-name"
          placeholder="e.g. Slack alerts, PagerDuty integration"
          value={name}
          onChange={(e) => setName(e.target.value)}
          disabled={pending}
          required
        />
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="webhook-url" className="text-xs">
          Endpoint URL
        </Label>
        <Input
          id="webhook-url"
          type="url"
          placeholder="https://example.com/webhooks/runbook"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          disabled={pending}
          required
        />
        <p className="text-2xs text-fg-muted">
          Must be an HTTPS URL. Credentials and loopback/private addresses are not permitted.
        </p>
      </div>
      <fieldset className="grid gap-2">
        <legend className="text-xs font-medium text-fg">Events</legend>
        <EventPicker selected={selected} onChange={setSelected} disabled={pending} />
      </fieldset>
      {runbooks.length > 0 ? (
        <fieldset className="grid gap-2">
          <legend className="text-xs font-medium text-fg">Runbooks</legend>
          <div className="grid gap-1.5">
            {runbooks.map((runbook) => (
              <label key={runbook.runbook_id} className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={runbookIds.has(runbook.runbook_id)}
                  disabled={pending}
                  onChange={(e) => {
                    const next = new Set(runbookIds);
                    if (e.target.checked) next.add(runbook.runbook_id);
                    else next.delete(runbook.runbook_id);
                    setRunbookIds(next);
                  }}
                  className="size-4 rounded border-border text-accent focus:ring-accent"
                />
                <span>
                  {runbook.title ?? runbook.slug}{' '}
                  <span className="font-mono text-2xs text-fg-muted">{runbook.slug}</span>
                </span>
              </label>
            ))}
          </div>
          <p className="text-2xs text-fg-muted">None ticked means every runbook in this project.</p>
        </fieldset>
      ) : null}
      <div className="space-y-1.5">
        <Label htmlFor="webhook-tags" className="text-xs">
          Tag filters (optional)
        </Label>
        <Input
          id="webhook-tags"
          placeholder="e.g. prod, critical"
          value={tagsInput}
          onChange={(e) => setTagsInput(e.target.value)}
          disabled={pending}
        />
        <p className="text-2xs text-fg-muted">
          Only receive events for runs carrying at least one of these tags (empty for all).
        </p>
      </div>
      <div className="flex items-center gap-2 pt-1">
        <input
          id="webhook-enabled"
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
          disabled={pending}
          className="size-4 rounded border-border text-accent focus:ring-accent"
        />
        <Label htmlFor="webhook-enabled" className="text-xs cursor-pointer">
          Enabled
        </Label>
      </div>
      <div className="flex gap-2">
        <Button type="submit" disabled={pending}>
          {pending ? (
            <>
              <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
              <span>{webhook ? 'Saving…' : 'Creating…'}</span>
            </>
          ) : webhook ? (
            'Save changes'
          ) : (
            'Create webhook'
          )}
        </Button>
        <Button type="button" variant="ghost" onClick={onCancel} disabled={pending}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
