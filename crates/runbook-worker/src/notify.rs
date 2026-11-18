//! The Workers side of notification delivery (CF4), wasm32 only: the
//! Queues producer the ExecutionDO publishes through, the fetch transport,
//! and the queue consumer. The logic is `crate::delivery` (portable).
//!
//! CF3: the consumer and the redelivery pass run over D1's webhook tables
//! and the AES-GCM sealer (`WEBHOOK_SECRET_KEY`).
use crate::delivery::{FANOUT_RETRY_SECONDS, NotificationMessage, NotificationQueue, consume};
use runbook_core::{
    StoreError,
    webhooks::{
        DeliverySettings, OutgoingRequest, TargetPolicy, TransportResult, WebhookTransport,
        address_allowed, parse_target,
    },
};
use worker::{
    AbortController, Delay, Env, Fetch, MessageBatch, MessageExt, Method, QueueRetryOptionsBuilder,
    Request, RequestInit, RequestRedirect,
};

/// The Queues binding notifications are published to.
pub const BINDING: &str = "NOTIFICATIONS";
/// Bytes of a response body kept in the delivery log (as native).
const SNIPPET_BYTES: usize = 512;
/// How long the response snippet may take to arrive after the headers.
const SNIPPET_WAIT_MS: u64 = 2_000;
/// The POST timeout (as native, `DELIVERY_TIMEOUT_MS`).
const TIMEOUT_MS: u64 = runbook_core::webhooks::DELIVERY_TIMEOUT_MS;

/// A Queues producer.
pub struct WorkersQueue(pub worker::Queue);
impl NotificationQueue for WorkersQueue {
    async fn send_batch(&self, messages: Vec<NotificationMessage>) -> Result<(), StoreError> {
        self.0
            .send_batch(messages)
            .await
            .map_err(|_| StoreError::Backend("queue.send_failed"))
    }
}

/// Vet a webhook URL on Workers, which cannot resolve a name: its syntax
/// and any literal address (core's `parse_target`), then the host as fetch
/// will read it (gate fix B3, WHATWG): an address it decodes from a
/// name-like host is judged as an address.
pub fn vet(url: &str, policy: TargetPolicy) -> Result<(), &'static str> {
    let target = parse_target(url, policy)?;
    if let Some(ip) = target.literal
        && !address_allowed(ip, policy)
    {
        return Err("webhooks.target_forbidden");
    }
    let parsed = worker::Url::parse(url).map_err(|_| "webhooks.target_invalid")?;
    let address = match parsed.host() {
        Some(worker::url::Host::Ipv4(v4)) => Some(std::net::IpAddr::V4(v4)),
        Some(worker::url::Host::Ipv6(v6)) => Some(std::net::IpAddr::V6(v6)),
        Some(worker::url::Host::Domain(_)) => None,
        None => return Err("webhooks.target_invalid"),
    };
    if address.is_some_and(|ip| !address_allowed(ip, policy)) {
        return Err("webhooks.target_forbidden");
    }
    Ok(())
}

/// HTTP POST with the native rules: the target is vetted on every attempt,
/// redirects are never followed, the call times out after 10 s, and up to
/// 512 bytes of the answer are kept.
///
/// Workers cannot resolve a name, so only the URL's syntax and a literal
/// address are vetted here; a hostname is not resolved and checked. Workers
/// egress cannot reach private networks; that is the remaining guard.
pub struct FetchTransport {
    pub policy: TargetPolicy,
}
impl WebhookTransport for FetchTransport {
    async fn post(&self, request: OutgoingRequest) -> TransportResult {
        if let Err(reason) = vet(&request.url, self.policy) {
            return TransportResult::Blocked { reason };
        }
        let started = worker::Date::now().as_millis();
        let latency = || worker::Date::now().as_millis().saturating_sub(started);
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_redirect(RequestRedirect::Manual)
            .with_body(Some(request.body.clone().into()));
        let Ok(mut outgoing) = Request::new_with_init(&request.url, &init) else {
            return TransportResult::Blocked {
                reason: "webhooks.target_invalid",
            };
        };
        if let Ok(headers) = outgoing.headers_mut() {
            for (name, value) in &request.headers {
                let _ = headers.set(name, value);
            }
        }
        let controller = AbortController::default();
        let signal = controller.signal();
        let fetch = Fetch::Request(outgoing);
        let sent = futures_util::future::select(
            Box::pin(fetch.send_with_signal(&signal)),
            Box::pin(Delay::from(std::time::Duration::from_millis(TIMEOUT_MS))),
        )
        .await;
        match sent {
            futures_util::future::Either::Right(((), _)) => {
                controller.abort();
                TransportResult::Timeout {
                    latency_ms: latency(),
                }
            }
            futures_util::future::Either::Left((Err(_), _)) => TransportResult::Failed {
                error: "connection_failed",
                latency_ms: latency(),
            },
            futures_util::future::Either::Left((Ok(mut response), _)) => {
                let status = response.status_code();
                // Review 7: read at most the snippet, never the whole body
                // (a receiver may answer with megabytes, or never end it).
                let read = async move {
                    let mut kept = Vec::new();
                    if let Ok(mut body) = response.stream() {
                        use futures_util::StreamExt;
                        while kept.len() < SNIPPET_BYTES {
                            match body.next().await {
                                Some(Ok(chunk)) => kept.extend_from_slice(&chunk),
                                _ => break,
                            }
                        }
                    }
                    kept
                };
                let mut snippet = match futures_util::future::select(
                    Box::pin(read),
                    Box::pin(Delay::from(std::time::Duration::from_millis(
                        SNIPPET_WAIT_MS,
                    ))),
                )
                .await
                {
                    futures_util::future::Either::Left((kept, _)) => kept,
                    futures_util::future::Either::Right(_) => Vec::new(),
                };
                snippet.truncate(SNIPPET_BYTES);
                TransportResult::Response {
                    status,
                    latency_ms: latency(),
                    snippet: String::from_utf8_lossy(&snippet).into_owned(),
                }
            }
        }
    }
}

/// The target policy: private and plain-http targets only for local tests.
pub fn policy(env: &Env) -> TargetPolicy {
    TargetPolicy {
        // Gate fix B1: a test seam; the deploy build never allows them.
        allow_private: crate::seams::on(env, "WEBHOOKS_ALLOW_PRIVATE"),
        allow_link_local: false,
    }
}

/// Fill `bytes` from the platform CSPRNG (`crypto.getRandomValues`).
pub fn crypto_fill(bytes: &mut [u8]) -> Result<(), ()> {
    use wasm_bindgen::JsCast;
    let global = js_sys::global();
    let crypto = js_sys::Reflect::get(&global, &"crypto".into()).map_err(|_| ())?;
    let get: js_sys::Function = js_sys::Reflect::get(&crypto, &"getRandomValues".into())
        .map_err(|_| ())?
        .dyn_into()
        .map_err(|_| ())?;
    for chunk in bytes.chunks_mut(65_536) {
        let array =
            js_sys::Uint8Array::new_with_length(u32::try_from(chunk.len()).map_err(|_| ())?);
        get.call1(&crypto, &array).map_err(|_| ())?;
        array.copy_to(chunk);
    }
    Ok(())
}

fn key(text: &str) -> Option<[u8; 32]> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .ok()?
        .try_into()
        .ok()
}

/// The webhook secret sealer (CF3): `WEBHOOK_SECRET_KEY` (base64, 32
/// bytes, a secret binding) seals; `WEBHOOK_PREVIOUS_SECRET_KEYS`
/// (comma-separated base64) still open, as natively. None without a key:
/// webhook creation answers unconfigured and deliveries wait.
pub fn sealer(env: &Env) -> Option<crate::sealer::AesSealer> {
    let current = env
        .secret("WEBHOOK_SECRET_KEY")
        .ok()
        .or_else(|| env.var("WEBHOOK_SECRET_KEY").ok())
        .and_then(|v| key(&v.to_string()))?;
    let previous: Vec<[u8; 32]> = env
        .var("WEBHOOK_PREVIOUS_SECRET_KEYS")
        .ok()
        .map(|v| v.to_string().split(',').filter_map(key).collect())
        .unwrap_or_default();
    Some(crate::sealer::AesSealer::with_previous(
        &current,
        &previous,
        crypto_fill,
    ))
}

/// The webhook store the consumer and the crons use (CF11): D1 and the
/// subscriptions' WebhookDOs.
pub type WorkersWebhooks = crate::webhooks::CfWebhooks<
    runbook_store_d1::workers::WorkersD1,
    crate::webhooks::ViaTransport<crate::edge::DoTransport>,
>;
pub fn webhooks(env: &Env) -> Option<WorkersWebhooks> {
    let shards = crate::edge::env_shards(env).ok()?;
    let transport = crate::edge::DoTransport::new(env).ok()?;
    Some(crate::webhooks::CfWebhooks::sharded(
        shards,
        crate::webhooks::ViaTransport(transport),
    ))
}

/// NOTIFY1: mail through the auth Worker's transport.
pub(crate) struct AuthMail(pub(crate) crate::auth::WorkersSessions);
impl runbook_core::notify::deliver::MailTransport for AuthMail {
    async fn send(&self, message: &runbook_core::notify::EmailMessage) -> Result<(), String> {
        self.0.email_send(message).await
    }
}

fn salt() -> String {
    let mut bytes = [0u8; 16];
    if crypto_fill(&mut bytes).is_ok() {
        return bytes.iter().map(|b| format!("{b:02x}")).collect();
    }
    // No entropy: an empty salt, which matches no token, so the links
    // fail closed instead of being guessable.
    String::new()
}

/// SLACK1: Slack posts ride the same fetch as everything else here.
struct SlackFetch;
impl runbook_core::slack::SlackTransport for SlackFetch {
    async fn post_slack(&self, url: &str, text: &str) -> Result<(), String> {
        let mut init = RequestInit::new();
        init.with_method(Method::Post);
        let headers = worker::Headers::new();
        headers
            .set("content-type", "application/json")
            .map_err(|_| "slack_headers".to_owned())?;
        init.with_headers(headers);
        init.with_body(Some(serde_json::json!({ "text": text }).to_string().into()));
        let request = Request::new_with_init(url, &init).map_err(|_| "slack_url".to_owned())?;
        let response = Fetch::Request(request)
            .send()
            .await
            .map_err(|_| "slack_unreachable".to_owned())?;
        if response.status_code() >= 300 {
            return Err(format!("slack_{}", response.status_code()));
        }
        Ok(())
    }
}

/// The queue consumer: fan each message out (D1 and the subscriptions'
/// WebhookDOs) and
/// attempt its deliveries; a message that fails is retried after the
/// native fan-out lease, and after `max_retries` lands in the dead-letter
/// queue (redrive: CF3 "Dead letters").
pub async fn consume_batch(
    batch: MessageBatch<NotificationMessage>,
    env: &Env,
) -> worker::Result<()> {
    let retry = QueueRetryOptionsBuilder::new()
        .with_delay_seconds(FANOUT_RETRY_SECONDS)
        .build();
    let webhooks = webhooks(env);
    let sealer = sealer(env);
    let transport = FetchTransport {
        policy: policy(env),
    };
    // NOTIFY1: email fan-out + send, best-effort beside webhooks. Mail
    // rows are idempotent, so a queue retry re-records nothing.
    let mail_sessions = crate::auth::WorkersSessions::from_env(env, &crate::edge::origin(env));
    for message in batch.messages()? {
        let body = message.body().clone();
        if let (Some(shards), Ok(origin)) = (
            crate::edge::env_shards(env).ok(),
            Ok::<String, ()>(crate::edge::origin(env)),
        ) {
            let notify = runbook_store_d1::D1Notify::new(shards.control());
            let directory = runbook_store_d1::D1Directory::new(shards.control());
            let _ = runbook_core::notify::fanout::fan_out_email(
                &notify,
                &directory,
                &body.scope,
                &body.intent,
                &body.meta.title,
                &origin,
                salt,
            )
            .await;
            let _ = runbook_core::slack::fan_out_slack(
                &notify,
                &SlackFetch,
                &body.scope,
                &body.intent,
                &body.meta.title,
                body.intent
                    .ready_steps
                    .first()
                    .map(|work| work.title.as_str()),
                &origin,
            )
            .await;
            if let Some(sessions) = &mail_sessions {
                let _ = runbook_core::notify::deliver::deliver_emails(
                    &notify,
                    &AuthMail(sessions.clone()),
                    25,
                )
                .await;
            }
        }
        let outcome = match (&webhooks, &sealer) {
            (Some(webhooks), Some(sealer)) => {
                consume(
                    webhooks,
                    &transport,
                    sealer,
                    body,
                    DeliverySettings::default(),
                )
                .await
            }
            _ => Err(StoreError::Backend("webhooks.unconfigured")),
        };
        match outcome {
            Ok(_) => message.ack(),
            Err(_) => message.retry_with_options(&retry),
        }
    }
    Ok(())
}
