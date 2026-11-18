//! Native webhook host (C1): the AES-256-GCM secret sealer, DNS vetting
//! and the HTTP transport, plus the `/v1/projects/{p}/webhooks` handler.
//!
//! Every delivery resolves the target again, rejects private, loopback,
//! link-local and unspecified addresses (unless the self-hosted setting
//! allows private targets), and then connects only to the addresses it
//! vetted: the HTTP client's resolver returns exactly that list, so a DNS
//! answer cannot change between the check and the connection. Redirects
//! are never followed and no proxy is consulted. Secrets, signatures and
//! request bodies are never logged.
use crate::config::WebhookConfig;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use runbook_api::webhooks::WebhookHost;
use runbook_core::{
    StoreError,
    webhooks::{
        OutgoingRequest, SealedSecret, SecretSealer, TargetPolicy, TransportResult,
        WebhookTransport, address_allowed, parse_target,
    },
};
use std::{
    io::Read,
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

/// Host name and port to socket addresses. Tests substitute a resolver to
/// model DNS rebinding; production uses the system resolver.
pub type Resolve = Arc<dyn Fn(&str, u16) -> std::io::Result<Vec<SocketAddr>> + Send + Sync>;

pub fn system_resolver() -> Resolve {
    Arc::new(|host, port| (host, port).to_socket_addrs().map(Iterator::collect))
}

/// AES-256-GCM with a random 96-bit nonce per seal; the subscription ID is
/// associated data, so a sealed secret only opens for its own row.
///
/// F101: `v2.<base64url(kid || nonce || ciphertext || tag)>`. `kid` is one
/// byte derived from the key ([`AeadSealer::key_id`]) and is authenticated
/// with the ciphertext, so a key can be rotated: the new key seals, and
/// previous keys (RUNBOOK_WEBHOOK_PREVIOUS_SECRET_KEYS) still open what they
/// sealed. A value whose key is absent fails closed. Values written before
/// key ids (`v1.<base64url(nonce || ciphertext || tag)>`) stay readable with
/// any configured key; the tag decides which.
pub struct AeadSealer {
    /// The current key first; it alone seals.
    keys: Vec<(u8, LessSafeKey)>,
}
impl AeadSealer {
    pub fn new(key: &[u8; 32]) -> Self {
        Self::with_previous(key, &[])
    }
    pub fn with_previous(current: &[u8; 32], previous: &[[u8; 32]]) -> Self {
        Self {
            keys: std::iter::once(current)
                .chain(previous)
                .map(|key| {
                    let unbound = UnboundKey::new(&AES_256_GCM, key).expect("32-byte AES key");
                    (Self::key_id(key), LessSafeKey::new(unbound))
                })
                .collect(),
        }
    }
    /// A one-byte key fingerprint; it selects keys, never authenticates.
    pub fn key_id(key: &[u8; 32]) -> u8 {
        let mut input = b"runbook.webhook-key.v1\0".to_vec();
        input.extend_from_slice(key);
        ring::digest::digest(&ring::digest::SHA256, &input).as_ref()[0]
    }
}
fn v2_aad(kid: u8, context: &str) -> Vec<u8> {
    let mut aad = vec![kid];
    aad.extend_from_slice(context.as_bytes());
    aad
}
fn open_with(key: &LessSafeKey, aad: &[u8], sealed: &[u8]) -> Option<String> {
    let (nonce, data) = sealed.split_at_checked(NONCE_LEN)?;
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let mut data = data.to_vec();
    let plain = key.open_in_place(nonce, Aad::from(aad), &mut data).ok()?;
    String::from_utf8(plain.to_vec()).ok()
}
impl SecretSealer for AeadSealer {
    fn seal(&self, context: &str, secret: &str) -> Result<SealedSecret, StoreError> {
        let (kid, key) = &self.keys[0];
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| StoreError::Backend("webhooks.entropy"))?;
        let mut data = secret.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(v2_aad(*kid, context)),
            &mut data,
        )
        .map_err(|_| StoreError::Backend("webhooks.seal_failed"))?;
        let mut out = vec![*kid];
        out.extend(nonce);
        out.extend(data);
        Ok(SealedSecret(format!("v2.{}", URL_SAFE_NO_PAD.encode(out))))
    }
    fn open(&self, context: &str, sealed: &SealedSecret) -> Result<String, StoreError> {
        let failed = || StoreError::Backend("webhooks.open_failed");
        let (version, body) = sealed.0.split_once('.').ok_or_else(failed)?;
        let bytes = URL_SAFE_NO_PAD.decode(body).map_err(|_| failed())?;
        match version {
            "v2" => {
                let (&kid, rest) = bytes.split_first().ok_or_else(failed)?;
                let aad = v2_aad(kid, context);
                self.keys
                    .iter()
                    .filter(|(id, _)| *id == kid)
                    .find_map(|(_, key)| open_with(key, &aad, rest))
                    .ok_or_else(failed)
            }
            "v1" => self
                .keys
                .iter()
                .find_map(|(_, key)| open_with(key, context.as_bytes(), &bytes))
                .ok_or_else(failed),
            _ => Err(failed()),
        }
    }
}

/// Name resolution is bounded: a resolver (getaddrinfo has no timeout of
/// its own) runs on a helper thread and is abandoned after this long.
pub const DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// Parse, resolve (bounded by [`DNS_TIMEOUT`]) and vet a target. Every
/// resolved address must be allowed by the policy.
pub fn vet(
    url: &str,
    policy: TargetPolicy,
    resolve: &Resolve,
) -> Result<Vec<SocketAddr>, &'static str> {
    vet_within(url, policy, resolve, DNS_TIMEOUT)
}

pub fn vet_within(
    url: &str,
    policy: TargetPolicy,
    resolve: &Resolve,
    dns_timeout: Duration,
) -> Result<Vec<SocketAddr>, &'static str> {
    let target = parse_target(url, policy)?;
    let addrs = match target.literal {
        Some(ip) => vec![SocketAddr::new(ip, target.port)],
        None => resolve_within(resolve, &target.host, target.port, dns_timeout)
            .map_err(|_| "webhooks.target_unresolved")?,
    };
    if addrs.is_empty() {
        return Err("webhooks.target_unresolved");
    }
    if addrs.iter().any(|a| !address_allowed(a.ip(), policy)) {
        return Err("webhooks.target_forbidden");
    }
    Ok(addrs)
}

fn resolve_within(
    resolve: &Resolve,
    host: &str,
    port: u16,
    timeout: Duration,
) -> std::io::Result<Vec<SocketAddr>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (resolve, host) = (resolve.clone(), host.to_owned());
    std::thread::Builder::new()
        .name("runbook-webhook-dns".into())
        .spawn(move || {
            let _ = tx.send(resolve(&host, port));
        })?;
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| Err(std::io::ErrorKind::TimedOut.into()))
}

const SNIPPET_BYTES: u64 = 512;

/// Blocking POST to vetted addresses only; runs on Tokio's blocking pool.
#[derive(Clone)]
pub struct HttpTransport {
    pub policy: TargetPolicy,
    pub resolve: Resolve,
    pub timeout: Duration,
}
impl HttpTransport {
    pub fn new(policy: TargetPolicy, resolve: Resolve) -> Self {
        Self {
            policy,
            resolve,
            timeout: Duration::from_millis(runbook_core::webhooks::DELIVERY_TIMEOUT_MS),
        }
    }
    fn post_blocking(&self, request: OutgoingRequest) -> TransportResult {
        let addrs = match vet(&request.url, self.policy, &self.resolve) {
            Ok(addrs) => addrs,
            // F49: a resolver failure or timeout is transient, like a
            // refused connection; only a forbidden target is final.
            Err(reason @ "webhooks.target_unresolved") => {
                return TransportResult::Failed {
                    error: reason,
                    latency_ms: 0,
                };
            }
            Err(reason) => return TransportResult::Blocked { reason },
        };
        let agent = ureq::AgentBuilder::new()
            .resolver(move |_: &str| Ok(addrs.clone()))
            .redirects(0)
            .timeout(self.timeout)
            .build();
        let mut call = agent.post(&request.url);
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let started = Instant::now();
        let result = call.send_string(&request.body);
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let response = match result {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(transport)) => {
                let timed_out = matches!(transport.kind(), ureq::ErrorKind::Io)
                    && latency_ms >= u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX);
                return if timed_out {
                    TransportResult::Timeout { latency_ms }
                } else {
                    TransportResult::Failed {
                        error: match transport.kind() {
                            ureq::ErrorKind::Dns => "dns",
                            ureq::ErrorKind::ConnectionFailed => "connection_failed",
                            ureq::ErrorKind::TooManyRedirects => "redirect",
                            _ => "transport",
                        },
                        latency_ms,
                    }
                };
            }
        };
        let status = response.status();
        let mut snippet = Vec::new();
        let _ = response
            .into_reader()
            .take(SNIPPET_BYTES)
            .read_to_end(&mut snippet);
        TransportResult::Response {
            status,
            latency_ms,
            snippet: String::from_utf8_lossy(&snippet).into_owned(),
        }
    }
}
impl WebhookTransport for HttpTransport {
    async fn post(&self, request: OutgoingRequest) -> TransportResult {
        let transport = self.clone();
        tokio::task::spawn_blocking(move || transport.post_blocking(request))
            .await
            .unwrap_or(TransportResult::Failed {
                error: "worker",
                latency_ms: 0,
            })
    }
}

/// The API's host capabilities on the native server.
pub struct ServerHost {
    pub sealer: Option<AeadSealer>,
    pub policy: TargetPolicy,
    pub resolve: Resolve,
}
impl ServerHost {
    pub fn new(config: &WebhookConfig) -> Self {
        Self {
            sealer: config
                .secret_key
                .as_ref()
                .map(|key| AeadSealer::with_previous(key, &config.previous_secret_keys)),
            policy: config.policy(),
            resolve: config.resolver.clone().unwrap_or_else(system_resolver),
        }
    }
}
impl WebhookHost for ServerHost {
    type Sealer = AeadSealer;
    fn sealer(&self) -> Option<&AeadSealer> {
        self.sealer.as_ref()
    }
    fn random32(&self) -> Option<[u8; 32]> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).ok().map(|_| bytes)
    }
    fn target_policy(&self) -> TargetPolicy {
        self.policy
    }
    async fn vet_target(&self, url: &str) -> Result<(), &'static str> {
        let (url, policy, resolve) = (url.to_owned(), self.policy, self.resolve.clone());
        tokio::task::spawn_blocking(move || vet(&url, policy, &resolve).map(|_| ()))
            .await
            .unwrap_or(Err("webhooks.target_unresolved"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sealed_secrets_open_only_for_their_subscription_and_key() {
        let sealer = AeadSealer::new(&[7; 32]);
        let secret = runbook_core::webhooks::encode_webhook_secret([3; 32]);
        let sealed = sealer.seal("whk_a", &secret).unwrap();
        assert!(!sealed.0.contains(&secret) && sealed.0.starts_with("v2."));
        assert_ne!(
            sealed,
            sealer.seal("whk_a", &secret).unwrap(),
            "fresh nonce"
        );
        assert_eq!(sealer.open("whk_a", &sealed).unwrap(), secret);
        assert!(sealer.open("whk_b", &sealed).is_err());
        assert!(AeadSealer::new(&[8; 32]).open("whk_a", &sealed).is_err());
        // Always change the last character (a random nonce may already end
        // in any given base64 symbol, which made a fixed 'A' flaky).
        let mut tampered = sealed.0.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(sealer.open("whk_a", &SealedSecret(tampered)).is_err());
    }

    /// F101: a value sealed before key ids existed (v1, key [7; 32]).
    const V1_FIXTURE: &str = "v1.u73Surapxd1KnyaICzYt4UI5ZXfOBEqo7xlBePD-0PQKEJs-e0s8M9LoYjA0SXt_NMXxGRiOYBm0ekLg1x-xOsOr45aZuyE_cqKdVsk";
    const FIXTURE_CONTEXT: &str = "whk_00000000000000000000000001";

    #[test]
    fn sealed_secrets_carry_a_key_id_and_old_values_stay_readable() {
        let secret = runbook_core::webhooks::encode_webhook_secret([3; 32]);
        let fixture = SealedSecret(V1_FIXTURE.into());
        // Existing v1 values open with the key that sealed them, current or
        // previous, and with nothing else.
        assert_eq!(
            AeadSealer::new(&[7; 32])
                .open(FIXTURE_CONTEXT, &fixture)
                .unwrap(),
            secret
        );
        let rotated = AeadSealer::with_previous(&[8; 32], &[[7; 32]]);
        assert_eq!(rotated.open(FIXTURE_CONTEXT, &fixture).unwrap(), secret);
        assert!(
            AeadSealer::new(&[8; 32])
                .open(FIXTURE_CONTEXT, &fixture)
                .is_err()
        );
        // New values are v2 and name their key.
        let sealed = rotated.seal(FIXTURE_CONTEXT, &secret).unwrap();
        let body = sealed.0.strip_prefix("v2.").expect("v2 format");
        let bytes = URL_SAFE_NO_PAD.decode(body).unwrap();
        assert_eq!(bytes[0], AeadSealer::key_id(&[8; 32]));
        assert_ne!(AeadSealer::key_id(&[8; 32]), AeadSealer::key_id(&[7; 32]));
        assert_eq!(rotated.open(FIXTURE_CONTEXT, &sealed).unwrap(), secret);
        assert_eq!(
            AeadSealer::new(&[8; 32])
                .open(FIXTURE_CONTEXT, &sealed)
                .unwrap(),
            secret
        );
    }

    #[test]
    fn a_wrong_key_fails_closed() {
        let secret = runbook_core::webhooks::encode_webhook_secret([3; 32]);
        let sealed = AeadSealer::new(&[7; 32])
            .seal(FIXTURE_CONTEXT, &secret)
            .unwrap();
        // The key named by the id is missing: refused, nothing returned.
        let other = AeadSealer::with_previous(&[8; 32], &[[9; 32]]);
        assert!(matches!(
            other.open(FIXTURE_CONTEXT, &sealed),
            Err(StoreError::Backend("webhooks.open_failed"))
        ));
        // Relabelling the value with another key's id does not help: the id
        // is authenticated with the ciphertext.
        let mut bytes = URL_SAFE_NO_PAD
            .decode(sealed.0.strip_prefix("v2.").unwrap())
            .unwrap();
        bytes[0] = AeadSealer::key_id(&[8; 32]);
        let relabelled = SealedSecret(format!("v2.{}", URL_SAFE_NO_PAD.encode(&bytes)));
        assert!(
            AeadSealer::with_previous(&[8; 32], &[[7; 32]])
                .open(FIXTURE_CONTEXT, &relabelled)
                .is_err()
        );
        // Unknown versions are refused.
        let v3 = SealedSecret(sealed.0.replacen("v2.", "v3.", 1));
        assert!(
            AeadSealer::new(&[7; 32])
                .open(FIXTURE_CONTEXT, &v3)
                .is_err()
        );
    }

    #[test]
    fn vetting_rejects_private_answers_unless_allowed() {
        let private: Resolve =
            Arc::new(|_, port| Ok(vec![SocketAddr::from(([10, 0, 0, 7], port))]));
        let mixed: Resolve = Arc::new(|_, port| {
            Ok(vec![
                SocketAddr::from(([93, 184, 216, 34], port)),
                SocketAddr::from(([127, 0, 0, 1], port)),
            ])
        });
        let public: Resolve =
            Arc::new(|_, port| Ok(vec![SocketAddr::from(([93, 184, 216, 34], port))]));
        let url = "https://hooks.example.com/x";
        let cloud = TargetPolicy::default();
        let self_hosted = TargetPolicy {
            allow_private: true,
            allow_link_local: false,
        };
        assert_eq!(vet(url, cloud, &private), Err("webhooks.target_forbidden"));
        assert_eq!(vet(url, cloud, &mixed), Err("webhooks.target_forbidden"));
        assert_eq!(vet(url, cloud, &public).unwrap().len(), 1);
        assert!(vet(url, self_hosted, &private).is_ok());
        assert_eq!(
            vet("http://hooks.example.com/x", cloud, &public),
            Err("webhooks.url_https_required")
        );
        // A name answering with cloud metadata stays blocked when only
        // private targets are allowed.
        let metadata: Resolve =
            Arc::new(|_, port| Ok(vec![SocketAddr::from(([169, 254, 169, 254], port))]));
        assert_eq!(
            vet(url, self_hosted, &metadata),
            Err("webhooks.target_forbidden")
        );
        let teredo: Resolve = Arc::new(|_, port| {
            Ok(vec![SocketAddr::new(
                "2001:0:4136:e378::1".parse().unwrap(),
                port,
            )])
        });
        assert_eq!(vet(url, cloud, &teredo), Err("webhooks.target_forbidden"));
        assert!(
            vet(
                url,
                TargetPolicy {
                    allow_private: true,
                    allow_link_local: true
                },
                &metadata
            )
            .is_ok()
        );
    }

    #[test]
    fn hung_resolvers_are_abandoned_after_the_dns_timeout() {
        let hung: Resolve = Arc::new(|_, _| {
            std::thread::sleep(Duration::from_secs(30));
            Ok(vec![])
        });
        let started = std::time::Instant::now();
        assert_eq!(
            vet_within(
                "https://hooks.example.com/x",
                TargetPolicy::default(),
                &hung,
                Duration::from_millis(200)
            ),
            Err("webhooks.target_unresolved")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
