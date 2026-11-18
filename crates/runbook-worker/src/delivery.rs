//! Notification delivery through a Queue (CF4, D58). Portable: the
//! Workers queue binding and fetch transport are wired in `edge`; native
//! tests run it over the DO store on SQLite and core's in-memory webhooks.
//!
//! - **Producer (in the ExecutionDO).** After each commit, and when the
//!   alarm retries, [`publish`] reads the notification stream
//!   (`pending_notifications`), sends one message per intent
//!   (`send_batch`), and only then acknowledges through the last one
//!   (`ack_notifications`). A failed send acknowledges nothing and backs
//!   the stream off (`defer_outbox`), so the alarm tries again.
//! - **Message.** The tenant, the intent and the run facts a webhook
//!   envelope names ([`MetaWire`]), read by the object itself: the run's
//!   title and tags from its latest derived row, its version from the start
//!   record. The consumer needs no read of the object.
//! - **Consumer (in the api Worker).** [`consume`] is core's native
//!   pipeline: `plan_fan_out` and `record_event` (idempotent on the event
//!   ID, so a redelivered message records nothing twice), then
//!   `deliver_due`: signed requests, 2xx delivers, 408/429/5xx and
//!   timeouts retry 30 s doubling to 2 h within a 24-hour window, anything
//!   else fails, and every attempt is logged. The webhook tables are D1's
//!   (`WebhookStore`, CF3).
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{
        DeliverySettings, DeliveryStats, ExecutionMeta, SecretSealer, WebhookStore,
        WebhookTransport, deliver_due, plan_fan_out,
    },
};
use runbook_engine::NotificationIntent;
use runbook_model::{ExecutionId, RunbookId, VersionId};
use runbook_store_do::{DoSql, DoStore, NotificationCursor, OutboxStream};
use serde::{Deserialize, Serialize};
use std::future::Future;

/// Most intents one publish sends (a Queues batch holds up to 100).
pub const PUBLISH_BATCH: usize = 50;

/// The run facts a webhook envelope names, as they travel in a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaWire {
    pub id: ExecutionId,
    pub title: String,
    pub runbook_id: RunbookId,
    pub version_id: VersionId,
    pub version: Option<u32>,
    pub url: String,
    pub tags: Vec<String>,
}
impl From<MetaWire> for ExecutionMeta {
    fn from(meta: MetaWire) -> Self {
        Self {
            id: meta.id,
            title: meta.title,
            runbook_id: meta.runbook_id,
            version_id: meta.version_id,
            version: meta.version,
            url: meta.url,
            tags: meta.tags,
        }
    }
}

/// One notification intent on its way to the webhook fan-out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotificationMessage {
    pub scope: TenantScope,
    pub intent: NotificationIntent,
    pub meta: MetaWire,
}

/// A queue producer (a Queues binding on Workers).
pub trait NotificationQueue {
    fn send_batch(
        &self,
        messages: Vec<NotificationMessage>,
    ) -> impl Future<Output = Result<(), StoreError>>;
}

/// The facts for this object's run, with `url` filled per intent.
async fn meta<B: DoSql>(store: &DoStore<B>) -> Result<MetaWire, StoreError> {
    use runbook_core::ExecutionStore;
    let run = store.load(store.name()).await?;
    let summary = run.derived_outbox.values().next_back().map(|u| &u.summary);
    let record = store.start_record(store.name())?;
    Ok(MetaWire {
        id: store.name().clone(),
        title: summary.map(|s| s.title.clone()).unwrap_or_default(),
        runbook_id: run.projection.runbook_id.clone(),
        version_id: run.projection.version_id.clone(),
        version: record
            .and_then(|r| r.version)
            .map(std::num::NonZeroU32::get),
        url: String::new(),
        tags: summary.map(|s| s.tags.clone()).unwrap_or_default(),
    })
}

/// Send this object's undelivered notifications, then acknowledge them.
/// Returns how many were sent. A failed send defers the stream and returns
/// the error; nothing is acknowledged.
pub async fn publish<B: DoSql, Q: NotificationQueue>(
    store: &DoStore<B>,
    queue: &Q,
) -> Result<usize, StoreError> {
    let mut sent = 0;
    loop {
        let page = store.pending_notifications(None, PUBLISH_BATCH)?;
        let Some(last) = page.last().map(NotificationCursor::of) else {
            return Ok(sent);
        };
        let base = meta(store).await?;
        let messages: Vec<NotificationMessage> = page
            .into_iter()
            .map(|intent| NotificationMessage {
                scope: store.scope().clone(),
                meta: MetaWire {
                    url: intent.link.clone(),
                    ..base.clone()
                },
                intent,
            })
            .collect();
        let count = messages.len();
        if let Err(error) = queue.send_batch(messages).await {
            store.defer_outbox(OutboxStream::Notification)?;
            return Err(error);
        }
        store.ack_notifications(&last)?;
        sent += count;
    }
}

/// Fan one message out to its tenant's matching subscriptions, then
/// attempt that tenant's due deliveries.
pub async fn consume<W, T, K>(
    webhooks: &W,
    transport: &T,
    sealer: &K,
    message: NotificationMessage,
    settings: DeliverySettings,
) -> Result<DeliveryStats, StoreError>
where
    W: WebhookStore,
    T: WebhookTransport,
    K: SecretSealer,
{
    let scope = message.scope;
    let subscriptions = webhooks.list_subscriptions(&scope).await?;
    let meta: ExecutionMeta = message.meta.into();
    let (event, deliveries) = plan_fan_out(&scope, &message.intent, &meta, &subscriptions)?;
    webhooks
        .record_event(&scope, event, deliveries, None)
        .await?;
    deliver_due(webhooks, transport, sealer, &scope, settings).await
}

/// How long a message whose fan-out failed waits before the queue tries
/// it again: the native fan-out lease (`DeliverySettings::lease_ms`), after
/// which the native drain retries a failed notification.
pub const FANOUT_RETRY_SECONDS: u32 = 60;
