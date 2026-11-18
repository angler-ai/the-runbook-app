import { useState } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { MonoRef } from '../status/MonoRef';
import { EmptyState } from '../layout/EmptyState';
import { Button } from '../primitives/button';
import { formatRelativeTime, formatDuration } from '../../lib/format';
import { Send, CheckCircle2, XCircle, RotateCw, KeyRound, Loader2 } from 'lucide-react';

export type DeliveryView = components['schemas']['DeliveryView'];
export type DeliveryAttemptView = components['schemas']['DeliveryAttemptView'];

export type DeliveryStatus = 'delivered' | 'retrying' | 'failed' | 'secret_unavailable';

export interface DeliveryTableProps {
  deliveries: DeliveryView[];
  onSelectDelivery?: (delivery: DeliveryView) => void;
  onLoadMore?: () => void;
  hasMore?: boolean;
  isLoadingMore?: boolean;
  now?: number;
  className?: string;
}

export function getDeliveryStatus(delivery: DeliveryView): DeliveryStatus {
  if (delivery.failure === 'secret_unavailable' || delivery.failure?.includes('secret')) {
    return 'secret_unavailable';
  }
  if (delivery.state === 'delivered') {
    return 'delivered';
  }
  if (delivery.state === 'failed') {
    return 'failed';
  }
  if (
    delivery.next_attempt_at_ms != null ||
    delivery.attempts.some((a) => a.outcome === 'retry') ||
    delivery.state === 'pending'
  ) {
    return 'retrying';
  }
  return 'failed';
}

function DeliveryStatusBadge({ status }: { status: DeliveryStatus }) {
  switch (status) {
    case 'delivered':
      return (
        <span
          data-slot="delivery-status"
          className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-2xs font-medium bg-verdict-pass-bg text-verdict-pass-fg border border-verdict-pass-border"
        >
          <CheckCircle2 className="size-3" aria-hidden="true" />
          <span>delivered</span>
        </span>
      );
    case 'retrying':
      return (
        <span
          data-slot="delivery-status"
          className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-2xs font-medium bg-verdict-warn-bg text-verdict-warn-fg border border-verdict-warn-border"
        >
          <RotateCw className="size-3" aria-hidden="true" />
          <span>retrying</span>
        </span>
      );
    case 'failed':
      return (
        <span
          data-slot="delivery-status"
          className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-2xs font-medium bg-verdict-fail-bg text-verdict-fail-fg border border-verdict-fail-border"
        >
          <XCircle className="size-3" aria-hidden="true" />
          <span>failed</span>
        </span>
      );
    case 'secret_unavailable':
      return (
        <span
          data-slot="delivery-status"
          className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-2xs font-medium bg-accent/10 text-accent border border-accent/30"
        >
          <KeyRound className="size-3" aria-hidden="true" />
          <span>secret_unavailable</span>
        </span>
      );
  }
}

export function DeliveryTable({
  deliveries,
  onSelectDelivery,
  onLoadMore,
  hasMore = false,
  isLoadingMore = false,
  now: propNow,
  className,
}: DeliveryTableProps) {
  const [mountedNow] = useState(() => Date.now());
  const now = propNow ?? mountedNow;

  if (deliveries.length === 0) {
    return (
      <EmptyState
        icon={Send}
        title="No webhook deliveries"
        description="No events have been dispatched to this webhook endpoint yet."
        className={className}
      />
    );
  }

  return (
    <div data-slot="delivery-table-container" className={cn('flex flex-col gap-4', className)}>
      <div className="overflow-x-auto border border-border rounded-lg bg-surface">
        <table className="w-full text-left text-xs border-collapse">
          <thead>
            <tr className="border-b border-border bg-surface-raised text-fg-muted font-medium">
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Event
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Status
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Attempts
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Response
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Next retry
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Duration
              </th>
            </tr>
          </thead>
          <tbody>
            {deliveries.map((delivery) => {
              const status = getDeliveryStatus(delivery);
              const lastAttempt = delivery.attempts[delivery.attempts.length - 1];
              const attemptCount = delivery.attempts.length;
              const isInteractive = Boolean(onSelectDelivery);

              return (
                <tr
                  key={delivery.id}
                  onClick={isInteractive ? () => onSelectDelivery?.(delivery) : undefined}
                  className={cn(
                    'border-b border-border/60 transition-colors last:border-b-0',
                    isInteractive
                      ? 'cursor-pointer hover:bg-surface-raised focus-visible:bg-surface-raised'
                      : '',
                  )}
                >
                  {/* Event */}
                  <td className="py-3 px-4">
                    <div className="flex flex-col gap-0.5">
                      <span className="font-semibold text-fg font-mono text-2xs">
                        {delivery.event_type}
                      </span>
                      <span className="text-[10px] text-fg-muted">
                        <MonoRef className="text-[10px]">{delivery.id.slice(0, 12)}</MonoRef>
                      </span>
                    </div>
                  </td>

                  {/* Status */}
                  <td className="py-3 px-4 whitespace-nowrap">
                    <DeliveryStatusBadge status={status} />
                  </td>

                  {/* Attempts */}
                  <td className="py-3 px-4 text-fg font-mono text-2xs">{attemptCount}</td>

                  {/* Last response code */}
                  <td className="py-3 px-4 text-fg font-mono text-2xs">
                    {lastAttempt?.status ? (
                      <span
                        className={cn(
                          'px-1.5 py-0.5 rounded font-mono',
                          lastAttempt.status >= 200 && lastAttempt.status < 300
                            ? 'bg-verdict-pass-bg text-verdict-pass-fg'
                            : 'bg-verdict-fail-bg text-verdict-fail-fg',
                        )}
                      >
                        {lastAttempt.status}
                      </span>
                    ) : (
                      <span className="text-fg-muted">—</span>
                    )}
                  </td>

                  {/* Next retry time */}
                  <td className="py-3 px-4 whitespace-nowrap text-fg-muted">
                    {delivery.next_attempt_at_ms
                      ? formatRelativeTime(delivery.next_attempt_at_ms, now)
                      : '—'}
                  </td>

                  {/* Duration */}
                  <td className="py-3 px-4 whitespace-nowrap font-mono text-2xs text-fg-muted">
                    {lastAttempt?.latency_ms != null ? formatDuration(lastAttempt.latency_ms) : '—'}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {/* Pagination Load more */}
      {hasMore && onLoadMore && (
        <div className="flex justify-center pt-1">
          <Button
            variant="secondary"
            size="sm"
            onClick={onLoadMore}
            disabled={isLoadingMore}
            className="text-xs"
          >
            {isLoadingMore ? (
              <>
                <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
                <span>Loading deliveries...</span>
              </>
            ) : (
              'Load more deliveries'
            )}
          </Button>
        </div>
      )}
    </div>
  );
}
