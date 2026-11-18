//! AUTH2 (D43): `/api/auth/*` is proxied to the auth child on loopback, so
//! the browser sees one origin. The proxy never follows redirects (OAuth
//! flows send the browser on), passes every `Set-Cookie`, drops every
//! client-address header and reports the client as `x-runbook-client-ip`
//! (the TCP peer, or the client a trusted proxy names:
//! [`crate::client_ip`]; auth/README.md), strips the internal secret header,
//! refuses traversal paths, evicts the session cache entry on sign-out (D44)
//! and clears the cache on any successful revoke.
use crate::session::Sessions;
use axum::body::{Body, to_bytes};
use http::{HeaderName, HeaderValue, Request, Response, header};
use std::{io::Read, net::IpAddr, sync::Arc, time::Duration};

pub const PREFIX: &str = "/api/auth";
/// Sign-in requests are small JSON; OAuth callbacks are GETs.
const REQUEST_BYTES: usize = runbook_api::http::AUTH_BODY_BYTES as usize;
const RESPONSE_BYTES: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

pub fn recognizes(path: &str) -> bool {
    path.strip_prefix(PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Gate fix 5: a path the proxy forwards: no `..` segment, no empty
/// segment (`//`), no backslash and no percent-encoded dot, slash or
/// backslash, which a server behind it might decode into one of those.
fn plain_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    !(path.contains('\\')
        || path.contains("//")
        || path
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
        || ["%2e", "%2f", "%5c"].iter().any(|e| lower.contains(e)))
}

/// Gate fix 6: Better Auth routes that end sessions other than the
/// caller's (which the proxy cannot name), so a success clears the cache.
fn revokes_sessions(method: &http::Method, path: &str) -> bool {
    method == http::Method::POST
        && matches!(
            path,
            "/api/auth/revoke-session"
                | "/api/auth/revoke-sessions"
                | "/api/auth/revoke-other-sessions"
                // MCP2 re-check 4: a revoked OAuth token stops at once here.
                | "/api/auth/oauth2/revoke"
        )
}

/// Never forwarded from the client: hop-by-hop headers, anything that
/// claims a client address or the internal secret.
fn dropped(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
            | "x-runbook-internal-secret"
            | "x-runbook-client-ip"
            | "x-runbook-auth-nonce"
            | "x-runbook-auth-proof"
            | "forwarded"
            | "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-proto"
            | "x-forwarded-port"
            | "x-forwarded-server"
            | "x-real-ip"
            | "x-client-ip"
            | "x-cluster-client-ip"
            | "true-client-ip"
            | "fastly-client-ip"
            | "cf-connecting-ip"
            | "cf-connecting-ipv6"
    )
}

fn problem(status: u16, code: &str, id: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/problem+json")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-request-id", id)
        .body(Body::from(
            serde_json::json!({"type":"about:blank","status":status,"code":code,"request_id":id})
                .to_string(),
        ))
        .expect("fixed response")
}

pub async fn forward(
    sessions: Arc<Sessions>,
    request: Request<Body>,
    client: Option<IpAddr>,
    id: &str,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    if !plain_path(parts.uri.path()) {
        return problem(404, "resource.not_found", id);
    }
    let Ok(bytes) = to_bytes(body, REQUEST_BYTES).await else {
        return problem(413, "request.body_too_large", id);
    };
    let cookie = sessions.cookie(&parts.headers);
    let sign_out = parts.uri.path() == "/api/auth/sign-out";
    let revoke = revokes_sessions(&parts.method, parts.uri.path());
    if sign_out && let Some(cookie) = &cookie {
        sessions.evict(cookie);
    }
    // Gate fix 2: no ready child, no call; a fresh nonce on the call.
    let (Some(base), Some(nonce)) = (sessions.child().base_url(), sessions.child().nonce()) else {
        return problem(502, "auth.unavailable", id);
    };
    let url = format!(
        "{}{}",
        base,
        parts
            .uri
            .path_and_query()
            .map_or(parts.uri.path(), |p| p.as_str())
    );
    let mut headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .filter(|(name, _)| !dropped(name.as_str()))
        .filter_map(|(name, value)| {
            Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
        })
        .collect();
    if let Some(ip) = client {
        headers.push((crate::auth_child::CLIENT_IP_HEADER.into(), ip.to_string()));
    }
    headers.push((crate::auth_child::NONCE_HEADER.into(), nonce.clone()));
    let agent = sessions.agent().clone();
    let method = parts.method.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let mut call = agent.request(&method, &url).timeout(TIMEOUT);
        for (name, value) in &headers {
            call = call.set(name, value);
        }
        let response = match if bytes.is_empty() {
            call.call()
        } else {
            call.send_bytes(&bytes)
        } {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(_) => return None,
        };
        let status = response.status();
        let mut names = response.headers_names();
        names.sort();
        names.dedup();
        let headers: Vec<(String, String)> = names
            .into_iter()
            .flat_map(|name| {
                response
                    .all(&name)
                    .into_iter()
                    .map(|v| (name.clone(), v.to_owned()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut body = Vec::new();
        response
            .into_reader()
            .take(RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .ok()?;
        (body.len() as u64 <= RESPONSE_BYTES).then_some((status, headers, body))
    })
    .await
    .ok()
    .flatten();
    if sign_out && let Some(cookie) = &cookie {
        sessions.evict(cookie);
    }
    let Some((status, response_headers, body)) = result else {
        return problem(502, "auth.unavailable", id);
    };
    // Gate fix 2: only the child that holds the per-boot secret answers.
    let proof = response_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(crate::auth_child::PROOF_HEADER))
        .map(|(_, value)| value.as_str());
    if !sessions.child().proof_ok(&nonce, proof) {
        return problem(502, "auth.unavailable", id);
    }
    if revoke && (200..300).contains(&status) {
        sessions.clear();
    }
    let mut builder = Response::builder().status(status);
    for (name, value) in response_headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "upgrade"
                | "content-length"
                | "trailer"
                | "x-runbook-auth-proof"
        ) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(lower.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            builder = builder.header(name, value);
        }
    }
    let mut response = builder
        .body(Body::from(body))
        .unwrap_or_else(|_| problem(502, "auth.unavailable", id));
    let headers = response.headers_mut();
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(id) {
        headers.insert("x-request-id", value);
    }
    response
}
