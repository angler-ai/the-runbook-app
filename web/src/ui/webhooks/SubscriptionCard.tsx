import { useState } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { MonoRef } from '../status/MonoRef';
import { formatRelativeTime } from '../../lib/format';
import {
  Webhook,
  Globe,
  Filter,
  KeyRound,
  CheckCircle2,
  AlertTriangle,
  XCircle,
  Clock,
} from 'lucide-react';

export type WebhookView = components['schemas']['WebhookView'];

export interface SubscriptionCardProps {
  subscription: WebhookView;
  now?: number;
  className?: string;
}

export function SubscriptionCard({ subscription, now: propNow, className }: SubscriptionCardProps) {
  const [mountedNow] = useState(() => Date.now());
  const now = propNow ?? mountedNow;
  const isDisabled = !subscription.enabled;

  return (
    <div
      data-slot="subscription-card"
      className={cn(
        'p-5 rounded-xl border border-border bg-surface text-fg flex flex-col gap-4 shadow-xs',
        isDisabled && 'border-verdict-fail-border bg-verdict-fail-bg',
        className,
      )}
    >
      {/* Top Header: Name + Health / Disabled status */}
      <div className="flex items-start justify-between gap-3 flex-wrap pb-3 border-b border-border">
        <div className="flex items-center gap-2.5">
          <div className="size-8 rounded-md flex items-center justify-center bg-accent/10 border border-accent/20 text-accent shrink-0">
            <Webhook className="size-4" aria-hidden="true" />
          </div>
          <div>
            <div className="flex items-center gap-2 flex-wrap">
              <h3 className="text-sm font-semibold text-fg">{subscription.name}</h3>
              <MonoRef className="text-[10px] text-fg-muted">{subscription.id}</MonoRef>
            </div>
          </div>
        </div>

        <div className="shrink-0">
          <WebhookHealth subscription={subscription} />
        </div>
      </div>

      <SubscriptionDetails subscription={subscription} now={now} />
    </div>
  );
}

/** The disabled/failing notices, the endpoint, the event filters and the signing secret. */
export function SubscriptionDetails({
  subscription,
  now: propNow,
  className,
}: SubscriptionCardProps) {
  const [mountedNow] = useState(() => Date.now());
  const now = propNow ?? mountedNow;
  const isFailing = Boolean(subscription.enabled && subscription.failing_since_ms);
  const isDisabled = !subscription.enabled;
  return (
    <div data-slot="subscription-details" className={cn('flex flex-col gap-4', className)}>
      {/* Disabled reason banner if applicable */}
      {isDisabled && (
        <div
          data-slot="disabled-notice"
          className="p-3 rounded-lg bg-verdict-fail-bg border border-verdict-fail-border text-xs text-verdict-fail-fg flex items-start gap-2"
        >
          <AlertTriangle
            className="size-4 shrink-0 mt-0.5 text-verdict-fail-fg"
            aria-hidden="true"
          />
          <div className="flex flex-col gap-0.5">
            <span className="font-semibold">Subscription disabled</span>
            <span className="text-2xs text-verdict-fail-fg">
              Reason:{' '}
              {subscription.disabled_reason ||
                subscription.admin_notice?.message ||
                'Automatic disable after delivery failures'}
              {subscription.disabled_at_ms &&
                ` (${formatRelativeTime(subscription.disabled_at_ms, now)})`}
            </span>
          </div>
        </div>
      )}

      {/* Failing since notice */}
      {isFailing && subscription.failing_since_ms && (
        <div
          data-slot="failing-notice"
          className="p-3 rounded-lg bg-verdict-warn-bg border border-verdict-warn-border text-xs text-verdict-warn-fg flex items-start gap-2"
        >
          <Clock className="size-4 shrink-0 mt-0.5 text-verdict-warn-fg" aria-hidden="true" />
          <div className="flex flex-col gap-0.5">
            <span className="font-semibold">Recent deliveries failing</span>
            <span className="text-2xs text-verdict-warn-fg">
              Failing since {formatRelativeTime(subscription.failing_since_ms, now)}. Check the
              receiver endpoint status.
            </span>
          </div>
        </div>
      )}

      {/* Endpoint URL (Text only - NEVER a link) */}
      <div className="flex flex-col gap-1.5">
        <span className="text-2xs font-semibold uppercase tracking-wider text-fg-muted">
          Endpoint URL
        </span>
        <div className="flex items-center gap-2 p-2.5 rounded-lg bg-surface-raised border border-border">
          <Globe className="size-4 text-fg-muted shrink-0" aria-hidden="true" />
          {/* Plain text span: NEVER an <a> tag */}
          <span
            data-slot="subscription-url"
            className="font-mono text-xs text-fg break-all select-all flex-1"
          >
            {subscription.url}
          </span>
        </div>
      </div>

      {/* Event Filters */}
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-1.5 text-2xs font-semibold uppercase tracking-wider text-fg-muted">
          <Filter className="size-3" aria-hidden="true" />
          <span>Event filters</span>
        </div>
        <div className="flex items-center gap-1.5 flex-wrap">
          {subscription.events.length === 0 ? (
            <span
              data-slot="subscription-event-filter"
              className="px-2 py-0.5 rounded bg-surface-raised border border-border text-2xs text-fg-muted"
            >
              All events (*)
            </span>
          ) : (
            subscription.events.map((event) => (
              <span
                key={event}
                data-slot="subscription-event-filter"
                className="px-2 py-0.5 rounded bg-surface-raised border border-border text-2xs font-mono text-fg font-medium"
              >
                {event}
              </span>
            ))
          )}
        </div>
      </div>

      {/* Secret Rotation State (Never reveals raw secret) */}
      <div className="flex flex-col gap-1.5 pt-3 border-t border-border text-xs">
        <div className="flex items-center gap-1.5 text-2xs font-semibold uppercase tracking-wider text-fg-muted">
          <KeyRound className="size-3" aria-hidden="true" />
          <span>Signing secret</span>
        </div>

        <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-2 p-2.5 rounded-lg bg-surface-raised border border-border text-2xs">
          <div className="flex items-center gap-2 text-fg">
            <span className="font-medium">Secret rotated:</span>
            <span className="text-fg-muted">
              {formatRelativeTime(subscription.secret_rotated_at_ms, now)}
            </span>
          </div>

          {subscription.previous_secret_expires_at_ms ? (
            <div
              data-slot="dual-signing-notice"
              className="inline-flex items-center gap-1 text-verdict-warn-fg font-medium"
            >
              <Clock className="size-3" aria-hidden="true" />
              <span>
                Dual signing active: expires in{' '}
                {formatRelativeTime(subscription.previous_secret_expires_at_ms, now)}
              </span>
            </div>
          ) : (
            <span className="text-fg-muted">Single active key</span>
          )}
        </div>
      </div>
    </div>
  );
}

/** Active, Failing or Disabled. */
export function WebhookHealth({ subscription }: { subscription: WebhookView }) {
  const isFailing = Boolean(subscription.enabled && subscription.failing_since_ms);
  const isDisabled = !subscription.enabled;
  return (
    <>
      {isDisabled ? (
        <span
          data-slot="subscription-health"
          className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-2xs font-semibold bg-verdict-fail-bg text-verdict-fail-fg border border-verdict-fail-border"
        >
          <XCircle className="size-3.5" aria-hidden="true" />
          <span>Disabled</span>
        </span>
      ) : isFailing ? (
        <span
          data-slot="subscription-health"
          className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-2xs font-semibold bg-verdict-warn-bg text-verdict-warn-fg border border-verdict-warn-border"
        >
          <AlertTriangle className="size-3.5" aria-hidden="true" />
          <span>Failing</span>
        </span>
      ) : (
        <span
          data-slot="subscription-health"
          className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-2xs font-semibold bg-verdict-pass-bg text-verdict-pass-fg border border-verdict-pass-border"
        >
          <CheckCircle2 className="size-3.5" aria-hidden="true" />
          <span>Active</span>
        </span>
      )}
    </>
  );
}
