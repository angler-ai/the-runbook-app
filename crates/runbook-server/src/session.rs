//! AUTH2 browser sessions (D44–D46), the native half: the auth child's
//! `POST /internal/session` behind [`runbook_api::session::SessionResolver`],
//! cached 30 s by the cookie's SHA-256 (a proxied sign-out evicts its entry
//! at once), and the loopback gate of `/internal/*`. Cookie parsing, CSRF,
//! linking and `can-register` are portable (`runbook_api::session`).
use crate::{auth_child::AuthChild, config::AuthConfig};
use http::HeaderMap;
use runbook_api::{
    ResolvedSession,
    oauth::TokenIntrospector,
    session::{SessionResolver, Unavailable, cookie_name, find_session_cookie, parse_session},
};
use runbook_core::credentials::secret_hash;
use serde_json::Value;
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// What the auth child says about a cookie, cached.
#[derive(Clone)]
struct Cached {
    session: ResolvedSession,
    at: Instant,
}

/// Session resolution through the auth child, shared by the transport and
/// the store executor. Send and Sync; the cache never holds raw cookies.
pub struct Sessions {
    child: AuthChild,
    pub config: AuthConfig,
    pub origin: String,
    cookie_name: &'static str,
    agent: ureq::Agent,
    cache: Mutex<HashMap<[u8; 32], Cached>>,
    /// MCP2 review 6: tokens the child just said are not active, for
    /// `oauth::NEGATIVE_CACHE_MS`.
    inactive: Mutex<HashMap<[u8; 32], Instant>>,
}

impl Sessions {
    pub fn new(child: AuthChild, config: AuthConfig, origin: &str) -> Arc<Self> {
        Arc::new(Self {
            child,
            config,
            origin: origin.to_owned(),
            cookie_name: cookie_name(origin),
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout(Duration::from_secs(10))
                .build(),
            cache: Mutex::default(),
            inactive: Mutex::default(),
        })
    }
    pub fn child(&self) -> &AuthChild {
        &self.child
    }
    /// The sign-in methods for discovery: what the child's ready line
    /// listed, or RUNBOOK_AUTH_METHODS until it has printed one.
    pub fn methods(&self) -> Vec<String> {
        self.child
            .methods()
            .unwrap_or_else(|| self.config.methods.clone())
    }
    pub fn agent(&self) -> &ureq::Agent {
        &self.agent
    }
    pub fn cookie_name(&self) -> &'static str {
        self.cookie_name
    }
    pub fn cookie(&self, headers: &HeaderMap) -> Option<String> {
        find_session_cookie(&crate::http::headers(headers), &self.origin)
    }
    fn key(cookie: &str) -> [u8; 32] {
        *secret_hash(cookie).as_bytes()
    }
    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Cached>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// D44: forget a cookie at once (a proxied sign-out).
    pub fn evict(&self, cookie: &str) {
        self.cache().remove(&Self::key(cookie));
    }
    /// Gate fix 6: forget every cached session (a revoke of sessions the
    /// proxy cannot map to cookies).
    pub fn clear(&self) {
        self.cache().clear();
    }
    /// The live session a cookie names, or None for an unknown, expired or
    /// unverified one. Cached for `session_cache_ttl`; failures are not.
    pub async fn resolve(&self, cookie: &str) -> Result<Option<ResolvedSession>, Unavailable> {
        // auth/README.md: the secret header, and only the session cookie as
        // the `Cookie` header (never the browser's other cookies).
        let header = format!("{}={cookie}", self.cookie_name);
        self.cached(
            Self::key(cookie),
            "session",
            ("cookie", header),
            String::new(),
        )
        .await
    }

    /// MCP2: an OAuth access token through the child's
    /// `POST /internal/oauth/introspect {token, resource}`, which answers in
    /// the session's shape plus `scope`; cached in the same map under a
    /// digest kept apart from cookie keys.
    pub async fn introspect(
        &self,
        token: &str,
        resource: &str,
    ) -> Result<Option<ResolvedSession>, Unavailable> {
        let body = serde_json::json!({ "token": token, "resource": resource }).to_string();
        let mut key = Self::key(token);
        key[0] ^= 0xff;
        let header = ("content-type", "application/json".to_owned());
        let negative = Duration::from_millis(runbook_api::oauth::NEGATIVE_CACHE_MS);
        {
            let mut inactive = self.inactive.lock().unwrap_or_else(PoisonError::into_inner);
            inactive.retain(|_, at| at.elapsed() < negative);
            if inactive.contains_key(&key) {
                return Ok(None);
            }
        }
        let found = self.cached(key, "oauth/introspect", header, body).await;
        if let Ok(None) = found {
            self.inactive
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, Instant::now());
        }
        found
    }

    /// One cached D44 call: `POST /internal/{route}` with the secret, one
    /// header and a body (gate fix 2: a fresh nonce, and only an answer that
    /// proves the secret); 401/403/404 mean "no such session".
    async fn cached(
        &self,
        key: [u8; 32],
        route: &str,
        header: (&'static str, String),
        body: String,
    ) -> Result<Option<ResolvedSession>, Unavailable> {
        let now = now_ms();
        {
            let mut cache = self.cache();
            cache.retain(|_, c| c.at.elapsed() < self.config.session_cache_ttl);
            if let Some(hit) = cache.get(&key) {
                return Ok((hit.session.expires_at_ms > now).then(|| hit.session.clone()));
            }
        }
        let answer = self.internal(route, header, body).await?;
        let Some(session) = answer.as_ref().and_then(parse_session) else {
            return Ok(None);
        };
        // D45: an unverified email is never a session here.
        if !session.email_verified || session.expires_at_ms <= now {
            return Ok(None);
        }
        if answer
            .as_ref()
            .is_some_and(|body| body.get("mfa_enrolled").is_some())
        {
            return Ok(Some(session));
        }
        self.cache().insert(
            key,
            Cached {
                session: session.clone(),
                at: Instant::now(),
            },
        );
        Ok(Some(session))
    }

    /// MCP2: a connected-app call (`oauth/grants`, `oauth/grants/revoke`)
    /// with a JSON body. A revoke clears the positive cache, so a revoked
    /// grant's tokens stop at once here.
    pub async fn auth_internal(
        &self,
        route: &str,
        body: &Value,
    ) -> Result<Option<Value>, Unavailable> {
        let header = ("content-type", "application/json".to_owned());
        let answer = self.internal(route, header, body.to_string()).await;
        if route.ends_with("/revoke") || route == "mfa/reset" {
            self.clear();
        }
        answer
    }

    /// One D44 internal call: `POST /internal/{route}` with the secret, a
    /// fresh nonce (gate fix 2: only an answer that proves the secret), one
    /// header and a body. 401/403/404 is `None`.
    async fn internal(
        &self,
        route: &str,
        header: (&'static str, String),
        body: String,
    ) -> Result<Option<Value>, Unavailable> {
        let base = self.child.base_url().ok_or(Unavailable)?;
        let nonce = self.child.nonce().ok_or(Unavailable)?;
        let url = format!("{base}/internal/{route}");
        let secret = self.child.secret().to_owned();
        let agent = self.agent.clone();
        let sent = nonce.clone();
        let (proof, answer) = tokio::task::spawn_blocking(move || {
            let proof =
                |r: &ureq::Response| r.header(crate::auth_child::PROOF_HEADER).map(str::to_owned);
            match agent
                .post(&url)
                .set(crate::auth_child::SECRET_HEADER, &secret)
                .set(crate::auth_child::NONCE_HEADER, &sent)
                .set(header.0, &header.1)
                .timeout(Duration::from_secs(5))
                .send_string(&body)
            {
                Ok(response) => {
                    let proof = proof(&response);
                    let body = response
                        .into_string()
                        .ok()
                        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                        .map(Some)
                        .ok_or(Unavailable);
                    (proof, body)
                }
                Err(ureq::Error::Status(401 | 403 | 404, response)) => (proof(&response), Ok(None)),
                Err(_) => (None, Err(Unavailable)),
            }
        })
        .await
        .map_err(|_| Unavailable)?;
        let answer = answer?;
        if !self.child.proof_ok(&nonce, proof.as_deref()) {
            return Err(Unavailable);
        }
        Ok(answer)
    }
}

impl TokenIntrospector for Sessions {
    async fn introspect(
        &self,
        token: &str,
        resource: &str,
    ) -> Result<Option<ResolvedSession>, Unavailable> {
        Sessions::introspect(self, token, resource).await
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub use runbook_api::session::CAN_REGISTER;

/// The `/internal/*` gate, decided from the transport alone: auth is on,
/// the peer is loopback, no proxy forwarded the request, and the per-boot
/// secret matches. Anything else is answered as an unknown route (404).
pub fn internal_allowed(
    sessions: Option<&Sessions>,
    headers: &HeaderMap,
    peer: Option<IpAddr>,
) -> bool {
    let Some(sessions) = sessions else {
        return false;
    };
    if !peer.is_some_and(|ip| ip.is_loopback())
        || [
            "forwarded",
            "x-forwarded-for",
            "x-real-ip",
            "cf-connecting-ip",
        ]
        .iter()
        .any(|h| headers.contains_key(*h))
    {
        return false;
    }
    let mut values = headers.get_all(crate::auth_child::SECRET_HEADER).iter();
    match (values.next(), values.next()) {
        (Some(v), None) => {
            let presented = secret_hash(v.to_str().unwrap_or_default());
            // Compare digests so the comparison leaks nothing about the secret.
            presented == secret_hash(sessions.child.secret())
        }
        _ => false,
    }
}

impl SessionResolver for Sessions {
    fn origin(&self) -> &str {
        &self.origin
    }
    async fn resolve(&self, cookie: &str) -> Result<Option<ResolvedSession>, Unavailable> {
        Sessions::resolve(self, cookie).await
    }
}
