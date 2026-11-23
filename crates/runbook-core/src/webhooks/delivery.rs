//! Fan-out of committed outbox intents and at-least-once delivery.
//!
//! A notification lease is fanned out once: the event envelope and one
//! delivery row per matching subscription are recorded idempotently, then
//! the lease is acknowledged. Deliveries are then claimed under their own
//! generation-fenced lease, POSTed with no store transaction open, and
//! completed. A crash after the receiver accepted but before completion
//! leaves the lease to expire; the next claim sends the same body, event ID
//! and delivery ID again (receivers deduplicate on the event ID).
use super::*;
use crate::{
    catalog::TenantScope,
    redact::redact_lines,
    webhooks::{AttemptCompletion, DeliveryLease, EventRecord, Subscription, WebhookStore},
};
use runbook_engine::NotificationIntent;
use runbook_model::{RecipientRef, VersionId};
use serde_json::{Value, json};

/// Facts about the run an envelope describes, read outside any session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionMeta {
    pub id: ExecutionId,
    pub title: String,
    pub runbook_id: RunbookId,
    pub version_id: VersionId,
    pub version: Option<u32>,
    pub url: String,
    pub tags: Vec<String>,
}

/// RFC 3339 UTC with milliseconds, from epoch milliseconds.
pub fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// A ULID-shaped ID: 48-bit time prefix plus 80 bits of a SHA-256 over the
/// length-framed parts, so fan-out IDs are stable across retries.
pub fn derived_id(prefix: &str, at_ms: i64, parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let digest = hash.finalize();
    let timestamp = (at_ms.max(0) as u64).min((1_u64 << 48) - 1);
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&timestamp.to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(&digest[..10]);
    let mut value = u128::from_be_bytes(bytes);
    let mut encoded = [b'0'; 26];
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    for byte in encoded.iter_mut().rev() {
        *byte = ALPHABET[(value & 31) as usize];
        value >>= 5;
    }
    format!(
        "{prefix}{}",
        encoded.into_iter().map(char::from).collect::<String>()
    )
}

fn recipients(list: &[RecipientRef]) -> Value {
    Value::Array(
        list.iter()
            .map(|r| match r {
                RecipientRef::User(id) => json!({"type":"user","id":id}),
                RecipientRef::Team(id) => json!({"type":"team","id":id}),
            })
            .collect(),
    )
}

/// The thin, versioned envelope. Only identifiers, recipients, links and
/// type-specific facts: never credentials or secrets.
pub fn envelope(scope: &TenantScope, intent: &NotificationIntent, meta: &ExecutionMeta) -> Value {
    let step = intent.step_run_ids.first();
    let data = match intent.kind.as_str() {
        "agent.work_available" => json!({
            "reason": intent.reason,
            "ready_steps": intent.ready_steps.iter().map(|s| {
                let mut step = json!({
                    "step_run_id": s.step_run_id, "path": s.path, "title": s.title,
                });
                // R0-4: runner agent steps name their agent and group.
                if let (Some(agent), Some(group)) = (&s.agent_id, &s.group_id) {
                    step["agent_id"] = json!(agent);
                    step["group_id"] = json!(group);
                }
                step
            }).collect::<Vec<_>>(),
            "params": intent.params.clone().unwrap_or(Value::Null),
        }),
        "approval.requested" => json!({
            "step_run_id": step,
            "recipients": recipients(&intent.recipients),
            "link": intent.link,
        }),
        // Recipients are the expanded users; `targets` keeps the configured
        // escalation teams/users (absent on intents stored before C4).
        "approval.escalated" | "approval.expired" | "step.escalated" => {
            let mut data = json!({
                "step_run_id": step,
                "recipients": recipients(&intent.recipients),
                "link": intent.link,
            });
            if let Some(targets) = intent
                .detail
                .as_ref()
                .and_then(|detail| detail.get("targets"))
                .and_then(|targets| {
                    serde_json::from_value::<Vec<RecipientRef>>(targets.clone()).ok()
                })
            {
                data["targets"] = recipients(&targets);
            }
            data
        }
        // LANG9: a reminder names the step and who it's waiting on.
        "step.reminder" => json!({
            "step_run_id": step,
            "recipients": recipients(&intent.recipients),
            "link": intent.link,
        }),
        "step.overdue" => json!({
            "step_run_id": step,
            "path": intent.detail.as_ref().and_then(|d| d.get("path")),
            "due_at_ms": intent.detail.as_ref().and_then(|d| d.get("due_at_ms")),
            "recipients": recipients(&intent.recipients),
            "reason": intent.reason,
            "link": intent.link,
        }),
        "execution.started" => json!({
            "status": intent.detail.as_ref().and_then(|detail| detail.get("status")),
        }),
        "step.blocked" => json!({
            "step_run_id": step,
            "recipients": recipients(&intent.recipients),
            "reason": intent.reason,
            "link": intent.link,
        }),
        _ => {
            let mut data = intent.detail.clone().unwrap_or_else(|| json!({}));
            if let (Some(step), Value::Object(map)) = (step, &mut data) {
                map.insert("step_run_id".into(), json!(step));
            }
            data
        }
    };
    json!({
        "id": intent.event_id,
        "type": intent.kind,
        "version": 1,
        "created_at": rfc3339(intent.at_ms),
        "org": scope.org_id,
        "project": scope.project_id,
        "execution": {
            "id": meta.id,
            "title": meta.title,
            "runbook": meta.runbook_id,
            "runbook_version_id": meta.version_id,
            "runbook_version": meta.version,
            "url": meta.url,
            "web_url": runbook_model::execution_web_url(&meta.url, &meta.id),
        },
        "sequence": intent.sequence,
        "data": data,
    })
}

/// The event row and one delivery per matching subscription.
pub fn plan_fan_out(
    scope: &TenantScope,
    intent: &NotificationIntent,
    meta: &ExecutionMeta,
    subscriptions: &[Subscription],
) -> Result<(EventRecord, Vec<DeliveryRecord>), StoreError> {
    let body = serde_json::to_string(&envelope(scope, intent, meta))
        .map_err(|_| StoreError::Backend("webhooks.encode_failed"))?;
    let event = EventRecord {
        event_id: intent.event_id.clone(),
        event_type: intent.kind.clone(),
        execution_id: Some(meta.id.clone()),
        step_run_id: intent.step_run_ids.first().cloned(),
        human: HUMAN_EVENTS.contains(&intent.kind.as_str()),
        created_at_ms: intent.at_ms,
        body,
    };
    let deliveries = first_deliveries(
        scope,
        &event,
        Some(&meta.runbook_id),
        &meta.tags,
        subscriptions,
    )?;
    Ok((event, deliveries))
}

/// One first delivery per matching subscription, with IDs stable across
/// retries of the same fan-out. A subscription created after the event
/// happened does not receive it: the drain fans events out some time after
/// they commit, and what a new webhook receives must not depend on how far
/// behind the drain was when it was created.
fn first_deliveries(
    scope: &TenantScope,
    event: &EventRecord,
    runbook: Option<&RunbookId>,
    tags: &[String],
    subscriptions: &[Subscription],
) -> Result<Vec<DeliveryRecord>, StoreError> {
    let mut deliveries = vec![];
    for subscription in subscriptions {
        if subscription.scope != *scope
            || subscription.created_at_ms > event.created_at_ms
            || !subscription.matches(&event.event_type, runbook, tags)
        {
            continue;
        }
        let id = derived_id(
            "whd_",
            event.created_at_ms,
            &[&event.event_id, subscription.id.as_str()],
        )
        .parse()
        .map_err(|_| StoreError::Backend("webhooks.invalid_id"))?;
        deliveries.push(new_delivery(
            id,
            subscription,
            event,
            None,
            event.created_at_ms,
        ));
    }
    Ok(deliveries)
}

/// The event row and deliveries for a project-level event: the same thin
/// envelope with `execution: null` and `sequence: 0`. Runbook filters
/// apply; tag filters select run tags, so a tag-filtered subscription does
/// not receive project events.
pub fn plan_project_fan_out(
    scope: &TenantScope,
    event: &ProjectEvent,
    subscriptions: &[Subscription],
) -> Result<(EventRecord, Vec<DeliveryRecord>), StoreError> {
    let body = serde_json::to_string(&json!({
        "id": event.event_id,
        "type": event.event_type,
        "version": 1,
        "created_at": rfc3339(event.at_ms),
        "org": scope.org_id,
        "project": scope.project_id,
        "execution": null,
        "sequence": 0,
        "data": event.data,
    }))
    .map_err(|_| StoreError::Backend("webhooks.encode_failed"))?;
    let record = EventRecord {
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        execution_id: None,
        step_run_id: None,
        human: false,
        created_at_ms: event.at_ms,
        body,
    };
    let deliveries = first_deliveries(
        scope,
        &record,
        event.runbook_id.as_ref(),
        &[],
        subscriptions,
    )?;
    Ok((record, deliveries))
}

/// A pending delivery due now with a fresh 24-hour retry window.
pub fn new_delivery(
    id: DeliveryId,
    subscription: &Subscription,
    event: &EventRecord,
    redelivery_of: Option<DeliveryId>,
    at_ms: i64,
) -> DeliveryRecord {
    DeliveryRecord {
        id,
        subscription_id: subscription.id.clone(),
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        execution_id: event.execution_id.clone(),
        redelivery_of,
        state: DeliveryState::Pending,
        created_at_ms: at_ms,
        window_ends_at_ms: at_ms + RETRY_WINDOW_MS,
        next_attempt_at_ms: Some(at_ms),
        attempts: 0,
        failure: None,
    }
}

/// The `webhook.test` event for one subscription; no execution.
pub fn test_event(
    scope: &TenantScope,
    subscription: &Subscription,
    event_id: String,
    at_ms: i64,
) -> Result<EventRecord, StoreError> {
    let body = serde_json::to_string(&json!({
        "id": event_id,
        "type": TEST_EVENT,
        "version": 1,
        "created_at": rfc3339(at_ms),
        "org": scope.org_id,
        "project": scope.project_id,
        "execution": null,
        "sequence": 0,
        "data": {"webhook_id": subscription.id, "message": "Test delivery from The Runbook."},
    }))
    .map_err(|_| StoreError::Backend("webhooks.encode_failed"))?;
    Ok(EventRecord {
        event_id,
        event_type: TEST_EVENT.into(),
        execution_id: None,
        step_run_id: None,
        human: false,
        created_at_ms: at_ms,
        body,
    })
}

/// Host-side secret protection at rest. `context` binds the ciphertext to
/// its subscription so sealed values cannot be swapped between rows.
pub trait SecretSealer {
    fn seal(&self, context: &str, secret: &str) -> Result<SealedSecret, StoreError>;
    fn open(&self, context: &str, sealed: &SealedSecret) -> Result<String, StoreError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// What the host's HTTP POST produced. The host enforces the 10-second
/// timeout, no redirects, and the per-delivery target vetting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportResult {
    Response {
        status: u16,
        latency_ms: u64,
        snippet: String,
    },
    Timeout {
        latency_ms: u64,
    },
    /// Connection or protocol failure (retryable).
    Failed {
        error: &'static str,
        latency_ms: u64,
    },
    /// The target failed vetting at delivery time (not retried).
    Blocked {
        reason: &'static str,
    },
}

pub trait WebhookTransport {
    fn post(&self, request: OutgoingRequest) -> impl Future<Output = TransportResult>;
}

/// Delay before attempt `failed + 1`: 30 s doubling to a 2-hour ceiling.
pub fn backoff_ms(failed_attempts: u32) -> i64 {
    let exponent = failed_attempts.saturating_sub(1).min(16);
    (30_000_i64 << exponent).min(2 * 3_600_000)
}

/// PRD delivery rules: 2xx delivers; timeouts, transport errors, 5xx, 408
/// and 429 retry inside the window; every other status, and a target that
/// fails vetting, fails immediately.
pub fn complete(
    delivery: &DeliveryRecord,
    result: &TransportResult,
    at_ms: i64,
) -> AttemptCompletion {
    let number = delivery.attempts + 1;
    let (status, latency_ms, snippet, error, verdict) = match result {
        TransportResult::Response {
            status,
            latency_ms,
            snippet,
        } => {
            let verdict = match status {
                200..=299 => AttemptOutcome::Delivered,
                408 | 429 | 500..=599 => AttemptOutcome::Retry,
                _ => AttemptOutcome::Failed,
            };
            (
                Some(*status),
                *latency_ms,
                Some(snippet.clone()),
                None,
                verdict,
            )
        }
        TransportResult::Timeout { latency_ms } => (
            None,
            *latency_ms,
            None,
            Some("timeout".to_owned()),
            AttemptOutcome::Retry,
        ),
        TransportResult::Failed { error, latency_ms } => (
            None,
            *latency_ms,
            None,
            Some((*error).to_owned()),
            AttemptOutcome::Retry,
        ),
        TransportResult::Blocked { reason } => (
            None,
            0,
            None,
            Some((*reason).to_owned()),
            AttemptOutcome::Failed,
        ),
    };
    let finished_at = at_ms.saturating_add(i64::try_from(latency_ms).unwrap_or(i64::MAX));
    let (outcome, state, next, failure) = match verdict {
        AttemptOutcome::Delivered => (verdict, DeliveryState::Delivered, None, None),
        AttemptOutcome::Failed => (
            verdict,
            DeliveryState::Failed,
            None,
            Some(match status {
                Some(code) => format!("http_{code}"),
                None => error.clone().unwrap_or_default(),
            }),
        ),
        AttemptOutcome::Retry => {
            let next = finished_at.saturating_add(backoff_ms(number));
            if finished_at >= delivery.window_ends_at_ms {
                (
                    AttemptOutcome::Failed,
                    DeliveryState::Failed,
                    None,
                    Some("retries_exhausted".to_owned()),
                )
            } else {
                (
                    AttemptOutcome::Retry,
                    DeliveryState::Pending,
                    Some(next.min(delivery.window_ends_at_ms)),
                    None,
                )
            }
        }
    };
    let snippet = snippet
        .filter(|s| !s.is_empty())
        .map(|s| redact_lines(vec![s]).join("\n"));
    AttemptCompletion {
        attempt: AttemptRecord {
            delivery_id: delivery.id.clone(),
            number,
            at_ms,
            status,
            latency_ms,
            outcome,
            error,
            snippet,
        },
        state,
        next_attempt_at_ms: next,
        failure,
        health: Some(verdict == AttemptOutcome::Delivered),
    }
}

/// Headers and body for one claimed delivery. The timestamp is the claim's
/// store time in seconds; secrets are opened only for this call.
pub fn signed_request<K: SecretSealer>(
    sealer: &K,
    subscription: &Subscription,
    lease: &DeliveryLease,
) -> Result<OutgoingRequest, StoreError> {
    let at = lease.claimed_at_ms;
    let timestamp = at.div_euclid(1000);
    let context = subscription.id.as_str();
    let mut secrets = vec![sealer.open(context, &subscription.secret)?];
    if let Some(previous) = subscription.active_previous(at) {
        secrets.push(sealer.open(context, previous)?);
    }
    let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let signature = signature_header(&refs, timestamp, lease.body.as_bytes());
    Ok(OutgoingRequest {
        url: subscription.url.clone(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("User-Agent".into(), "TheRunbook-Webhooks/1".into()),
            ("Runbook-Event-Id".into(), lease.delivery.event_id.clone()),
            (
                "Runbook-Event-Type".into(),
                lease.delivery.event_type.clone(),
            ),
            (
                "Runbook-Delivery-Id".into(),
                lease.delivery.id.as_str().to_owned(),
            ),
            ("Runbook-Timestamp".into(), timestamp.to_string()),
            ("Runbook-Signature".into(), signature),
        ],
        body: lease.body.clone(),
    })
}

#[derive(Clone, Copy, Debug)]
pub struct DeliverySettings {
    /// Most deliveries attempted for one tenant in one call.
    pub batch: usize,
    /// One delivery's lease. Each delivery is claimed alone just before its
    /// attempt, so this only has to exceed one attempt (the 10-second POST
    /// timeout plus bounded DNS resolution), never a whole batch.
    pub lease_ms: u64,
}
impl Default for DeliverySettings {
    fn default() -> Self {
        Self {
            batch: 16,
            lease_ms: 60_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeliveryStats {
    pub claimed: usize,
    pub delivered: usize,
    pub retried: usize,
    pub failed: usize,
    /// Completions that did not commit (stale lease or store failure); the
    /// lease expires and the same delivery is attempted again.
    pub errors: usize,
}

/// Claim and attempt every due delivery of one tenant.
pub async fn deliver_due<W, T, K>(
    webhooks: &W,
    transport: &T,
    sealer: &K,
    scope: &TenantScope,
    settings: DeliverySettings,
) -> Result<DeliveryStats, StoreError>
where
    W: WebhookStore,
    T: WebhookTransport,
    K: SecretSealer,
{
    let mut stats = DeliveryStats::default();
    // I1: claim one delivery at a time. Claiming a batch under one lease
    // let later attempts in the batch outlive it, so their results were
    // rejected as stale and they were re-sent with no backoff.
    for _ in 0..settings.batch {
        let Some(lease) = webhooks
            .claim_deliveries(scope, 1, settings.lease_ms)
            .await?
            .pop()
        else {
            break;
        };
        stats.claimed += 1;
        let subscription = webhooks
            .get_subscription(scope, &lease.delivery.subscription_id)
            .await?;
        let completion = match &subscription {
            // Disabled subscriptions receive nothing further, except an
            // explicitly requested test.
            Some(s) if s.enabled || lease.delivery.event_type == TEST_EVENT => {
                match signed_request(sealer, s, &lease) {
                    Ok(request) => {
                        let result = transport.post(request).await;
                        complete(&lease.delivery, &result, lease.claimed_at_ms)
                    }
                    Err(_) => {
                        // An unreadable secret is an operator problem; retry.
                        let mut c = complete(
                            &lease.delivery,
                            &TransportResult::Failed {
                                error: "secret_unavailable",
                                latency_ms: 0,
                            },
                            lease.claimed_at_ms,
                        );
                        c.health = None;
                        c
                    }
                }
            }
            _ => {
                let mut c = complete(
                    &lease.delivery,
                    &TransportResult::Blocked {
                        reason: "subscription_disabled",
                    },
                    lease.claimed_at_ms,
                );
                c.health = None;
                c
            }
        };
        let state = completion.state;
        match webhooks
            .complete_attempt(scope, &lease.delivery.id, lease.generation, completion)
            .await
        {
            Ok(()) => match state {
                DeliveryState::Delivered => stats.delivered += 1,
                DeliveryState::Pending => stats.retried += 1,
                DeliveryState::Failed => stats.failed += 1,
            },
            Err(_) => stats.errors += 1,
        }
    }
    Ok(stats)
}
