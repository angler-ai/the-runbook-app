import { CalendarClock, Check, ChevronRight, Copy, Webhook } from 'lucide-react';
import { useId, useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { MonoRef } from '../../ui/status/MonoRef';
import { describeCron } from './schedule';
import { webhookTriggerUrl, type RunbookTriggersView, type WebhookTriggerView } from './triggers';

export interface TriggersPanelProps {
  triggers: RunbookTriggersView;
  projectRef: string;
  runbookSlug: string;
  publishedVersion: number | null;
  /** Defaults to the page origin; component tests pass a fixed origin. */
  origin?: string;
}

function WebhookTriggerRow({ trigger, url }: { trigger: WebhookTriggerView; url: string }) {
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);

  async function copyUrl() {
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      setCopyFailed(false);
    } catch {
      setCopied(false);
      setCopyFailed(true);
    }
  }

  return (
    <li className="grid gap-2 py-3 first:pt-0 last:pb-0">
      <div className="flex flex-wrap items-center gap-2">
        <Webhook className="size-4 shrink-0 text-fg-muted" aria-hidden="true" />
        <strong className="text-sm font-medium text-fg">{trigger.id}</strong>
        {trigger.conditional ? <span className="text-xs text-fg-muted">Conditional</span> : null}
        {trigger.hasPayloadSchema ? (
          <span className="text-xs text-fg-muted">Payload schema</span>
        ) : null}
        {trigger.deduped ? <span className="text-xs text-fg-muted">Deduped</span> : null}
      </div>
      <div className="flex min-w-0 flex-wrap items-start gap-2">
        <code className="min-w-0 flex-1 break-all rounded bg-surface-raised px-2 py-1 font-mono text-xs text-fg ring-1 ring-border ring-inset">
          POST {url}
        </code>
        <Button
          type="button"
          variant="secondary"
          size="sm"
          aria-label={`Copy ${trigger.id} webhook URL`}
          onClick={() => void copyUrl()}
        >
          {copied ? (
            <Check className="size-3.5" aria-hidden="true" />
          ) : (
            <Copy className="size-3.5" aria-hidden="true" />
          )}
          {copied ? 'Copied' : 'Copy URL'}
        </Button>
      </div>
      {copyFailed ? (
        <p role="alert" className="text-xs text-fg-muted">
          Copy failed. Select the URL above and copy it manually.
        </p>
      ) : null}
      {trigger.inputNames.length > 0 ? (
        <p className="text-xs text-fg-muted">Maps inputs: {trigger.inputNames.join(', ')}</p>
      ) : null}
    </li>
  );
}

export function TriggersPanel({
  triggers,
  projectRef,
  runbookSlug,
  publishedVersion,
  origin = window.location.origin,
}: TriggersPanelProps) {
  const headingId = useId();
  if (triggers.schedule.length === 0 && triggers.webhook.length === 0) return null;

  const parts = [
    triggers.schedule.length > 0
      ? `${triggers.schedule.length} schedule${triggers.schedule.length === 1 ? '' : 's'}`
      : null,
    triggers.webhook.length > 0
      ? `${triggers.webhook.length} webhook${triggers.webhook.length === 1 ? '' : 's'}`
      : null,
  ].filter(Boolean);
  const names = [...triggers.schedule, ...triggers.webhook].map((t) => t.id).join(', ');

  // A one-line summary that opens to the details: open, it pushed the graph
  // below the fold.
  return (
    <section
      aria-labelledby={headingId}
      className="mb-6 rounded-lg border border-border bg-surface"
    >
      <details className="group">
        <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-3 gap-y-1 rounded-lg px-4 py-3 hover:bg-surface-raised focus-visible:outline-2 focus-visible:outline-accent [&::-webkit-details-marker]:hidden">
          <ChevronRight
            className="size-4 shrink-0 text-fg-muted transition-transform group-open:rotate-90"
            aria-hidden="true"
          />
          <h2 id={headingId} className="text-sm font-semibold text-fg">
            Triggers
          </h2>
          <span className="text-sm text-fg-muted">
            {parts.join(' · ')}
            {names ? (
              <>
                {' '}
                · <span className="font-mono text-xs">{names}</span>
              </>
            ) : null}
          </span>
          <span className="ml-auto text-xs text-fg-muted">
            {publishedVersion != null ? (
              <>Active from published v{publishedVersion}</>
            ) : (
              <>In the draft: publish to activate</>
            )}
          </span>
        </summary>
        <div className="border-t border-border px-4 pb-4">
          <p className="mt-3 text-xs text-fg-muted">
            {publishedVersion != null
              ? 'Only the latest published version acts.'
              : 'Publish this runbook to activate them.'}{' '}
            Change them in the editor, under Runbook settings → Triggers.
          </p>
          {triggers.schedule.length > 0 ? (
            <div className="mt-4">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-fg-muted">
                Schedules
              </h3>
              <ul className="divide-y divide-border">
                {triggers.schedule.map((trigger) => (
                  <li key={trigger.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-3">
                    <CalendarClock className="size-4 shrink-0 text-fg-muted" aria-hidden="true" />
                    <strong className="text-sm font-medium text-fg">{trigger.id}</strong>
                    {describeCron(trigger.cron) ? (
                      <span className="text-sm text-fg">{describeCron(trigger.cron)}</span>
                    ) : null}
                    <MonoRef>{trigger.cron}</MonoRef>
                    <span className="text-xs text-fg-muted">{trigger.timezone}</span>
                    {trigger.catchUp ? (
                      <span className="text-xs text-fg-muted">
                        Catch up within {trigger.catchUp}
                      </span>
                    ) : null}
                    {trigger.inputNames.length > 0 ? (
                      <span className="text-xs text-fg-muted">
                        Inputs: {trigger.inputNames.join(', ')}
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          {triggers.webhook.length > 0 ? (
            <div className="mt-4">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-fg-muted">
                Webhooks
              </h3>
              <ul className="divide-y divide-border">
                {triggers.webhook.map((trigger) => (
                  <WebhookTriggerRow
                    key={trigger.id}
                    trigger={trigger}
                    url={webhookTriggerUrl({
                      origin,
                      project: projectRef,
                      runbook: runbookSlug,
                      trigger: trigger.id,
                    })}
                  />
                ))}
              </ul>
              <p className="mt-3 text-xs leading-relaxed text-fg-muted">
                Send a project service token with <code>executions:start</code> as{' '}
                <code>Authorization: Bearer &lt;token&gt;</code>. Senders that cannot set headers
                may append <code>?token=&lt;token&gt;</code> to the URL.
              </p>
            </div>
          ) : null}
        </div>
      </details>
    </section>
  );
}
