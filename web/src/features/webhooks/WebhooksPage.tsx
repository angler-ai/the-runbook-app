import { useNavigate, useParams, useSearch } from '@tanstack/react-router';
import { useMemo, useState } from 'react';
import {
  Webhook as WebhookIcon,
  Plus,
  Send,
  KeyRound,
  Pencil,
  Trash2,
  RotateCw,
  Shield,
  Loader2,
  AlertCircle,
  CheckCircle2,
} from 'lucide-react';
import { ApiError } from '../../api/client';
import { EmptyState, ErrorState, PageHeader, Skeleton } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '../../ui/primitives/dialog';
import { DeliveryTable, type DeliveryView } from '../../ui/webhooks/DeliveryTable';
import { SubscriptionDetails, WebhookHealth } from '../../ui/webhooks/SubscriptionCard';
import { MonoRef } from '../../ui/status/MonoRef';
import { useMe } from '../../api/hooks';
import { useMyProjectRole } from '../execution/hooks';
import { useOrgContext } from '../context/OrgContext';
import { ListPaneHeading, ListPaneItem, ListPaneItems, SplitPane } from '../../ui/layout';
import { useRunbookCatalog } from '../runbooks/hooks';
import { WebhookForm } from './WebhookForm';
import { RotateSecretDialog } from './RotateSecretDialog';
import {
  friendlyWebhookError,
  useDeleteWebhook,
  useRedeliver,
  useTestWebhook,
  useWebhookDeliveries,
  useWebhooks,
  type WebhookView,
} from './hooks';

export function WebhooksPage({ projectId: propProjectId }: { projectId?: string } = {}) {
  let navigate: ReturnType<typeof useNavigate> | null = null;
  try {
    // eslint-disable-next-line react-hooks/rules-of-hooks -- same try pattern as useParams below
    navigate = useNavigate();
  } catch {
    navigate = null;
  }
  // Keep the selection in the URL. Outside a router (some tests) the
  // navigate function throws when called; the page then keeps its state
  // in memory only.
  const syncUrl = (search: { webhook?: string; new?: string }) => {
    if (!navigate) return;
    try {
      void navigate({ to: '.', search, replace: true });
    } catch {
      // No router: nothing to sync.
    }
  };
  let pageSearch: { webhook?: string; new?: string } = {};
  try {
    // eslint-disable-next-line react-hooks/rules-of-hooks -- same try pattern as useParams below
    pageSearch = useSearch({ strict: false }) as { webhook?: string; new?: string };
  } catch {
    // Rendered outside the router in some tests.
  }
  let routeProject: string | undefined;
  try {
    const params = useParams({ strict: false }) as { project?: string };
    routeProject = params.project;
  } catch {
    // In tests or outside router
  }

  const { projects, isLoading: orgLoading } = useOrgContext();
  const currentProject = projects.find(
    (p) => p.slug === (propProjectId ?? routeProject) || p.id === (propProjectId ?? routeProject),
  );
  // The project's ID once the org's projects load (a slug would 404 the API).
  const projectId = propProjectId ?? currentProject?.id;

  const me = useMe();
  const myRole = useMyProjectRole(projectId, me.data?.user?.id);
  const isOrgAdmin = me.data?.org.role === 'owner' || me.data?.org.role === 'admin';
  const hasManageScope = Boolean(me.data?.credential.scopes?.includes('webhooks:manage'));
  const canManage = (isOrgAdmin || myRole === 'admin') && hasManageScope;

  const webhooksQuery = useWebhooks(projectId, { enabled: hasManageScope });
  const rawWebhooks = webhooksQuery.data?.items;
  const webhooks = useMemo(() => rawWebhooks ?? [], [rawWebhooks]);

  // Selected webhook for viewing delivery logs
  const [selectedWebhookId, setSelectedWebhookIdState] = useState<string | null>(
    pageSearch.webhook ?? null,
  );
  const setSelectedWebhookId = (id: string | null) => {
    setSelectedWebhookIdState(id);
    setEditing(false);
    syncUrl(id ? { webhook: id } : {});
  };

  // Active webhook object
  const activeWebhook = useMemo(() => {
    if (webhooks.length === 0) return null;
    if (selectedWebhookId) {
      return webhooks.find((w) => w.id === selectedWebhookId) ?? webhooks[0];
    }
    return webhooks[0];
  }, [webhooks, selectedWebhookId]);

  // Deliveries query for active webhook
  const deliveriesQuery = useWebhookDeliveries(projectId, activeWebhook?.id, {
    enabled: hasManageScope,
  });
  const catalogQuery = useRunbookCatalog(projectId);
  const catalogRunbooks = (catalogQuery.data?.pages ?? []).flatMap((page) => page.items);
  const allDeliveries = useMemo(
    () => deliveriesQuery.data?.pages.flatMap((p) => p.items) ?? [],
    [deliveriesQuery.data],
  );

  // Right-panel mode & action states. Selection and "new" live in the
  // URL so a webhook (or the create form) is a linkable place.
  const [createOpen, setCreateOpenState] = useState(pageSearch.new === '1');
  const [editing, setEditing] = useState(false);
  const setCreateOpen = (open: boolean) => {
    setCreateOpenState(open);
    syncUrl(open ? { new: '1' } : {});
  };
  const [rotatingWebhook, setRotatingWebhook] = useState<WebhookView | null>(null);
  const [deletingWebhook, setDeletingWebhook] = useState<WebhookView | null>(null);
  const [inspectingDelivery, setInspectingDelivery] = useState<DeliveryView | null>(null);

  const [actionError, setActionError] = useState<string | null>(null);
  const [actionSuccess, setActionSuccess] = useState<string | null>(null);

  // Mutations
  const deleteMutation = useDeleteWebhook(projectId);
  const testMutation = useTestWebhook(projectId);
  const redeliverMutation = useRedeliver(projectId, activeWebhook?.id);

  async function handleDeleteConfirm() {
    if (!deletingWebhook) return;
    setActionError(null);
    try {
      await deleteMutation.mutateAsync(deletingWebhook.id);
      if (selectedWebhookId === deletingWebhook.id) {
        setSelectedWebhookId(null);
      }
      setDeletingWebhook(null);
      setActionSuccess(`Webhook "${deletingWebhook.name}" deleted.`);
    } catch (err) {
      setActionError(friendlyWebhookError(err));
    }
  }

  async function handleSendTest(wh: WebhookView) {
    setActionError(null);
    setActionSuccess(null);
    try {
      const delivery = await testMutation.mutateAsync(wh.id);
      setActionSuccess(`Test event sent to "${wh.name}" (delivery ${delivery.id.slice(0, 12)}).`);
    } catch (err) {
      setActionError(friendlyWebhookError(err));
    }
  }

  async function handleRedeliver(delivery: DeliveryView) {
    setActionError(null);
    setActionSuccess(null);
    try {
      await redeliverMutation.mutateAsync(delivery.id);
      setActionSuccess(`Redelivery queued for event ${delivery.event_type}.`);
      setInspectingDelivery(null);
    } catch (err) {
      setActionError(friendlyWebhookError(err));
    }
  }

  if (me.data && !hasManageScope) {
    return (
      <div className="w-full">
        <PageHeader
          title="Webhooks"
          description="Receive real-time HTTP callbacks when events occur in this project."
        />
        <div role="status" className="mt-6 rounded-md border border-border bg-surface p-6 text-sm">
          <h3 className="font-semibold text-fg">Sign in with email or Google to manage webhooks</h3>
          <p className="mt-1 text-fg-muted">
            You are currently signed in with a personal access token. Managing webhooks requires
            signing in with email or Google.
          </p>
        </div>
      </div>
    );
  }

  if (webhooksQuery.isLoading || (!projectId && orgLoading)) {
    return (
      <div className="w-full">
        <PageHeader title="Webhooks" description="Loading webhooks…" />
        <div className="mt-6 space-y-4">
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-48 w-full" />
        </div>
      </div>
    );
  }

  if (webhooksQuery.isError) {
    const error = webhooksQuery.error;
    return (
      <div className="w-full">
        <ErrorState
          title="Could not load webhooks"
          message={
            error instanceof ApiError && error.status === 403
              ? 'You do not have permission to view or manage webhooks. Project administrator role is required.'
              : error instanceof Error
                ? error.message
                : 'An error occurred.'
          }
          code={error instanceof ApiError ? error.code : undefined}
          requestId={error instanceof ApiError ? (error.requestId ?? undefined) : undefined}
          onRetry={() => void webhooksQuery.refetch()}
        />
      </div>
    );
  }

  return (
    <div className="w-full">
      <PageHeader
        title="Webhooks"
        description="Receive real-time HTTP callbacks when events occur in this project."
        actions={
          canManage ? (
            <Button onClick={() => setCreateOpen(true)}>
              <Plus className="size-4 mr-1.5" aria-hidden="true" />
              <span>Add webhook</span>
            </Button>
          ) : undefined
        }
      />

      {/* Permission notice if read-only */}
      {!canManage ? (
        <div className="mt-4 rounded-lg border border-border bg-surface-raised p-3 text-xs text-fg-muted flex items-center gap-2">
          <Shield className="size-4 shrink-0 text-fg-muted" aria-hidden="true" />
          <span>
            You have read-only access to webhooks. Project administrator role or webhooks:manage
            permission is required to add or modify webhooks.
          </span>
        </div>
      ) : null}

      {/* Feedback banner */}
      {actionSuccess ? (
        <div
          role="status"
          className="mt-4 rounded-lg border border-verdict-pass-border bg-verdict-pass-bg p-3 text-xs text-verdict-pass-fg flex items-center justify-between"
        >
          <div className="flex items-center gap-2">
            <CheckCircle2 className="size-4 shrink-0" aria-hidden="true" />
            <span>{actionSuccess}</span>
          </div>
          <button
            type="button"
            onClick={() => setActionSuccess(null)}
            className="text-xs hover:underline ml-4"
          >
            Dismiss
          </button>
        </div>
      ) : null}

      {actionError ? (
        <div
          role="alert"
          className="mt-4 rounded-lg border border-verdict-fail-border bg-verdict-fail-bg p-3 text-xs text-verdict-fail-fg flex items-center justify-between"
        >
          <div className="flex items-center gap-2">
            <AlertCircle className="size-4 shrink-0" aria-hidden="true" />
            <span>{actionError}</span>
          </div>
          <button
            type="button"
            onClick={() => setActionError(null)}
            className="text-xs hover:underline ml-4"
          >
            Dismiss
          </button>
        </div>
      ) : null}

      {createOpen && projectId ? (
        <div className="mt-6 rounded-lg border border-border bg-surface p-4 sm:p-6">
          <h2 className="text-sm font-semibold text-fg mb-4">New webhook</h2>
          <WebhookForm
            projectId={projectId}
            runbooks={catalogRunbooks}
            onDone={(id) => {
              setCreateOpen(false);
              if (id) setSelectedWebhookId(id);
            }}
            onCancel={() => setCreateOpen(false)}
          />
        </div>
      ) : webhooks.length === 0 ? (
        <div className="mt-6">
          <EmptyState
            icon={WebhookIcon}
            title="No webhooks configured"
            description="Create an HTTPS webhook subscription to receive alerts and notifications."
            action={
              canManage ? (
                <Button onClick={() => setCreateOpen(true)} className="mt-2">
                  <Plus className="size-4 mr-1.5" aria-hidden="true" />
                  <span>Add webhook</span>
                </Button>
              ) : undefined
            }
          />
        </div>
      ) : (
        <div className="mt-6">
          {/* One page, list left and detail right: webhooks are few, their
              detail is the delivery log, and creating or editing belongs
              beside it - the same split as the run page. State lives in
              the URL (?webhook= / ?new=1) so a form is still linkable. */}
          <SplitPane
            ariaLabel="Webhooks"
            fillViewport
            leftWidth="md:w-80 lg:w-96"
            leftClassName="p-3 sm:p-4"
            left={
              <section aria-labelledby="subscriptions-heading">
                <ListPaneHeading
                  id="subscriptions-heading"
                  title="Subscriptions"
                  count={webhooks.length}
                />
                <ListPaneItems>
                  {webhooks.map((subscription) => (
                    <ListPaneItem
                      key={subscription.id}
                      selected={activeWebhook?.id === subscription.id}
                      onSelect={() => setSelectedWebhookId(subscription.id)}
                      icon={<WebhookIcon className="size-4" aria-hidden="true" />}
                      title={subscription.name}
                      trailing={<WebhookHealth subscription={subscription} />}
                    />
                  ))}
                </ListPaneItems>
              </section>
            }
            right={
              <div className="grid gap-6">
                {createOpen && projectId ? (
                  <section aria-label="New webhook">
                    <h2 className="text-sm font-semibold text-fg mb-4">New webhook</h2>
                    <WebhookForm
                      projectId={projectId}
                      runbooks={catalogRunbooks}
                      onDone={(id) => {
                        setCreateOpen(false);
                        if (id) setSelectedWebhookId(id);
                      }}
                      onCancel={() => setCreateOpen(false)}
                    />
                  </section>
                ) : editing && activeWebhook && projectId ? (
                  <section aria-label={`Edit ${activeWebhook.name}`}>
                    <h2 className="text-sm font-semibold text-fg mb-4">
                      Edit {activeWebhook.name}
                    </h2>
                    <WebhookForm
                      key={activeWebhook.id}
                      projectId={projectId}
                      webhook={activeWebhook}
                      runbooks={catalogRunbooks}
                      onDone={() => setEditing(false)}
                      onCancel={() => setEditing(false)}
                    />
                  </section>
                ) : !activeWebhook ? (
                  <EmptyState
                    title="Pick a webhook"
                    description="Choose one on the left to see its deliveries, or create a new one."
                  />
                ) : (
                  <section aria-labelledby="webhook-detail-heading" className="grid gap-4">
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <div className="grid min-w-0 gap-1">
                        <div className="flex flex-wrap items-center gap-2">
                          <h2
                            id="webhook-detail-heading"
                            className="text-base font-semibold text-fg"
                          >
                            {activeWebhook.name}
                          </h2>
                          <WebhookHealth subscription={activeWebhook} />
                        </div>
                        <MonoRef className="text-2xs text-fg-muted">{activeWebhook.id}</MonoRef>
                      </div>
                      {canManage ? (
                        <div className="flex flex-wrap items-center gap-1.5">
                          <Button
                            size="sm"
                            variant="secondary"
                            onClick={() => void handleSendTest(activeWebhook)}
                            disabled={testMutation.isPending}
                            title="Send a test ping"
                            aria-label={`Send test event to ${activeWebhook.name}`}
                          >
                            <Send className="size-3.5 mr-1" aria-hidden="true" />
                            <span>Test</span>
                          </Button>
                          <Button
                            size="sm"
                            variant="secondary"
                            onClick={() => setRotatingWebhook(activeWebhook)}
                            title="Rotate signing secret"
                            aria-label={`Rotate secret for ${activeWebhook.name}`}
                          >
                            <KeyRound className="size-3.5 mr-1" aria-hidden="true" />
                            <span>Rotate secret</span>
                          </Button>
                          <Button
                            size="sm"
                            variant="secondary"
                            onClick={() => setEditing(true)}
                            aria-label={`Edit ${activeWebhook.name}`}
                          >
                            <Pencil className="size-3.5 mr-1" aria-hidden="true" />
                            <span>Edit</span>
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => setDeletingWebhook(activeWebhook)}
                            className="text-verdict-fail-fg hover:text-verdict-fail-fg"
                            aria-label={`Delete ${activeWebhook.name}`}
                          >
                            <Trash2 className="size-3.5" aria-hidden="true" />
                          </Button>
                        </div>
                      ) : null}
                    </div>
                    <SubscriptionDetails subscription={activeWebhook} />
                  </section>
                )}
                {/* Delivery log for active webhook */}
                {activeWebhook && !createOpen ? (
                  <section
                    aria-labelledby="delivery-log-heading"
                    className="space-y-4 pt-4 border-t border-border"
                  >
                    <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2">
                      <div>
                        <h2 id="delivery-log-heading" className="text-sm font-semibold text-fg">
                          Deliveries
                        </h2>
                        <p className="text-xs text-fg-muted">
                          Recent delivery attempts and responses for this webhook endpoint. Kept for
                          90 days.
                        </p>
                      </div>

                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => void deliveriesQuery.refetch()}
                        disabled={deliveriesQuery.isFetching}
                        className="text-xs self-start sm:self-auto"
                      >
                        <RotateCw
                          className={`size-3.5 mr-1.5 ${deliveriesQuery.isFetching ? 'animate-spin' : ''}`}
                          aria-hidden="true"
                        />
                        <span>Refresh</span>
                      </Button>
                    </div>

                    {deliveriesQuery.isLoading ? (
                      <div className="space-y-3">
                        <Skeleton className="h-8 w-full" />
                        <Skeleton className="h-24 w-full" />
                      </div>
                    ) : deliveriesQuery.isError ? (
                      <ErrorState
                        title="Could not load deliveries"
                        message={
                          deliveriesQuery.error instanceof Error
                            ? deliveriesQuery.error.message
                            : 'An error occurred.'
                        }
                        onRetry={() => void deliveriesQuery.refetch()}
                      />
                    ) : (
                      <DeliveryTable
                        deliveries={allDeliveries}
                        onSelectDelivery={(delivery) => setInspectingDelivery(delivery)}
                        hasMore={Boolean(deliveriesQuery.hasNextPage)}
                        isLoadingMore={deliveriesQuery.isFetchingNextPage}
                        onLoadMore={() => void deliveriesQuery.fetchNextPage()}
                      />
                    )}
                  </section>
                ) : null}
              </div>
            }
          />
        </div>
      )}

      {/* Dialogs */}
      {projectId ? (
        <>
          <RotateSecretDialog
            open={Boolean(rotatingWebhook)}
            onOpenChange={(open) => !open && setRotatingWebhook(null)}
            projectId={projectId}
            webhook={rotatingWebhook}
          />

          {/* Delete confirmation dialog */}
          <Dialog
            open={Boolean(deletingWebhook)}
            onOpenChange={(open) => !open && setDeletingWebhook(null)}
          >
            <DialogContent className="sm:max-w-md">
              <DialogHeader>
                <DialogTitle>Delete webhook</DialogTitle>
                <DialogDescription>
                  Are you sure you want to delete &quot;{deletingWebhook?.name}&quot;? All pending
                  deliveries and history will be permanently deleted.
                </DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <Button
                  variant="ghost"
                  onClick={() => setDeletingWebhook(null)}
                  disabled={deleteMutation.isPending}
                >
                  Cancel
                </Button>
                <Button
                  variant="destructive"
                  onClick={() => void handleDeleteConfirm()}
                  disabled={deleteMutation.isPending}
                >
                  {deleteMutation.isPending ? (
                    <>
                      <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
                      <span>Deleting…</span>
                    </>
                  ) : (
                    'Delete webhook'
                  )}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>

          {/* Inspect Delivery Detail & Redeliver Modal */}
          <Dialog
            open={Boolean(inspectingDelivery)}
            onOpenChange={(open) => !open && setInspectingDelivery(null)}
          >
            <DialogContent className="sm:max-w-lg">
              <DialogHeader>
                <DialogTitle>Delivery details</DialogTitle>
                <DialogDescription>
                  Event {inspectingDelivery?.event_type} ({inspectingDelivery?.id.slice(0, 12)})
                </DialogDescription>
              </DialogHeader>

              {inspectingDelivery ? (
                <div className="space-y-4 py-2 text-xs">
                  <div className="grid grid-cols-2 gap-2 p-3 rounded-lg bg-surface-raised border border-border">
                    <div>
                      <span className="text-fg-muted font-medium">State:</span>{' '}
                      <span className="font-semibold uppercase text-fg">
                        {inspectingDelivery.state}
                      </span>
                    </div>
                    <div>
                      <span className="text-fg-muted font-medium">Attempts:</span>{' '}
                      <span className="text-fg">{inspectingDelivery.attempts.length}</span>
                    </div>
                  </div>

                  {inspectingDelivery.attempts.length > 0 ? (
                    <div className="space-y-2">
                      <h3 className="font-semibold text-fg">Attempt history</h3>
                      <div className="space-y-2 max-h-48 overflow-y-auto">
                        {inspectingDelivery.attempts.map((attempt, idx) => (
                          <div
                            key={idx}
                            className="p-2.5 rounded border border-border bg-surface text-2xs space-y-1"
                          >
                            <div className="flex items-center justify-between font-mono">
                              <span>Status: {attempt.status ?? 'Network failure'}</span>
                              <span className="text-fg-muted">{attempt.latency_ms} ms</span>
                            </div>
                            {attempt.response_snippet ? (
                              <pre className="p-1.5 rounded bg-surface-raised font-mono overflow-x-auto text-[10px]">
                                {attempt.response_snippet}
                              </pre>
                            ) : null}
                          </div>
                        ))}
                      </div>
                    </div>
                  ) : null}

                  <DialogFooter>
                    <Button variant="ghost" onClick={() => setInspectingDelivery(null)}>
                      Close
                    </Button>
                    {canManage ? (
                      <Button
                        onClick={() => void handleRedeliver(inspectingDelivery)}
                        disabled={redeliverMutation.isPending}
                      >
                        {redeliverMutation.isPending ? (
                          <>
                            <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
                            <span>Redelivering…</span>
                          </>
                        ) : (
                          <>
                            <RotateCw className="size-3.5 mr-1.5" aria-hidden="true" />
                            <span>Redeliver</span>
                          </>
                        )}
                      </Button>
                    ) : null}
                  </DialogFooter>
                </div>
              ) : null}
            </DialogContent>
          </Dialog>
        </>
      ) : null}
    </div>
  );
}
