//! C1 generic webhooks: subscriptions, the thin event envelope, signing,
//! target safety rules and the durable delivery outbox.
//!
//! Portable by design (wasm32): no clock, entropy, DNS, sockets or TLS live
//! here. Hosts supply entropy for secrets, a [`SecretSealer`] for secrets at
//! rest and a [`WebhookTransport`] that resolves, vets and POSTs. Every
//! timestamp comes from the store clock, like the execution store.
//!
//! Secrets: a subscription's signing secret (`whsec_…`) is generated from host
//! entropy, returned once on create and on rotate, and stored only in sealed
//! (encrypted) form. Signing needs the raw secret, so the delivery worker
//! opens it transiently; it never enters a view, a body, a log or an error.
mod delivery;
pub mod memory;
mod signing;
mod target;
pub use delivery::*;
pub use signing::*;
pub use target::*;

use crate::{StoreError, catalog::TenantScope};
use runbook_model::{
    AttemptOutcome, DeliveryAttemptView, DeliveryId, DeliveryState, DeliveryView, ExecutionId,
    RunbookId, StepRunId, UndeliveredNotification, WebhookId, WebhookNotice, WebhookView,
};
use serde::{Deserialize, Serialize};
use std::{fmt, future::Future};

/// Event catalog v1 (PRD section 9). `memory.proposed` and `memory.saved`
/// come from the execution's `MemoryProposed` and `MemorySaved` events
/// (MEM3, D40; MEM6);
/// `execution.queued` and `runbook.published` flow from their own durable
/// outbox rows.
pub const CATALOG: &[&str] = &[
    "agent.work_available",
    "approval.requested",
    "approval.escalated",
    "approval.expired",
    "approval.decided",
    "lease.revoked",
    "step.blocked",
    "step.completed",
    "step.reminder",
    "step.escalated",
    "execution.queued",
    "execution.started",
    "execution.completed",
    "memory.proposed",
    "memory.saved",
    "memory.edited",
    "runbook.published",
    // R0-4: runner events, reported by the runner for the organization.
    "runner.budget_threshold",
    "runner.probe_succeeded",
    "runner.secret_in_response",
    "runner.binding_mismatch",
    "runner.release_denied",
    TEST_EVENT,
];
/// Events handed to people. Reaching no subscriber is shown on the execution.
pub const HUMAN_EVENTS: &[&str] = &[
    "approval.requested",
    "approval.escalated",
    "approval.expired",
    "step.blocked",
    "step.reminder",
    "step.escalated",
];
pub const TEST_EVENT: &str = "webhook.test";
/// Selects every event type in `events`.
pub const ALL_EVENTS: &str = "*";
pub const SUBSCRIPTION_LIMIT: usize = 20;
const DAY_MS: i64 = 86_400_000;
/// The previous secret keeps signing for this long after a rotation.
pub const ROTATION_OVERLAP_MS: i64 = DAY_MS;
/// Retries stop this long after a delivery (or redelivery) was created.
pub const RETRY_WINDOW_MS: i64 = DAY_MS;
/// Continuous failure for this long disables a subscription.
pub const DISABLE_AFTER_MS: i64 = 3 * DAY_MS;
/// A failing streak survives quiet gaps up to two retry windows (F21).
pub const STREAK_GAP_MS: i64 = 2 * RETRY_WINDOW_MS;
/// Delivery log retention: the delivery worker purges older rows.
pub const DELIVERY_LOG_RETENTION_MS: i64 = 90 * DAY_MS;
/// Most delivery-log rows one purge call removes for one tenant.
pub const PURGE_BATCH: usize = 500;
/// The admin notice kind recorded when the system disables a subscription.
pub const DISABLED_NOTICE: &str = "webhook.disabled";
/// A 2xx within this time counts as delivered.
pub const DELIVERY_TIMEOUT_MS: u64 = 10_000;

/// A secret in its at-rest form. Debug never prints it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SealedSecret(pub String);
impl fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealedSecret([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousSecret {
    pub sealed: SealedSecret,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: WebhookId,
    pub scope: TenantScope,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub runbooks: Vec<RunbookId>,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub secret: SealedSecret,
    pub previous_secret: Option<PreviousSecret>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub secret_rotated_at_ms: i64,
    pub failing_since_ms: Option<i64>,
    /// The latest failed attempt of the current streak (F21). A failure more
    /// than `STREAK_GAP_MS` after it starts a new streak.
    #[serde(default)]
    pub last_failure_at_ms: Option<i64>,
    pub disabled_reason: Option<String>,
    pub disabled_at_ms: Option<i64>,
    /// Compare-and-set token: every stored change (API edit, rotation or
    /// health change) increments it, and a save must carry the revision it
    /// read (M1).
    #[serde(default)]
    pub revision: u64,
    /// Durable admin notice, set when the system disables the subscription
    /// and cleared when an admin re-enables it.
    #[serde(default)]
    pub notice: Option<WebhookNotice>,
}
impl Subscription {
    pub fn view(&self) -> WebhookView {
        WebhookView {
            id: self.id.clone(),
            project_id: self.scope.project_id.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            events: self.events.clone(),
            runbooks: self.runbooks.clone(),
            tags: self.tags.clone(),
            enabled: self.enabled,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            secret_rotated_at_ms: self.secret_rotated_at_ms,
            previous_secret_expires_at_ms: self.previous_secret.as_ref().map(|p| p.expires_at_ms),
            failing_since_ms: self.failing_since_ms,
            disabled_reason: self.disabled_reason.clone(),
            disabled_at_ms: self.disabled_at_ms,
            admin_notice: self.notice.clone(),
        }
    }
    /// Fan-out filter. `webhook.test` is never fanned out: it is addressed.
    pub fn matches(&self, event_type: &str, runbook: Option<&RunbookId>, tags: &[String]) -> bool {
        self.enabled
            && event_type != TEST_EVENT
            && self
                .events
                .iter()
                .any(|e| e == ALL_EVENTS || e == event_type)
            && (self.runbooks.is_empty() || runbook.is_some_and(|r| self.runbooks.contains(r)))
            && (self.tags.is_empty() || tags.iter().any(|t| self.tags.contains(t)))
    }
    /// The previous secret while its overlap window is open at `at_ms`.
    pub fn active_previous(&self, at_ms: i64) -> Option<&SealedSecret> {
        self.previous_secret
            .as_ref()
            .filter(|p| p.expires_at_ms > at_ms)
            .map(|p| &p.sealed)
    }
    /// Replace the secret; the old one signs alongside for 24 hours.
    pub fn rotate(&mut self, sealed: SealedSecret, at_ms: i64) {
        let old = std::mem::replace(&mut self.secret, sealed);
        // An imported subscription has no secret (D3): nothing to overlap.
        self.previous_secret = (old.0 != crate::portable::UNSET_SECRET).then(|| PreviousSecret {
            sealed: old,
            expires_at_ms: at_ms + ROTATION_OVERLAP_MS,
        });
        self.secret_rotated_at_ms = at_ms;
        self.updated_at_ms = at_ms;
    }
    /// Health after one attempt: success clears the failing streak; failure
    /// starts it, and three days of continuous failure disable the
    /// subscription (recorded as `disabled_reason: delivery_failing`) and
    /// leave a durable admin notice naming the last failure.
    pub fn record_attempt(&mut self, ok: bool, at_ms: i64, failure: Option<&str>) {
        if ok {
            self.failing_since_ms = None;
            self.last_failure_at_ms = None;
            return;
        }
        // Continuous means the receiver kept failing while it was tried: a
        // failure after a quiet gap of more than two retry windows (no
        // attempt at all, so nothing showed it was down) restarts the
        // streak (F21).
        if self
            .last_failure_at_ms
            .is_some_and(|last| at_ms - last > STREAK_GAP_MS)
        {
            self.failing_since_ms = None;
        }
        self.last_failure_at_ms = Some(at_ms);
        let since = *self.failing_since_ms.get_or_insert(at_ms);
        if self.enabled && at_ms - since >= DISABLE_AFTER_MS {
            self.enabled = false;
            self.disabled_reason = Some("delivery_failing".into());
            self.disabled_at_ms = Some(at_ms);
            self.updated_at_ms = at_ms;
            self.notice = Some(WebhookNotice {
                kind: DISABLED_NOTICE.into(),
                message: format!(
                    "Disabled after failing continuously for three days. \
                     Fix the receiver, then re-enable \"{}\".",
                    self.name
                ),
                at_ms,
                failing_since_ms: since,
                last_failure: failure.map(str::to_owned),
            });
        }
    }
    /// An admin turning the subscription back on starts a fresh health
    /// record and acknowledges the notice.
    pub fn reenable(&mut self) {
        self.enabled = true;
        self.failing_since_ms = None;
        self.last_failure_at_ms = None;
        self.disabled_reason = None;
        self.disabled_at_ms = None;
        self.notice = None;
    }
}

/// One envelope, shared by every delivery of the event (and redeliveries).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub event_id: String,
    pub event_type: String,
    pub execution_id: Option<ExecutionId>,
    pub step_run_id: Option<StepRunId>,
    pub human: bool,
    pub created_at_ms: i64,
    /// Exact JSON body sent to receivers; identical on every attempt.
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub id: DeliveryId,
    pub subscription_id: WebhookId,
    pub event_id: String,
    pub event_type: String,
    pub execution_id: Option<ExecutionId>,
    pub redelivery_of: Option<DeliveryId>,
    pub state: DeliveryState,
    pub created_at_ms: i64,
    pub window_ends_at_ms: i64,
    pub next_attempt_at_ms: Option<i64>,
    pub attempts: u32,
    pub failure: Option<String>,
}
impl DeliveryRecord {
    pub fn view(&self, attempts: &[AttemptRecord]) -> DeliveryView {
        DeliveryView {
            id: self.id.clone(),
            webhook_id: self.subscription_id.clone(),
            event_id: self.event_id.clone(),
            event_type: self.event_type.clone(),
            execution_id: self.execution_id.clone(),
            state: self.state,
            created_at_ms: self.created_at_ms,
            next_attempt_at_ms: self.next_attempt_at_ms,
            redelivery_of: self.redelivery_of.clone(),
            failure: self.failure.clone(),
            attempts: attempts
                .iter()
                .map(|a| DeliveryAttemptView {
                    number: a.number,
                    at_ms: a.at_ms,
                    status: a.status,
                    latency_ms: a.latency_ms,
                    outcome: a.outcome,
                    error: a.error.clone(),
                    response_snippet: a.snippet.clone(),
                })
                .collect(),
        }
    }
}

/// One logged attempt: status, latency and a redacted response snippet only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub delivery_id: DeliveryId,
    pub number: u32,
    pub at_ms: i64,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub outcome: AttemptOutcome,
    pub error: Option<String>,
    pub snippet: Option<String>,
}
impl AttemptRecord {
    /// How a failed attempt failed: `http_<status>` or the transport error.
    pub fn failure_label(&self) -> Option<String> {
        match (self.outcome, self.status) {
            (AttemptOutcome::Delivered, _) => None,
            (_, Some(code)) => Some(format!("http_{code}")),
            (_, None) => self.error.clone(),
        }
    }
}

/// A claimed delivery. The generation fences the completion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryLease {
    pub delivery: DeliveryRecord,
    pub generation: u64,
    /// Store clock at claim; attempts are timed and signed from it.
    pub claimed_at_ms: i64,
    pub body: String,
}

/// The atomic result of one attempt: attempt row, delivery state and the
/// subscription's health, written together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptCompletion {
    pub attempt: AttemptRecord,
    pub state: DeliveryState,
    pub next_attempt_at_ms: Option<i64>,
    pub failure: Option<String>,
    /// Whether this attempt counts toward the subscription's health at all
    /// (a stale-subscription skip does not), and if so whether it succeeded.
    pub health: Option<bool>,
}

/// D20 replay row for webhook POSTs. `response` is null for secret-bearing
/// operations: their retry is `409 webhooks.secret_already_issued`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebhookReplay {
    pub key: String,
    pub request_hash: [u8; 32],
    pub response: serde_json::Value,
    pub created_at_ms: i64,
}

/// Tenant-scoped webhook storage. Every call filters by `scope`; a foreign
/// subscription or delivery reads as absent. Implementations keep each call
/// atomic; futures need not be Send.
pub trait WebhookStore {
    fn webhook_time_ms(&self) -> impl Future<Output = Result<i64, StoreError>>;
    fn list_subscriptions(
        &self,
        scope: &TenantScope,
    ) -> impl Future<Output = Result<Vec<Subscription>, StoreError>>;
    fn get_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> impl Future<Output = Result<Option<Subscription>, StoreError>>;
    /// Insert unless the project already has `limit` subscriptions
    /// (`InvalidBatch("webhooks.limit_reached")`), with the replay row.
    fn create_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        limit: usize,
        replay: Option<WebhookReplay>,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// Replace an existing subscription (NotFound when gone) if its stored
    /// revision still equals `subscription.revision`, storing revision + 1;
    /// otherwise `InvalidBatch("webhooks.conflict")`.
    fn save_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        replay: Option<WebhookReplay>,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// Delete with its deliveries and attempts. False when absent.
    fn delete_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    /// Replay rows older than 24 hours read as absent (D20).
    fn find_replay(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> impl Future<Output = Result<Option<WebhookReplay>, StoreError>>;
    /// Idempotent on the event ID: a second call for a recorded event writes
    /// nothing and returns false. Deliveries are unique per (event,
    /// subscription) unless they are redeliveries.
    fn record_event(
        &self,
        scope: &TenantScope,
        event: EventRecord,
        deliveries: Vec<DeliveryRecord>,
        replay: Option<WebhookReplay>,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    /// A redelivery row for an already recorded event.
    fn add_delivery(
        &self,
        scope: &TenantScope,
        delivery: DeliveryRecord,
        replay: Option<WebhookReplay>,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn get_event(
        &self,
        scope: &TenantScope,
        event_id: &str,
    ) -> impl Future<Output = Result<Option<EventRecord>, StoreError>>;
    /// Pending deliveries due at the store clock and not currently leased.
    fn claim_deliveries(
        &self,
        scope: &TenantScope,
        limit: usize,
        lease_ms: u64,
    ) -> impl Future<Output = Result<Vec<DeliveryLease>, StoreError>>;
    /// Fenced by generation and lease deadline
    /// (`InvalidBatch("webhooks.stale_lease")`).
    fn complete_attempt(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
        generation: u64,
        completion: AttemptCompletion,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// Newest first, strictly older than `before` (created_at_ms, id).
    fn deliveries(
        &self,
        scope: &TenantScope,
        subscription: &WebhookId,
        limit: usize,
        before: Option<(i64, DeliveryId)>,
    ) -> impl Future<Output = Result<Vec<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError>>;
    fn delivery(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
    ) -> impl Future<Output = Result<Option<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError>>;
    /// Every recorded event of one execution with its delivery states.
    fn execution_events(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<Vec<(EventRecord, Vec<DeliveryState>)>, StoreError>>;
    /// Delivery-log retention, bounded by `limit` rows of each kind:
    /// finished deliveries created before `before_ms` (with their attempts),
    /// except deliveries of human events, whose states stay as the
    /// execution's notified/not-delivered verdict; then attempts logged
    /// before `before_ms`. Pending deliveries are never touched. Returns
    /// how many rows went.
    fn purge_delivery_log(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> impl Future<Output = Result<usize, StoreError>>;
}

/// The delivery worker's retention pass for one tenant: drop delivery-log
/// rows older than [`DELIVERY_LOG_RETENTION_MS`] at the store clock.
pub async fn purge_expired_log<W: WebhookStore>(
    webhooks: &W,
    scope: &TenantScope,
    limit: usize,
) -> Result<usize, StoreError> {
    let now = webhooks.webhook_time_ms().await?;
    webhooks
        .purge_delivery_log(scope, now - DELIVERY_LOG_RETENTION_MS, limit)
        .await
}

/// An event with no execution (`runbook.published`), recorded in the
/// project's outbox in the same transaction as the fact it reports and
/// fanned out by the drain like an execution's intents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEvent {
    pub event_id: String,
    pub event_type: String,
    pub at_ms: i64,
    /// Matched against subscription runbook filters.
    pub runbook_id: Option<RunbookId>,
    /// The envelope's `data`; never source, credentials or secrets.
    pub data: serde_json::Value,
}

/// `runbook.published` for a newly numbered version: identifiers only,
/// never the source.
pub fn published_event(
    runbook_id: &RunbookId,
    slug: &str,
    version: &runbook_model::PublishedVersionView,
    at_ms: i64,
) -> ProjectEvent {
    ProjectEvent {
        event_id: derived_id(
            "evt_",
            at_ms,
            &[version.runbook_version_id.as_str(), "runbook.published"],
        ),
        event_type: "runbook.published".into(),
        at_ms,
        runbook_id: Some(runbook_id.clone()),
        data: serde_json::json!({
            "runbook": runbook_id,
            "runbook_slug": slug,
            "runbook_version_id": version.runbook_version_id,
            "runbook_version": version.runbook_version,
        }),
    }
}

/// The project-level outbox, kept by the execution/catalog store so a row
/// commits with the fact it reports. Tenant-scoped by the store.
pub trait ProjectOutbox {
    /// Oldest first.
    fn pending_project_events(
        &self,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ProjectEvent>, StoreError>>;
    /// Remove a fanned-out row; absent rows are fine (another worker).
    fn ack_project_event(&self, event_id: &str) -> impl Future<Output = Result<(), StoreError>>;
}

/// Fan pending project events out to matching subscriptions (idempotent on
/// the event ID), then remove them from the outbox. A crash between the two
/// re-runs harmlessly.
pub async fn process_project_events<S: ProjectOutbox, W: WebhookStore>(
    store: &S,
    webhooks: &W,
    scope: &TenantScope,
    limit: usize,
) -> Result<usize, StoreError> {
    let pending = store.pending_project_events(limit).await?;
    if pending.is_empty() {
        return Ok(0);
    }
    let subscriptions = webhooks.list_subscriptions(scope).await?;
    for event in &pending {
        let (record, deliveries) = plan_project_fan_out(scope, event, &subscriptions)?;
        webhooks
            .record_event(scope, record, deliveries, None)
            .await?;
        store.ack_project_event(&event.event_id).await?;
    }
    Ok(pending.len())
}

/// Human events that reached nobody: no matching subscriber, or every
/// delivery (including redeliveries) failed. Pending deliveries are not yet
/// a verdict, and one success anywhere counts as notified.
pub fn undelivered(events: &[(EventRecord, Vec<DeliveryState>)]) -> Vec<UndeliveredNotification> {
    events
        .iter()
        .filter(|(event, _)| event.human)
        .filter_map(|(event, states)| {
            let reason = if states.is_empty() {
                "no_subscriber"
            } else if states.iter().all(|s| *s == DeliveryState::Failed) {
                "delivery_failed"
            } else {
                return None;
            };
            Some(UndeliveredNotification {
                event_id: event.event_id.clone(),
                event_type: event.event_type.clone(),
                step_run_id: event.step_run_id.clone(),
                reason: reason.into(),
            })
        })
        .collect()
}

/// Webhooks for one execution read: the undelivered human notices.
pub async fn undelivered_for<W: WebhookStore>(
    webhooks: &W,
    scope: &TenantScope,
    execution: &ExecutionId,
) -> Result<Vec<UndeliveredNotification>, StoreError> {
    Ok(undelivered(
        &webhooks.execution_events(scope, execution).await?,
    ))
}
