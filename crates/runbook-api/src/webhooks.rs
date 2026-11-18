//! C1 `/v1/projects/{p}/webhooks`: subscription CRUD, test, delivery log,
//! redelivery and secret rotation, all under `webhooks:manage` with the
//! project role check (Manage: project admins). Human and service
//! principals use the same path; execution credentials never resolve here.
//!
//! D20: every POST needs `Idempotency-Key`. A replay of test or redeliver
//! returns the saved response. Create and rotate return a secret exactly
//! once, so their replay is `409 webhooks.secret_already_issued` (as D30
//! does for execution credentials); a different payload under the same key
//! is `409 request.idempotency_key_reused`.
use crate::{Request, problem::Response};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    credentials::{AuthError, CredentialStore, Principal, Scope, authorize},
    directory_operations::DirectoryOperations,
    webhooks::{
        self, CATALOG, SUBSCRIPTION_LIMIT, SecretSealer, Subscription, TEST_EVENT, WebhookReplay,
        WebhookStore,
    },
};
use runbook_model::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, future::Future};

/// Host capabilities the portable service cannot have: entropy, the secret
/// sealer, the self-hosted setting and DNS vetting of targets.
pub trait WebhookHost {
    type Sealer: SecretSealer;
    fn sealer(&self) -> Option<&Self::Sealer>;
    fn random32(&self) -> Option<[u8; 32]>;
    /// Which non-public targets this operator allows (self-hosted).
    fn target_policy(&self) -> webhooks::TargetPolicy;
    /// Resolve the URL's host; reject when any address is private,
    /// loopback, link-local or unspecified (unless private targets are
    /// allowed). Returns a stable problem code.
    fn vet_target(&self, url: &str) -> impl Future<Output = Result<(), &'static str>>;
}

#[derive(Clone)]
pub struct WebhookService<D, C, W, H> {
    pub directory: D,
    pub credentials: C,
    pub store: W,
    pub host: H,
}

pub fn recognizes(path: &str) -> bool {
    WEBHOOK_ROUTES
        .iter()
        .any(|r| crate::directory::match_path(r.path, path).is_some())
}

type Failure = (u16, &'static str);
const NOT_FOUND: Failure = (404, "resource.not_found");
const UNAVAILABLE: Failure = (503, "storage.unavailable");
const NAME_MAX: usize = 100;
const LIST_MAX: usize = 50;
const TAG_MAX: usize = 64;

fn store_failure(error: StoreError) -> Failure {
    match error {
        StoreError::NotFound => NOT_FOUND,
        StoreError::InvalidBatch("webhooks.limit_reached") => (409, "webhooks.limit_reached"),
        StoreError::InvalidBatch(_) => (409, "webhooks.conflict"),
        StoreError::Backend(_) => UNAVAILABLE,
    }
}

impl<D, C, W, H> WebhookService<D, C, W, H>
where
    D: DirectoryOperations,
    C: CredentialStore,
    W: WebhookStore,
    H: WebhookHost,
{
    pub async fn handle(&self, r: Request) -> Response {
        match self.dispatch(&r).await {
            Ok((204, _)) => Response::json(204, Value::Null, &r.request_id),
            Ok((status, body)) => Response::json(status, body, &r.request_id),
            Err((status, code)) => Response::error(status, code, &r.request_id),
        }
    }

    async fn authorize(
        &self,
        r: &Request,
        name: &str,
    ) -> Result<(TenantScope, Principal), Failure> {
        let invalid = (401, "auth.invalid_credential");
        let record = r
            .credential_record(&self.credentials)
            .await
            .map_err(|e| e.status())?;
        let now = self
            .directory
            .current_time_ms()
            .await
            .map_err(|_| UNAVAILABLE)?;
        if record.validate().is_err()
            || record.revoked_at.is_some()
            || record.expires_at.is_some_and(|t| t <= now)
        {
            return Err(invalid);
        }
        let snapshot = self
            .directory
            .snapshot(&record.org_id)
            .await
            .map_err(|_| UNAVAILABLE)?;
        // Another organization's project, or none: indistinguishable 404.
        let project = snapshot
            .projects
            .values()
            .find(|p| p.id.as_str() == name || p.slug == name)
            .map(|p| p.id.clone())
            .ok_or(NOT_FOUND)?;
        let principal = authorize(&record, &snapshot, now, &project, Scope::WebhooksManage)
            .map_err(|e| match e {
                AuthError::Forbidden if record.scopes.contains(&Scope::WebhooksManage) => {
                    (403, "auth.forbidden")
                }
                AuthError::Forbidden => (403, "auth.scope_missing"),
                _ => invalid,
            })?;
        self.credentials
            .touch_last_used(&record.org_id, &record.id, now)
            .await
            .map_err(|_| UNAVAILABLE)?;
        Ok((
            TenantScope {
                org_id: record.org_id,
                project_id: project,
            },
            principal,
        ))
    }

    async fn dispatch(&self, r: &Request) -> Result<(u16, Value), Failure> {
        let (route, params) = WEBHOOK_ROUTES
            .iter()
            .find_map(|route| {
                (route.method == r.method)
                    .then(|| crate::directory::match_path(route.path, &r.path).map(|p| (route, p)))
                    .flatten()
            })
            .ok_or(if recognizes(&r.path) {
                (405, "request.method_not_allowed")
            } else {
                NOT_FOUND
            })?;
        let (scope, principal) = self.authorize(r, &params["p"]).await?;
        let id = params
            .get("id")
            .map(|id| id.parse::<WebhookId>().map_err(|_| NOT_FOUND))
            .transpose()?;
        match (route.method, route.path, id) {
            ("GET", "/v1/projects/{p}/webhooks", None) => {
                let items = self
                    .store
                    .list_subscriptions(&scope)
                    .await
                    .map_err(store_failure)?
                    .iter()
                    .map(Subscription::view)
                    .collect();
                Ok((200, json!(WebhookPage { items })))
            }
            ("POST", "/v1/projects/{p}/webhooks", None) => self.create(r, &scope, &principal).await,
            (_, _, Some(id)) => {
                let subscription = self
                    .store
                    .get_subscription(&scope, &id)
                    .await
                    .map_err(store_failure)?
                    .ok_or(NOT_FOUND)?;
                match (route.method, route.path) {
                    ("GET", "/v1/projects/{p}/webhooks/{id}") => {
                        Ok((200, json!(subscription.view())))
                    }
                    ("PATCH", _) => self.update(r, &scope, subscription).await,
                    ("DELETE", _) => {
                        if self
                            .store
                            .delete_subscription(&scope, &id)
                            .await
                            .map_err(store_failure)?
                        {
                            Ok((204, Value::Null))
                        } else {
                            Err(NOT_FOUND)
                        }
                    }
                    ("POST", "/v1/projects/{p}/webhooks/{id}/test") => {
                        self.test(r, &scope, &principal, &subscription).await
                    }
                    ("GET", "/v1/projects/{p}/webhooks/{id}/deliveries") => {
                        self.deliveries(r, &scope, &id).await
                    }
                    ("POST", "/v1/projects/{p}/webhooks/{id}/deliveries/{d}/redeliver") => {
                        let delivery = params["d"].parse::<DeliveryId>().map_err(|_| NOT_FOUND)?;
                        self.redeliver(r, &scope, &principal, &subscription, &delivery)
                            .await
                    }
                    ("POST", "/v1/projects/{p}/webhooks/{id}/rotate-secret") => {
                        self.rotate(r, &scope, &principal, subscription).await
                    }
                    _ => Err((405, "request.method_not_allowed")),
                }
            }
            _ => Err(NOT_FOUND),
        }
    }

    /// D20 identity: the namespaced principal and key, plus the body hash.
    /// A saved result for this key decides the response before any work.
    async fn replay(
        &self,
        r: &Request,
        scope: &TenantScope,
        principal: &Principal,
        operation: &[&str],
    ) -> Result<(WebhookReplay, Option<Value>), Failure> {
        let key = r
            .idempotency_key
            .as_deref()
            .filter(|k| !k.trim().is_empty() && k.len() <= 256)
            .ok_or((428, "request.idempotency_key_required"))?;
        let who = match principal {
            Principal::Human { user_id } => format!("human:{user_id}"),
            Principal::Agent { sponsor } => format!("agent:{sponsor}"),
            Principal::Service { token_id, .. } => format!("service:{token_id}"),
        };
        let mut framed: Vec<Value> = operation.iter().map(|p| json!(p)).collect();
        framed.push(json!(who));
        framed.push(json!(key));
        let key = runbook_core::authoring::hex(&Sha256::digest(
            serde_json::to_vec(&framed).map_err(|_| (422, "request.invalid"))?,
        ));
        let request_hash: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&r.body).map_err(|_| (422, "request.invalid"))?)
                .into();
        let now = self.store.webhook_time_ms().await.map_err(store_failure)?;
        let saved = self
            .store
            .find_replay(scope, &key)
            .await
            .map_err(store_failure)?;
        let fresh = WebhookReplay {
            key,
            request_hash,
            response: Value::Null,
            created_at_ms: now,
        };
        match saved {
            None => Ok((fresh, None)),
            Some(saved) if saved.request_hash != request_hash => {
                Err((409, "request.idempotency_key_reused"))
            }
            Some(saved) if saved.response.is_null() => Err((409, "webhooks.secret_already_issued")),
            Some(saved) => Ok((fresh, Some(saved.response))),
        }
    }

    /// A racing retry lost the replay-key insert: answer from the winner.
    async fn raced(&self, scope: &TenantScope, replay: &WebhookReplay) -> Failure {
        match self.store.find_replay(scope, &replay.key).await {
            Ok(Some(saved)) if saved.request_hash != replay.request_hash => {
                (409, "request.idempotency_key_reused")
            }
            Ok(Some(_)) => (409, "webhooks.secret_already_issued"),
            _ => (409, "webhooks.conflict"),
        }
    }

    fn new_secret(&self, id: &WebhookId) -> Result<(String, webhooks::SealedSecret), Failure> {
        let sealer = self.host.sealer().ok_or((503, "webhooks.not_configured"))?;
        let random = self.host.random32().ok_or((503, "server.entropy"))?;
        let secret = webhooks::encode_webhook_secret(random);
        let sealed = sealer
            .seal(id.as_str(), &secret)
            .map_err(|_| (503, "webhooks.not_configured"))?;
        Ok((secret, sealed))
    }

    async fn check_url(&self, url: &str) -> Result<(), Failure> {
        webhooks::parse_target(url, self.host.target_policy()).map_err(|c| (422, c))?;
        self.host.vet_target(url).await.map_err(|c| (422, c))
    }

    async fn create(
        &self,
        r: &Request,
        scope: &TenantScope,
        principal: &Principal,
    ) -> Result<(u16, Value), Failure> {
        let (replay, saved) = self
            .replay(
                r,
                scope,
                principal,
                &["webhooks.create", scope.project_id.as_str()],
            )
            .await?;
        if saved.is_some() {
            return Err((409, "webhooks.secret_already_issued"));
        }
        let request: CreateWebhookRequest =
            serde_json::from_value(r.body.clone()).map_err(|_| (422, "request.invalid"))?;
        let fields = Fields {
            name: request.name,
            url: request.url,
            events: request.events,
            runbooks: request.runbooks,
            tags: request.tags,
        };
        fields.validate()?;
        self.check_url(&fields.url).await?;
        let id: WebhookId = format!("whk_{}", r.new_id)
            .parse()
            .map_err(|_| (503, "server.invalid_id"))?;
        let (secret, sealed) = self.new_secret(&id)?;
        let now = replay.created_at_ms;
        let subscription = Subscription {
            id,
            scope: scope.clone(),
            name: fields.name,
            url: fields.url,
            events: fields.events,
            runbooks: fields.runbooks,
            tags: fields.tags,
            enabled: request.enabled,
            secret: sealed,
            previous_secret: None,
            created_at_ms: now,
            updated_at_ms: now,
            secret_rotated_at_ms: now,
            failing_since_ms: None,
            last_failure_at_ms: None,
            disabled_reason: None,
            disabled_at_ms: None,
            revision: 0,
            notice: None,
        };
        let view = subscription.view();
        match self
            .store
            .create_subscription(
                scope,
                subscription,
                SUBSCRIPTION_LIMIT,
                Some(replay.clone()),
            )
            .await
        {
            Ok(()) => Ok((
                201,
                json!(WebhookSecretView {
                    webhook: view,
                    secret
                }),
            )),
            Err(StoreError::InvalidBatch("webhooks.replay_exists")) => {
                Err(self.raced(scope, &replay).await)
            }
            Err(e) => Err(store_failure(e)),
        }
    }

    async fn update(
        &self,
        r: &Request,
        scope: &TenantScope,
        mut subscription: Subscription,
    ) -> Result<(u16, Value), Failure> {
        let request: UpdateWebhookRequest =
            serde_json::from_value(r.body.clone()).map_err(|_| (422, "request.invalid"))?;
        let url_changed = request.url.as_ref().is_some_and(|u| *u != subscription.url);
        let fields = Fields {
            name: request.name.unwrap_or(subscription.name.clone()),
            url: request.url.unwrap_or(subscription.url.clone()),
            events: request.events.unwrap_or(subscription.events.clone()),
            runbooks: request.runbooks.unwrap_or(subscription.runbooks.clone()),
            tags: request.tags.unwrap_or(subscription.tags.clone()),
        };
        fields.validate()?;
        if url_changed {
            self.check_url(&fields.url).await?;
        }
        let now = self.store.webhook_time_ms().await.map_err(store_failure)?;
        if request.enabled == Some(true) && !subscription.enabled {
            // Re-enabling starts a fresh health record and acknowledges
            // the admin notice.
            subscription.reenable();
        }
        subscription.enabled = request.enabled.unwrap_or(subscription.enabled);
        subscription.name = fields.name;
        subscription.url = fields.url;
        subscription.events = fields.events;
        subscription.runbooks = fields.runbooks;
        subscription.tags = fields.tags;
        subscription.updated_at_ms = now;
        let view = subscription.view();
        self.store
            .save_subscription(scope, subscription, None)
            .await
            .map_err(store_failure)?;
        Ok((200, json!(view)))
    }

    async fn test(
        &self,
        r: &Request,
        scope: &TenantScope,
        principal: &Principal,
        subscription: &Subscription,
    ) -> Result<(u16, Value), Failure> {
        let (mut replay, saved) = self
            .replay(
                r,
                scope,
                principal,
                &["webhooks.test", subscription.id.as_str()],
            )
            .await?;
        if let Some(saved) = saved {
            return Ok((202, saved));
        }
        let now = replay.created_at_ms;
        let event = webhooks::test_event(scope, subscription, format!("evt_{}", r.new_id), now)
            .map_err(store_failure)?;
        let id: DeliveryId = format!("whd_{}", r.new_id)
            .parse()
            .map_err(|_| (503, "server.invalid_id"))?;
        let delivery = webhooks::new_delivery(id, subscription, &event, None, now);
        let view = delivery.view(&[]);
        replay.response = json!(view);
        match self
            .store
            .record_event(scope, event, vec![delivery], Some(replay.clone()))
            .await
        {
            Ok(true) => Ok((202, json!(view))),
            Ok(false) => Err((409, "webhooks.conflict")),
            Err(StoreError::InvalidBatch("webhooks.replay_exists")) => {
                Err(self.raced(scope, &replay).await)
            }
            Err(e) => Err(store_failure(e)),
        }
    }

    async fn deliveries(
        &self,
        r: &Request,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<(u16, Value), Failure> {
        let bad = (400, "request.invalid_query");
        let mut limit = 50;
        let mut before = None;
        for (key, value) in &r.query {
            match key.as_str() {
                "limit" => {
                    limit = value.parse().map_err(|_| bad)?;
                    if !(1..=100).contains(&limit) {
                        return Err(bad);
                    }
                }
                "cursor" => {
                    let (at, delivery) = value.split_once('.').ok_or(bad)?;
                    before = Some((
                        at.parse::<i64>().map_err(|_| bad)?,
                        delivery.parse::<DeliveryId>().map_err(|_| bad)?,
                    ));
                }
                _ => return Err(bad),
            }
        }
        let rows = self
            .store
            .deliveries(scope, id, limit + 1, before)
            .await
            .map_err(store_failure)?;
        let next_cursor = (rows.len() > limit)
            .then(|| &rows[limit - 1].0)
            .map(|d| format!("{}.{}", d.created_at_ms, d.id));
        let items = rows
            .iter()
            .take(limit)
            .map(|(d, attempts)| d.view(attempts))
            .collect();
        Ok((200, json!(DeliveryPage { items, next_cursor })))
    }

    async fn redeliver(
        &self,
        r: &Request,
        scope: &TenantScope,
        principal: &Principal,
        subscription: &Subscription,
        original: &DeliveryId,
    ) -> Result<(u16, Value), Failure> {
        let (mut replay, saved) = self
            .replay(
                r,
                scope,
                principal,
                &["webhooks.redeliver", original.as_str()],
            )
            .await?;
        if let Some(saved) = saved {
            return Ok((202, saved));
        }
        let (record, _) = self
            .store
            .delivery(scope, original)
            .await
            .map_err(store_failure)?
            .filter(|(d, _)| d.subscription_id == subscription.id)
            .ok_or(NOT_FOUND)?;
        let event = self
            .store
            .get_event(scope, &record.event_id)
            .await
            .map_err(store_failure)?
            .ok_or(NOT_FOUND)?;
        let id: DeliveryId = format!("whd_{}", r.new_id)
            .parse()
            .map_err(|_| (503, "server.invalid_id"))?;
        let delivery = webhooks::new_delivery(
            id,
            subscription,
            &event,
            Some(original.clone()),
            replay.created_at_ms,
        );
        let view = delivery.view(&[]);
        replay.response = json!(view);
        match self
            .store
            .add_delivery(scope, delivery, Some(replay.clone()))
            .await
        {
            Ok(()) => Ok((202, json!(view))),
            Err(StoreError::InvalidBatch("webhooks.replay_exists")) => {
                Err(self.raced(scope, &replay).await)
            }
            Err(e) => Err(store_failure(e)),
        }
    }

    async fn rotate(
        &self,
        r: &Request,
        scope: &TenantScope,
        principal: &Principal,
        mut subscription: Subscription,
    ) -> Result<(u16, Value), Failure> {
        let (replay, saved) = self
            .replay(
                r,
                scope,
                principal,
                &["webhooks.rotate", subscription.id.as_str()],
            )
            .await?;
        if saved.is_some() {
            return Err((409, "webhooks.secret_already_issued"));
        }
        let (secret, sealed) = self.new_secret(&subscription.id)?;
        subscription.rotate(sealed, replay.created_at_ms);
        let view = subscription.view();
        match self
            .store
            .save_subscription(scope, subscription, Some(replay.clone()))
            .await
        {
            Ok(()) => Ok((
                200,
                json!(WebhookSecretView {
                    webhook: view,
                    secret
                }),
            )),
            Err(StoreError::InvalidBatch("webhooks.replay_exists")) => {
                Err(self.raced(scope, &replay).await)
            }
            Err(e) => Err(store_failure(e)),
        }
    }
}

struct Fields {
    name: String,
    url: String,
    events: Vec<String>,
    runbooks: Vec<RunbookId>,
    tags: Vec<String>,
}
impl Fields {
    fn validate(&self) -> Result<(), Failure> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control)
        {
            return Err((422, "webhooks.name_invalid"));
        }
        let all = self.events.iter().any(|e| e == webhooks::ALL_EVENTS);
        let mut seen = BTreeMap::new();
        if self.events.is_empty()
            || (all && self.events.len() != 1)
            || self.events.iter().any(|e| {
                seen.insert(e, ()).is_some()
                    || (e != webhooks::ALL_EVENTS
                        && (!CATALOG.contains(&e.as_str()) || e == TEST_EVENT))
            })
        {
            return Err((422, "webhooks.events_invalid"));
        }
        if self.runbooks.len() > LIST_MAX {
            return Err((422, "webhooks.runbooks_invalid"));
        }
        if self.tags.len() > LIST_MAX
            || self
                .tags
                .iter()
                .any(|t| t.is_empty() || t.len() > TAG_MAX || t.chars().any(char::is_control))
        {
            return Err((422, "webhooks.tags_invalid"));
        }
        Ok(())
    }
}
