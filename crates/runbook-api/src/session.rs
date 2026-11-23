//! AUTH2 browser sessions (D44–D46), the portable half.
//!
//! - The session cookie is resolved by a [`SessionResolver`]: natively the
//!   auth child's internal `POST /internal/session` (cached 30 s by the
//!   cookie's SHA-256), on Workers a service binding.
//! - On first use a session is linked to a directory user (D45) and, on
//!   every request, authorizes as `auth_links::session_credential`
//!   ([`link`]).
//! - Cookie-authenticated unsafe methods need `Origin` (or
//!   `Sec-Fetch-Site: same-origin`) plus `X-Runbook-CSRF: 1` (D46).
//! - `POST /internal/can-register` answers the auth service's registration
//!   hook ([`can_register`]); the host decides who may reach it.
use crate::{
    Credential, Request, ResolvedSession,
    backends::{Backends, new_id},
    http::{Headers, bearer},
    problem::Response,
};
use runbook_core::{
    auth_links::{AuthLinkStore, LinkRequest, session_credential},
    directory::{DirectoryError, DirectoryStore, email_key},
    directory_operations::DirectoryOperations,
};
use serde_json::{Value, json};
use std::future::Future;

pub const CAN_REGISTER: &str = "/internal/can-register";

/// The one place the cookie's name is decided (D46): `__Secure-` on an
/// https origin, plain otherwise (a loopback http pilot). The auth service
/// must set exactly this name (Better Auth's `runbook` cookie prefix).
pub fn cookie_name(origin: &str) -> &'static str {
    if origin.starts_with("https://") {
        "__Host-runbook.session_token"
    } else {
        "runbook.session_token"
    }
}

/// Sign-in gate fix 7: the https name before `__Host-`, accepted for one
/// release (the auth service reads it as the new name and expires it when
/// it sets the new one).
pub const LEGACY_COOKIE: &str = "__Secure-runbook.session_token";

/// The session cookie's value: the current name, or on https the legacy
/// one when the current name is absent.
pub fn find_session_cookie(headers: &Headers, origin: &str) -> Option<String> {
    session_cookie(headers, cookie_name(origin)).or_else(|| {
        origin
            .starts_with("https://")
            .then(|| session_cookie(headers, LEGACY_COOKIE))
            .flatten()
    })
}

/// The session cookie's value from the request's `Cookie` headers.
pub fn session_cookie(headers: &Headers, name: &str) -> Option<String> {
    headers
        .get_all_str("cookie")
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, v)| *k == name && !v.is_empty())
        .map(|(_, v)| v.to_owned())
}

/// D46: may a cookie-authenticated request with this method proceed?
/// Safe methods always; unsafe ones need `X-Runbook-CSRF: 1` and an
/// `Origin` equal to the server's, or, with no `Origin`,
/// `Sec-Fetch-Site: same-origin`.
pub fn csrf_ok(method: &str, headers: &Headers, origin: &str) -> bool {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return true;
    }
    if headers.one("x-runbook-csrf") != Some("1") {
        return false;
    }
    match headers.one("origin") {
        Some(o) => o == origin,
        None => !headers.contains("origin") && headers.one("sec-fetch-site") == Some("same-origin"),
    }
}

/// Paths that never authenticate with a cookie (D46): agent transports.
pub fn ignores_cookies(path: &str) -> bool {
    ["/mcp", "/hooks"].iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// The auth service did not answer (503 `auth.unavailable`).
#[derive(Debug, PartialEq, Eq)]
pub struct Unavailable;

/// D44: who a session cookie belongs to. No `Send` bound; a host caches as
/// it likes (the documented bound is 30 s) and evicts on sign-out.
pub trait SessionResolver {
    /// The app origin: CSRF compares `Origin` with it, and it names the
    /// cookie ([`cookie_name`]).
    fn origin(&self) -> &str;
    /// The live, verified session a cookie names, or `None` for an unknown,
    /// expired or unverified one.
    fn resolve(
        &self,
        cookie: &str,
    ) -> impl Future<Output = Result<Option<ResolvedSession>, Unavailable>>;
}

/// D46: how a request authenticates, decided from its headers. An
/// `Authorization` header (even a malformed one) means bearer only and the
/// cookie is ignored; `/mcp` and `/hooks` never use cookies, and the public
/// discovery documents need no identity (so they stay up while the auth
/// service is down). A cookie on an unsafe method must pass the CSRF check
/// first (403 `auth.csrf`). Returns the credential and the raw cookie (for
/// stream rechecks), or a refusal.
pub async fn credential<S: SessionResolver>(
    sessions: Option<&S>,
    method: &str,
    path: &str,
    headers: &Headers,
    id: &str,
) -> Result<(Option<Credential>, Option<String>), Response> {
    if headers.contains("authorization") {
        return Ok((bearer(headers).map(Credential::Bearer), None));
    }
    let Some(sessions) = sessions else {
        return Ok((None, None));
    };
    if ignores_cookies(path) || matches!(path, "/.well-known/runbook" | "/v1/meta") {
        return Ok((None, None));
    }
    let Some(cookie) = find_session_cookie(headers, sessions.origin()) else {
        return Ok((None, None));
    };
    if !csrf_ok(method, headers, sessions.origin()) {
        return Err(Response::error(403, "auth.csrf", id));
    }
    match sessions.resolve(&cookie).await {
        Ok(Some(session)) => Ok((Some(Credential::Session(session)), Some(cookie))),
        Ok(None) => Ok((None, None)),
        Err(_) => Err(Response::error(503, "auth.unavailable", id)),
    }
}

/// What the auth service's `POST /internal/session` returns:
/// `{auth_user_id, email, email_verified, session_id, expires_at}`, with
/// `expires_at` in epoch milliseconds or an RFC 3339 UTC string. Its OAuth
/// introspection (MCP2) answers the same plus `scope`.
pub fn parse_session(v: &Value) -> Option<ResolvedSession> {
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 512)
            .map(str::to_owned)
    };
    let expires_at_ms = match v.get("expires_at")? {
        Value::Number(n) => n.as_i64()?,
        Value::String(s) => rfc3339_ms(s)?,
        _ => return None,
    };
    // MCP2 re-check 3: an OAuth answer (it carries `scope` or `client_id`;
    // a browser session carries neither) always sets `scopes`, even to "",
    // so it can never read as a browser session; a scope that isn't a
    // bounded string refuses the answer instead of dropping it.
    let scopes = if v.get("scope").is_some() || v.get("client_id").is_some() {
        match v.get("scope") {
            None | Some(Value::Null) => Some(String::new()),
            Some(Value::String(s)) if s.len() <= 512 => Some(s.clone()),
            Some(_) => return None,
        }
    } else {
        None
    };
    let mfa_method = v
        .get("mfa_method")
        .and_then(|method| serde_json::from_value(method.clone()).ok());
    let mfa_verified_at = mfa_method
        .and_then(|_: MfaMethod| v.get("mfa_verified_at")?.as_i64())
        .filter(|time| *time > 0);
    Some(ResolvedSession {
        auth_user_id: text("auth_user_id")?,
        email: text("email")?,
        email_verified: v.get("email_verified")?.as_bool()?,
        name: text("name")
            .map(|n| n.trim().chars().take(100).collect::<String>())
            .filter(|n| !n.is_empty()),
        picture_url: text("image").filter(|url| url.starts_with("https://")),
        session_id: text("session_id")?,
        expires_at_ms,
        mfa_verified_at,
        mfa_method,
        mfa_enrolled: v.get("mfa_enrolled").and_then(Value::as_bool),
        principal: None,
        scopes,
    })
}

/// `YYYY-MM-DDTHH:MM:SS[.fff]Z` (or `+00:00`) to epoch milliseconds.
pub fn rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00"))?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|p| p.parse::<i64>().ok());
    let (h, min, sec) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || min > 59 || sec > 60 {
        return None;
    }
    let millis: i64 = format!("{frac:0<3}").get(..3)?.parse().ok()?;
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + min) * 60_000 + sec * 1000 + millis)
}

fn directory_failure(e: DirectoryError) -> (u16, &'static str) {
    match e {
        DirectoryError::Backend(_) => (503, "storage.unavailable"),
        _ => (403, "auth.forbidden"),
    }
}

/// D45 first use, then D46 principal: find or record the session's link,
/// then build the principal from the current directory. A session with no
/// link and nothing to link it with (no member with that verified email, no
/// pending invitation) is refused; the auth service's registration hook
/// normally stops that person earlier.
pub async fn link<B: Backends>(
    backends: &B,
    session: &mut ResolvedSession,
) -> Result<(), (u16, &'static str)> {
    let unavailable = (503, "storage.unavailable");
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(|_| unavailable)?;
    let mut found = directory
        .auth_link(&session.auth_user_id)
        .await
        .map_err(directory_failure)?;
    if found.is_none() {
        email_key(&session.email).map_err(|_| (403, "auth.forbidden"))?;
        let invitations = directory
            .invitations_for_email(&session.email, now)
            .await
            .map_err(directory_failure)?;
        let existing = directory
            .user_by_verified_email(&session.email)
            .await
            .map_err(directory_failure)?;
        let (org, invitation) = match (invitations.first(), &existing) {
            (Some(invitation), _) => (invitation.org_id.clone(), Some(invitation.id.clone())),
            (None, Some(user)) => (
                backends
                    .active_orgs_of_user(&user.id)
                    .await
                    .map_err(directory_failure)?
                    .into_iter()
                    .next()
                    .ok_or((403, "auth.forbidden"))?,
                None,
            ),
            (None, None) => return Err((403, "auth.forbidden")),
        };
        let user_id = match &existing {
            Some(user) => user.id.clone(),
            None => new_id(backends, "usr_", now)
                .ok()
                .and_then(|id| id.parse().ok())
                .ok_or((503, "server.entropy"))?,
        };
        // The sign-in's own name (Google's), else the email's first part.
        let name = session.name.clone().unwrap_or_else(|| {
            session
                .email
                .split('@')
                .next()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or("user")
                .to_owned()
        });
        let request = LinkRequest {
            auth_user_id: session.auth_user_id.clone(),
            verified_email: session.email.clone(),
            user_id,
            name,
            invitation,
            now_ms: now,
        };
        found = Some(match directory.link_identity(&org, request).await {
            Ok(link) => link,
            // A concurrent first use linked it; read the winner.
            Err(DirectoryError::Duplicate) => directory
                .auth_link(&session.auth_user_id)
                .await
                .map_err(directory_failure)?
                .ok_or((403, "auth.forbidden"))?,
            Err(e) => return Err(directory_failure(e)),
        });
        // A Google photo becomes the person's picture, unless they have one.
        if let (Some(url), Some(link)) = (&session.picture_url, &found) {
            use runbook_core::pictures::{Picture, PictureOwner, PictureStore};
            let owner = PictureOwner::User(link.user_id.clone());
            if directory
                .picture(&owner)
                .await
                .map_err(directory_failure)?
                .is_none()
            {
                // Best effort: a failed write leaves the initials.
                let _ = directory
                    .set_picture(
                        &owner,
                        Some(Picture {
                            content_type: String::new(),
                            bytes: Vec::new(),
                            source_url: Some(url.clone()),
                            updated_at_ms: now,
                        }),
                    )
                    .await;
            }
        }
    }
    let link = found.ok_or((403, "auth.forbidden"))?;
    let snapshot = directory
        .snapshot(&link.org_id)
        .await
        .map_err(directory_failure)?;
    let mut principal = session_credential(
        &snapshot,
        &link.user_id,
        &session.session_id,
        session.expires_at_ms,
    )
    .ok_or((403, "auth.forbidden"))?;
    // MCP2: an OAuth access token authorizes as an agent of its user, with
    // the granted scopes narrowed to the user's own (never wider than the
    // role).
    if let Some(granted) = &session.scopes {
        principal.scopes = principal
            .scopes
            .iter()
            .copied()
            .filter(|s| {
                let name = serde_json::to_value(s).ok();
                granted
                    .split(' ')
                    .any(|g| name.as_ref().and_then(Value::as_str) == Some(g))
            })
            .collect();
        if let runbook_core::credentials::CredentialPrincipal::User { actor, .. } =
            &mut principal.principal
        {
            *actor = runbook_core::credentials::ActorKind::Agent;
        }
    } else {
        use runbook_core::credentials::Scope;
        if snapshot.projects.keys().any(|project| {
            snapshot
                .project_role(project, &link.user_id)
                .is_some_and(Scope::browser_runner_admin)
        }) {
            principal.scopes.insert(Scope::RunnerAdmin);
        }
    }
    session.principal = Some(Box::new(principal));
    Ok(())
}

/// D45: an email's domain passes when no domains are configured or it
/// equals one of them (case-insensitive, no subdomain matching).
pub fn domain_allowed(allowed_domains: &[String], email: &str) -> bool {
    let Some((_, domain)) = email.trim().rsplit_once('@') else {
        return false;
    };
    allowed_domains.is_empty()
        || allowed_domains
            .iter()
            .any(|d| d.to_lowercase() == domain.to_lowercase())
}

/// D45 `POST /internal/can-register {email}` → `{allowed}`: the domain is
/// allowed and either a directory user holds the email as verified or a
/// pending invitation names it.
pub async fn can_register<B: Backends>(
    backends: &B,
    allowed_domains: &[String],
    r: Request,
) -> Response {
    let id = r.request_id.clone();
    if r.method != "POST" {
        return Response::error(405, "request.method_not_allowed", &id);
    }
    let Some(email) = r
        .body
        .get("email")
        .and_then(Value::as_str)
        .and_then(|e| email_key(e).ok())
    else {
        return Response::error(422, "request.invalid", &id);
    };
    let directory = backends.directory();
    let allowed = async {
        if !domain_allowed(allowed_domains, &email) {
            return Ok(false);
        }
        if directory.user_by_verified_email(&email).await?.is_some() {
            return Ok(true);
        }
        let now = directory.current_time_ms().await?;
        Ok::<_, DirectoryError>(
            !directory
                .invitations_for_email(&email, now)
                .await?
                .is_empty(),
        )
    }
    .await;
    match allowed {
        Ok(allowed) => Response::json(200, json!({"allowed": allowed}), &id),
        Err(_) => Response::error(503, "storage.unavailable", &id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(pairs: &[(&str, &str)]) -> Headers {
        pairs.iter().copied().collect()
    }
    #[test]
    fn rfc3339_and_epoch_expiry_parse() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            rfc3339_ms("2026-11-12T10:20:30.5Z"),
            Some(1_794_478_830_500)
        );
        assert_eq!(
            rfc3339_ms("2026-11-12T10:20:30.123+00:00"),
            Some(1_794_478_830_123)
        );
        assert_eq!(rfc3339_ms("2026-11-12T10:20:30+02:00"), None);
        assert_eq!(rfc3339_ms("2026-13-12T10:20:30Z"), None);
        let v = json!({"auth_user_id":"a","email":"e@example.test","email_verified":true,"session_id":"s","expires_at":5});
        assert_eq!(parse_session(&v).unwrap().expires_at_ms, 5);
        let mut missing = v.clone();
        missing.as_object_mut().unwrap().remove("session_id");
        assert!(parse_session(&missing).is_none());
    }
    #[test]
    fn an_oauth_answer_never_reads_as_a_browser_session() {
        // MCP2 re-check 3: a browser session has no `scope`/`client_id`; an
        // OAuth answer always keeps `scopes` set, even when empty, and one
        // with a scope that isn't a bounded string is refused outright.
        let base = json!({"auth_user_id":"a","email":"e@example.test","email_verified":true,"session_id":"oauth:t","expires_at":5});
        assert_eq!(parse_session(&base).unwrap().scopes, None);
        let with = |extra: serde_json::Value| {
            let mut v = base.clone();
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            parse_session(&v)
        };
        let empty = with(json!({"scope": "", "client_id": "c"})).unwrap();
        assert_eq!(empty.scopes.as_deref(), Some(""));
        let client_only = with(json!({"client_id": "c"})).unwrap();
        assert_eq!(client_only.scopes.as_deref(), Some(""));
        assert!(with(json!({"scope": "x".repeat(513), "client_id": "c"})).is_none());
        assert!(with(json!({"scope": 7, "client_id": "c"})).is_none());
        let real = with(json!({"scope": "runbooks:read", "client_id": "c"})).unwrap();
        assert_eq!(real.scopes.as_deref(), Some("runbooks:read"));
    }
    #[test]
    fn cookie_name_follows_the_origin_scheme() {
        // Gate fix 7: __Host- on https (Secure, Path=/, no Domain).
        assert_eq!(
            cookie_name("https://runbooks.example"),
            "__Host-runbook.session_token"
        );
        assert_eq!(
            cookie_name("http://127.0.0.1:8080"),
            "runbook.session_token"
        );
        let headers = h(&[
            ("cookie", "a=1; runbook.session_token=tok.sig"),
            ("cookie", "other=2"),
        ]);
        assert_eq!(
            session_cookie(&headers, "runbook.session_token").as_deref(),
            Some("tok.sig")
        );
        assert_eq!(
            session_cookie(&headers, "__Secure-runbook.session_token"),
            None
        );
        // The pre-__Host- name is accepted for one release; the new one wins.
        let https = "https://runbooks.example";
        let legacy = h(&[("cookie", "__Secure-runbook.session_token=old.sig")]);
        assert_eq!(
            find_session_cookie(&legacy, https).as_deref(),
            Some("old.sig")
        );
        let both = h(&[(
            "cookie",
            "__Secure-runbook.session_token=old.sig; __Host-runbook.session_token=new.sig",
        )]);
        assert_eq!(
            find_session_cookie(&both, https).as_deref(),
            Some("new.sig")
        );
        assert_eq!(find_session_cookie(&legacy, "http://127.0.0.1:8080"), None);
    }
    #[test]
    fn csrf_needs_header_and_same_origin() {
        let origin = "https://runbooks.example";
        assert!(csrf_ok("GET", &h(&[]), origin));
        assert!(!csrf_ok("POST", &h(&[]), origin));
        assert!(!csrf_ok("POST", &h(&[("x-runbook-csrf", "1")]), origin));
        assert!(!csrf_ok("POST", &h(&[("origin", origin)]), origin));
        assert!(csrf_ok(
            "DELETE",
            &h(&[("origin", origin), ("x-runbook-csrf", "1")]),
            origin
        ));
        assert!(!csrf_ok(
            "POST",
            &h(&[("origin", "https://evil.example"), ("x-runbook-csrf", "1")]),
            origin
        ));
        assert!(!csrf_ok(
            "POST",
            &h(&[
                ("origin", "null"),
                ("x-runbook-csrf", "1"),
                ("sec-fetch-site", "same-origin")
            ]),
            origin
        ));
        assert!(csrf_ok(
            "PATCH",
            &h(&[("sec-fetch-site", "same-origin"), ("x-runbook-csrf", "1")]),
            origin
        ));
        assert!(!csrf_ok(
            "PATCH",
            &h(&[("sec-fetch-site", "same-site"), ("x-runbook-csrf", "1")]),
            origin
        ));
        assert!(!csrf_ok(
            "POST",
            &h(&[("origin", origin), ("x-runbook-csrf", "true")]),
            origin
        ));
        assert!(!csrf_ok(
            "POST",
            &h(&[
                ("origin", origin),
                ("origin", origin),
                ("x-runbook-csrf", "1")
            ]),
            origin
        ));
        assert!(ignores_cookies("/mcp") && ignores_cookies("/hooks/x"));
        assert!(!ignores_cookies("/mcpx") && !ignores_cookies("/v1/me"));
    }
    #[test]
    fn domains_match_exactly_without_subdomains() {
        let allowed = vec!["Example.test".to_owned()];
        assert!(domain_allowed(&allowed, "a@example.TEST"));
        assert!(!domain_allowed(&allowed, "a@sub.example.test"));
        assert!(!domain_allowed(&allowed, "no-at-sign"));
        assert!(domain_allowed(&[], "a@anything.test"));
        // Gate fix 7: Unicode case folds like the auth service's toLowerCase.
        assert!(domain_allowed(
            &["Exämple.TEST".to_owned()],
            "a@EXÄMPLE.test"
        ));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MfaMethod {
    Passkey,
    Totp,
    BackupCode,
}
