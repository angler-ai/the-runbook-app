//! CF7: sign-in on Workers (D43–D46). The auth service is the `auth/`
//! Worker (`worker.ts`, Better Auth on D1), reached through the `AUTH`
//! service binding; it is never public on its own.
//!
//! - **Sessions (D44).** A cookie is resolved by the auth Worker's
//!   `POST /internal/session` with the shared internal secret, cached 30 s
//!   per isolate by the cookie's SHA-256 (never the cookie itself); a
//!   proxied sign-out evicts its entry at once. Parsing, CSRF (D46) and
//!   linking (D45) are portable (`runbook_api::session`).
//! - **The proxy.** `/api/auth/*` goes to the auth Worker unchanged except
//!   for the headers only Rust may set (`x-runbook-internal-secret`,
//!   `x-runbook-client-ip`), which are dropped. Responses come back as they
//!   are, every `Set-Cookie` included.
//! - **can-register (D45).** The auth Worker calls back through its `API`
//!   service binding with the secret; the api Worker refuses any other
//!   caller.
use runbook_api::{
    ResolvedSession,
    oauth::TokenIntrospector,
    session::{SessionResolver, Unavailable, cookie_name, parse_session},
};
use runbook_core::credentials::secret_hash;
use std::{cell::RefCell, collections::HashMap};
use worker::{Env, Fetcher, Headers, Method, Request, RequestInit, Response, Result};

/// The header the auth service and Rust present to each other.
pub const INTERNAL_SECRET_HEADER: &str = "x-runbook-internal-secret";
/// The variable holding that secret (both Workers; 32+ characters).
pub const INTERNAL_SECRET_VAR: &str = "RUNBOOK_AUTH_INTERNAL_SECRET";
/// How long a resolved session is trusted without asking again (D44).
pub const SESSION_CACHE_MS: f64 = 30_000.0;

thread_local! {
    /// SHA-256 of the cookie → (session, when it was resolved).
    static CACHE: RefCell<HashMap<[u8; 32], (ResolvedSession, f64)>> = RefCell::default();
}

fn now() -> f64 {
    js_sys::Date::now()
}

/// The auth Worker as this Worker reaches it, when sign-in is configured
/// (an `AUTH` binding and the internal secret).
#[derive(Clone)]
pub struct WorkersSessions {
    auth: Fetcher,
    secret: String,
    origin: String,
}

impl WorkersSessions {
    pub fn from_env(env: &Env, origin: &str) -> Option<Self> {
        let auth = env.service("AUTH").ok()?;
        let secret = env
            .secret(INTERNAL_SECRET_VAR)
            .ok()
            .map(|s| s.to_string())
            .or_else(|| env.var(INTERNAL_SECRET_VAR).ok().map(|v| v.to_string()))?;
        (secret.len() >= 32).then(|| Self {
            auth,
            secret,
            origin: origin.to_owned(),
        })
    }
    fn key(cookie: &str) -> [u8; 32] {
        *secret_hash(cookie).as_bytes()
    }
    /// D44: forget a cookie at once (a proxied sign-out).
    pub fn evict(cookie: &str) {
        CACHE.with(|cache| cache.borrow_mut().remove(&Self::key(cookie)));
    }
    /// Whether `presented` is the internal secret, compared as digests so
    /// neither length nor prefix leaks.
    pub fn secret_matches(&self, presented: Option<&str>) -> bool {
        presented.is_some_and(|p| secret_hash(p) == secret_hash(&self.secret))
    }

    /// Forward one `/api/auth/*` request to the auth Worker.
    pub async fn proxy(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let headers = Headers::new();
        for (name, value) in req.headers().entries() {
            let lower = name.to_ascii_lowercase();
            if lower == INTERNAL_SECRET_HEADER || lower == "x-runbook-client-ip" {
                continue;
            }
            headers.append(&name, &value)?;
        }
        let sign_out = req.method() == Method::Post && url.path() == "/api/auth/sign-out";
        if sign_out
            && let Some(cookie) =
                runbook_api::session::find_session_cookie(&runbook_headers(&req)?, &self.origin)
        {
            Self::evict(&cookie);
        }
        let mut init = RequestInit::new();
        init.with_method(req.method()).with_headers(headers);
        if !matches!(req.method(), Method::Get | Method::Head) {
            // CF12 review round 1: native's proxy cap, declared or chunked.
            let crate::body::Capped::Body(body) =
                crate::body::read(&mut req, runbook_api::http::AUTH_BODY_BYTES).await?
            else {
                return too_large();
            };
            if !body.is_empty() {
                init.with_body(Some(js_sys::Uint8Array::from(body.as_slice()).into()));
            }
        }
        // Keep redirects as they are (Google's callback answers with one).
        init.with_redirect(worker::RequestRedirect::Manual);
        let target = format!(
            "https://auth.internal{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.auth.fetch(target, Some(init)).await
    }

    /// The sign-in methods the auth Worker reports (`/internal/health`).
    pub async fn methods(&self) -> Vec<String> {
        let mut init = RequestInit::new();
        let headers = Headers::new();
        if headers.set(INTERNAL_SECRET_HEADER, &self.secret).is_err() {
            return vec![];
        }
        init.with_method(Method::Get).with_headers(headers);
        let Ok(mut response) = self
            .auth
            .fetch("https://auth.internal/internal/health", Some(init))
            .await
        else {
            return vec![];
        };
        if response.status_code() != 200 {
            return vec![];
        }
        let Ok(body) = response.json::<serde_json::Value>().await else {
            return vec![];
        };
        body["methods"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl SessionResolver for WorkersSessions {
    fn origin(&self) -> &str {
        &self.origin
    }
    async fn resolve(
        &self,
        cookie: &str,
    ) -> std::result::Result<Option<ResolvedSession>, Unavailable> {
        // auth/README.md: the secret header, and only the session cookie.
        let header = format!("{}={cookie}", cookie_name(&self.origin));
        self.cached(
            Self::key(cookie),
            "https://auth.internal/internal/session",
            ("cookie", &header),
            None,
            false,
        )
        .await
    }
}

impl TokenIntrospector for WorkersSessions {
    /// MCP2: an OAuth access token, introspected by the auth Worker (the
    /// session's shape plus `scope`) and cached with the sessions, by its
    /// SHA-256. "Not active" is remembered for a few seconds (review 6).
    async fn introspect(
        &self,
        token: &str,
        resource: &str,
    ) -> std::result::Result<Option<ResolvedSession>, Unavailable> {
        // `/mcp` passes only `trb_oat_` tokens of letters, digits and `_`
        // (checked there), and its own resource URL, so neither needs escaping.
        let body = format!(r#"{{"token":"{token}","resource":"{resource}"}}"#);
        let mut key = Self::key(token);
        // Keyed apart from a cookie with the same text.
        key[0] ^= 0xff;
        self.cached(
            key,
            "https://auth.internal/internal/oauth/introspect",
            ("content-type", "application/json"),
            Some(body),
            true,
        )
        .await
    }
}

impl WorkersSessions {
    /// MCP2: a connected-app call (`oauth/grants`, `oauth/grants/revoke`).
    /// A revoke clears this isolate's cache; other isolates keep a revoked
    /// token's positive entry for at most `SESSION_CACHE_MS` (30 s).
    pub async fn auth_internal(
        &self,
        route: &str,
        body: &serde_json::Value,
    ) -> std::result::Result<Option<serde_json::Value>, Unavailable> {
        let answer = self
            .call(
                &format!("https://auth.internal/internal/{route}"),
                ("content-type", "application/json"),
                Some(body.to_string()),
            )
            .await;
        if route.ends_with("/revoke") || route == "mfa/reset" {
            CACHE.with(|cache| cache.borrow_mut().clear());
        }
        answer
    }

    /// NOTIFY1: send one rendered message through the auth Worker's
    /// mail transport. 204 is acceptance; anything else is a retryable
    /// failure code, never response content.
    pub async fn email_send(
        &self,
        message: &runbook_core::notify::EmailMessage,
    ) -> std::result::Result<(), String> {
        let headers = Headers::new();
        headers
            .set(INTERNAL_SECRET_HEADER, &self.secret)
            .map_err(|_| "email_unavailable".to_owned())?;
        headers
            .set("content-type", "application/json")
            .map_err(|_| "email_unavailable".to_owned())?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers);
        let body = serde_json::json!({
            "to": message.to,
            "subject": message.subject,
            "text": message.text,
            "html": message.html,
        });
        init.with_body(Some(body.to_string().into()));
        let response = self
            .auth
            .fetch("https://auth.internal/internal/email/send", Some(init))
            .await
            .map_err(|_| "email_unavailable".to_owned())?;
        match response.status_code() {
            204 => Ok(()),
            503 => Err("email_unavailable".into()),
            code => Err(format!("email_{code}")),
        }
    }

    /// One D44 internal call to the auth Worker, `POST {url}` with the
    /// secret; its 401 is `None`.
    async fn call(
        &self,
        url: &str,
        header: (&str, &str),
        body: Option<String>,
    ) -> std::result::Result<Option<serde_json::Value>, Unavailable> {
        let headers = Headers::new();
        headers
            .set(INTERNAL_SECRET_HEADER, &self.secret)
            .map_err(|_| Unavailable)?;
        headers.set(header.0, header.1).map_err(|_| Unavailable)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers);
        if let Some(body) = body {
            init.with_body(Some(body.into()));
        }
        let mut response = self
            .auth
            .fetch(url, Some(init))
            .await
            .map_err(|_| Unavailable)?;
        match response.status_code() {
            200 => response.json().await.map(Some).map_err(|_| Unavailable),
            401 => Ok(None),
            _ => Err(Unavailable),
        }
    }

    /// The call, cached 30 s per isolate under `key` (a digest, never the
    /// cookie or token). With `negative`, a 401 is remembered for
    /// `oauth::NEGATIVE_CACHE_MS` as an already-expired entry; a hit never
    /// re-inserts it, so the window does not slide (re-check 3).
    async fn cached(
        &self,
        key: [u8; 32],
        url: &str,
        header: (&str, &str),
        body: Option<String>,
        negative: bool,
    ) -> std::result::Result<Option<ResolvedSession>, Unavailable> {
        let at = now();
        let hit = CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.retain(|_, (_, resolved)| at - *resolved < SESSION_CACHE_MS);
            cache.get(&key).map(|(session, _)| session.clone())
        });
        if let Some(session) = hit {
            #[allow(clippy::cast_possible_truncation)]
            return Ok((session.expires_at_ms > at as i64).then_some(session));
        }
        match self.call(url, header, body).await? {
            Some(answer) => {
                let Some(session) = parse_session(&answer) else {
                    return Ok(None);
                };
                if answer.get("mfa_enrolled").is_none() {
                    CACHE.with(|cache| cache.borrow_mut().insert(key, (session.clone(), at)));
                }
                Ok(Some(session))
            }
            None => {
                if negative {
                    #[allow(clippy::cast_precision_loss)]
                    let resolved =
                        at - SESSION_CACHE_MS + runbook_api::oauth::NEGATIVE_CACHE_MS as f64;
                    let inactive = ResolvedSession {
                        auth_user_id: String::new(),
                        email: String::new(),
                        email_verified: false,
                        name: None,
                        picture_url: None,
                        session_id: String::new(),
                        expires_at_ms: 0,
                        mfa_verified_at: None,
                        mfa_method: None,
                        mfa_enrolled: Some(false),
                        principal: None,
                        scopes: None,
                    };
                    CACHE.with(|cache| cache.borrow_mut().insert(key, (inactive, resolved)));
                }
                Ok(None)
            }
        }
    }
}

/// A request's headers in the router's form.
pub fn runbook_headers(req: &Request) -> Result<runbook_api::http::Headers> {
    let mut headers = runbook_api::http::Headers::new();
    for (name, value) in req.headers().entries() {
        headers.push(&name, value.as_bytes().to_vec());
    }
    Ok(headers)
}

/// CF12 review round 1: native's auth proxy refusal of a body over
/// [`runbook_api::http::AUTH_BODY_BYTES`] (413 problem, as `auth_proxy.rs`).
fn too_large() -> Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", "application/problem+json")?;
    headers.set("cache-control", "no-store")?;
    Ok(Response::from_json(&serde_json::json!({
        "type": "about:blank",
        "status": 413,
        "code": "request.body_too_large",
    }))?
    .with_status(413)
    .with_headers(headers))
}
