//! AUTH3 (D47): the RFC 8628 device authorization grant for `runbook login`.
//!
//! - `POST /v1/device/code` (no credential, at most 20 per hour per IP):
//!   a high-entropy device code for the CLI and a short user code for the
//!   person (8 characters from an alphabet without vowels or look-alikes).
//! - `POST /v1/device/token` (no credential): RFC 8628 polling. Until a
//!   decision: 400 `device.authorization_pending`, `device.slow_down`
//!   (interval +5 s, at most 60 s), `device.expired_token` or
//!   `device.access_denied`, each also naming the RFC error in `error`.
//!   The one poll that sees an approval gets an ordinary `trb_pat_`
//!   (hash-only, listed, revocable; default 90 days).
//! - `POST /v1/device/lookup` (a browser session plus CSRF): what the
//!   approval page shows before a decision: the client name (the client's
//!   unverified claim), the address that asked, and the scopes asked for.
//!   A wrong code is a miss, like approve. The page must never approve or
//!   deny from its query string alone: `?user_code=` only fills the form,
//!   and a person submits it (sign-in gate fix 3).
//! - `POST /v1/device/approve` (a browser session plus CSRF): approve or
//!   deny; scopes, projects and expiry only narrow what the CLI asked for
//!   and what the approver's roles allow.
//!
//! Codes are stored as SHA-256 hashes only. Without sign-in configured
//! (no RUNBOOK_AUTH_SECRET) the routes do not exist.
use crate::{
    Request,
    backends::{Backends, new_id},
    problem::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{
    credentials::{
        ActorKind, CredentialPrincipal, CredentialRecord, Scope, TokenKind,
        directory_project_visible, encode_secret,
        issuance::{CLOCK_SKEW_MS, MAX_PERSONAL_LIFETIME_MS, TokenMetadata},
        secret_hash,
    },
    device_codes::{
        DEVICE_CODE_TTL_MS, DEVICE_POLL_INTERVAL_MS, DEVICE_TOKEN_TTL_MS, DeviceCodeRecord,
        DeviceCodeState, DeviceCodeStore, DeviceDecision, DeviceGrant, DevicePoll,
    },
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
};
use runbook_model::{
    DeviceApproveRequest, DeviceCodeRequest, DeviceCodeView, DeviceLookupRequest, DeviceLookupView,
    DeviceTokenRequest, DeviceTokenView, ProjectId, TokenId,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const CODE: &str = "/v1/device/code";
pub const TOKEN: &str = "/v1/device/token";
pub const APPROVE: &str = "/v1/device/approve";
pub const LOOKUP: &str = "/v1/device/lookup";
pub const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Shelf's alphabet: consonants and digits, no vowels (no words), no
/// 0/O/1/I/L look-alikes. 28 symbols, 8 of them: about 38 bits.
pub const USER_CODE_ALPHABET: &[u8; 28] = b"BCDFGHJKLMNPQRSTVWXZ23456789";
/// Shelf's limits: codes per IP per hour, approval misses per person per minute.
pub const CODES_PER_HOUR: u32 = 20;
pub const MISSES_PER_MINUTE: u32 = 5;
const MAX_CLIENT_NAME: usize = 100;

pub fn recognizes(method: &str, path: &str) -> bool {
    method == "POST" && matches!(path, CODE | TOKEN | APPROVE | LOOKUP)
}

/// A uniform 8-character user code (rejection sampling).
fn user_code<B: Backends>(backends: &B) -> Result<String, ()> {
    let mut out = String::with_capacity(8);
    while out.len() < 8 {
        let mut bytes = [0u8; 16];
        backends.fill_random(&mut bytes).map_err(|_| ())?;
        for b in bytes {
            // 252 is the largest multiple of 28 below 256.
            if b < 252 && out.len() < 8 {
                out.push(USER_CODE_ALPHABET[usize::from(b % 28)] as char);
            }
        }
    }
    Ok(out)
}
/// What a person types, normalized: case-folded, separators dropped.
pub fn normalize_user_code(code: &str) -> Option<String> {
    let code: String = code
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == 8 && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b))).then_some(code)
}
fn display(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}
fn scope_name(scope: &Scope) -> String {
    serde_json::to_value(scope)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn parse_scopes(names: &[String]) -> Option<BTreeSet<Scope>> {
    names
        .iter()
        .map(|n| serde_json::from_value::<Scope>(Value::String(n.clone())).ok())
        .collect()
}
fn rfc(status: u16, code: &'static str, error: &str, id: &str) -> Response {
    let mut response = Response::error(status, code, id);
    response.body["error"] = json!(error);
    response
}
fn storage(id: &str) -> Response {
    Response::error(503, "storage.unavailable", id)
}

/// The device routes; they exist only where sign-in is configured.
/// `origin` is the app origin the verification URI names.
pub async fn handle<B: Backends>(backends: &B, origin: &str, r: Request) -> Response {
    match r.path.as_str() {
        CODE => code(backends, origin, &r).await,
        TOKEN => token(backends, &r).await,
        LOOKUP => lookup(backends, &r).await,
        _ => approve(backends, &r).await,
    }
}

async fn code<B: Backends>(backends: &B, origin: &str, r: &Request) -> Response {
    let id = &r.request_id;
    let request: DeviceCodeRequest = if r.body.is_null() {
        DeviceCodeRequest::default()
    } else {
        match serde_json::from_value(r.body.clone()) {
            Ok(v) => v,
            Err(_) => return Response::error(422, "request.invalid", id),
        }
    };
    let client_name = request
        .client_name
        .as_deref()
        .map(str::trim)
        .unwrap_or("runbook CLI")
        .to_owned();
    if client_name.is_empty()
        || client_name.chars().count() > MAX_CLIENT_NAME
        || client_name.chars().any(char::is_control)
    {
        return Response::error(422, "request.invalid", id);
    }
    let requested = match &request.scopes {
        Some(names) => match parse_scopes(names).filter(|s| !s.is_empty()) {
            Some(scopes) => scopes,
            None => return Response::error(422, "request.invalid", id),
        },
        None => Scope::ALL.into(),
    };
    let directory = backends.directory();
    let Ok(now) = directory.current_time_ms().await else {
        return storage(id);
    };
    let credentials = backends.credentials();
    // A user-code collision is astronomically rare; try again a few times.
    for _ in 0..4 {
        let mut device = [0u8; 32];
        if backends.fill_random(&mut device).is_err() {
            return Response::error(503, "server.entropy", id);
        }
        let device_code = URL_SAFE_NO_PAD.encode(device);
        let Ok(user) = user_code(backends) else {
            return Response::error(503, "server.entropy", id);
        };
        let record = DeviceCodeRecord {
            device_code_hash: secret_hash(&device_code),
            user_code_hash: secret_hash(&user),
            client_name: client_name.clone(),
            requester_ip: r.client_ip.clone(),
            requested_scopes: requested.clone(),
            created_at_ms: now,
            expires_at_ms: now + DEVICE_CODE_TTL_MS,
            interval_ms: DEVICE_POLL_INTERVAL_MS,
            last_polled_at_ms: None,
            state: DeviceCodeState::Pending,
        };
        match credentials.insert_device_code(record).await {
            Ok(()) => {
                let uri = format!("{origin}/device");
                return Response::json(
                    200,
                    json!(DeviceCodeView {
                        device_code,
                        user_code: display(&user),
                        verification_uri_complete: format!("{uri}?user_code={}", display(&user)),
                        verification_uri: uri,
                        expires_in: (DEVICE_CODE_TTL_MS / 1000) as u32,
                        interval: (DEVICE_POLL_INTERVAL_MS / 1000) as u32,
                    }),
                    id,
                );
            }
            Err(runbook_core::credentials::CredentialError::Duplicate) => continue,
            Err(_) => return storage(id),
        }
    }
    storage(id)
}

async fn token<B: Backends>(backends: &B, r: &Request) -> Response {
    let id = &r.request_id;
    let Ok(request) = serde_json::from_value::<DeviceTokenRequest>(r.body.clone()) else {
        return Response::error(422, "request.invalid", id);
    };
    if request.grant_type != GRANT_TYPE {
        return rfc(400, "request.invalid", "unsupported_grant_type", id);
    }
    let directory = backends.directory();
    let credentials = backends.credentials();
    let Ok(now) = directory.current_time_ms().await else {
        return storage(id);
    };
    let Some(token_id) = new_id(backends, "tok_", now)
        .ok()
        .and_then(|t| t.parse::<TokenId>().ok())
    else {
        return Response::error(503, "server.entropy", id);
    };
    let hash = secret_hash(&request.device_code);
    let grant = match credentials.poll_device_code(&hash, &token_id, now).await {
        Ok(DevicePoll::Approved(grant)) => grant,
        Ok(DevicePoll::Pending) => {
            return rfc(
                400,
                "device.authorization_pending",
                "authorization_pending",
                id,
            );
        }
        Ok(DevicePoll::SlowDown { interval_ms }) => {
            let mut response = rfc(400, "device.slow_down", "slow_down", id);
            response.body["interval"] = json!(interval_ms / 1000);
            return response;
        }
        Ok(DevicePoll::Denied) => return rfc(400, "device.access_denied", "access_denied", id),
        Ok(DevicePoll::Expired | DevicePoll::AlreadyConsumed) => {
            return rfc(400, "device.expired_token", "expired_token", id);
        }
        Err(_) => return storage(id),
    };
    // The approval was consumed by this poll; mint the token it names.
    let client_name = backends
        .device_code_by_device_hash(&hash)
        .await
        .ok()
        .flatten()
        .map(|r| r.client_name)
        .unwrap_or_else(|| "runbook CLI".into());
    let mut random = [0u8; 32];
    if backends.fill_random(&mut random).is_err() {
        return Response::error(503, "server.entropy", id);
    }
    let secret = encode_secret(TokenKind::Personal, random);
    let record = CredentialRecord {
        id: token_id.clone(),
        kind: TokenKind::Personal,
        hash: secret_hash(&secret),
        principal: CredentialPrincipal::User {
            user_id: grant.user_id.clone(),
            actor: ActorKind::Human,
        },
        org_id: grant.org_id.clone(),
        scopes: grant.scopes.clone(),
        projects: grant.projects.clone(),
        expires_at: Some(grant.token_expires_at_ms),
        revoked_at: None,
        last_used_at: None,
    };
    let metadata = TokenMetadata {
        name: format!("Device login: {client_name}")
            .chars()
            .take(100)
            .collect(),
        created_at: Some(now),
        created_by: Some(grant.user_id.clone()),
        owner: None,
        fresh_mfa_at_issue: grant.fresh_mfa_at_issue,
    };
    if backends
        .token_insert_issued(&record, &metadata)
        .await
        .is_err()
    {
        return storage(id);
    }
    Response::json(
        200,
        json!(DeviceTokenView {
            access_token: secret,
            token_type: "bearer".into(),
            expires_in: u64::try_from((grant.token_expires_at_ms - now) / 1000).unwrap_or(0),
            scope: grant
                .scopes
                .iter()
                .map(scope_name)
                .collect::<Vec<_>>()
                .join(" "),
            token_id,
        }),
        id,
    )
}

/// D47: a signed-in person in a browser, never a token; the approver's
/// record and user, or the refusal.
async fn browser_person<B: Backends>(
    backends: &B,
    r: &Request,
) -> Result<(CredentialRecord, runbook_model::UserId), Response> {
    let id = &r.request_id;
    // MCP2 re-check 5: a browser session only, never an OAuth grant.
    if !r.is_browser_session() {
        return Err(if r.credential.is_none() {
            Response::error(401, "auth.required", id)
        } else {
            Response::error(403, "auth.forbidden", id)
        });
    }
    let credentials = backends.credentials();
    let approver = match r.credential_record(&credentials).await {
        Ok(record) => record,
        Err(e) => {
            let (status, code) = e.status();
            return Err(Response::error(status, code, id));
        }
    };
    let CredentialPrincipal::User {
        user_id,
        actor: ActorKind::Human,
    } = &approver.principal
    else {
        return Err(Response::error(403, "auth.forbidden", id));
    };
    let user_id = user_id.clone();
    Ok((approver, user_id))
}

/// Guessing codes costs a person a few misses a minute (Shelf's limit),
/// shared by lookup and approve.
async fn miss<B: Backends>(backends: &B, user_id: &runbook_model::UserId, id: &str) -> Response {
    match backends
        .rate_limit(&format!("miss:{user_id}"), 60_000, MISSES_PER_MINUTE)
        .await
    {
        Ok(()) => Response::error(404, "resource.not_found", id),
        Err(limited) => limited.response(id),
    }
}

/// Sign-in gate fix 3: the pending code a person is about to decide.
async fn lookup<B: Backends>(backends: &B, r: &Request) -> Response {
    let id = &r.request_id;
    let (_, user_id) = match browser_person(backends, r).await {
        Ok(found) => found,
        Err(refused) => return refused,
    };
    let Ok(request) = serde_json::from_value::<DeviceLookupRequest>(r.body.clone()) else {
        return Response::error(422, "request.invalid", id);
    };
    let Ok(now) = backends.directory().current_time_ms().await else {
        return storage(id);
    };
    let Some(code) = normalize_user_code(&request.user_code) else {
        return miss(backends, &user_id, id).await;
    };
    match backends
        .credentials()
        .device_code_by_user_code(&secret_hash(&code))
        .await
    {
        Ok(Some(record))
            if record.state == DeviceCodeState::Pending && now < record.expires_at_ms =>
        {
            Response::json(
                200,
                json!(DeviceLookupView {
                    user_code: display(&code),
                    client_name: record.client_name,
                    client_name_verified: false,
                    requester_ip: record.requester_ip,
                    requested_scopes: record.requested_scopes.iter().map(scope_name).collect(),
                    created_at_ms: record.created_at_ms,
                    expires_at_ms: record.expires_at_ms,
                }),
                id,
            )
        }
        Ok(_) => miss(backends, &user_id, id).await,
        Err(_) => storage(id),
    }
}

async fn approve<B: Backends>(backends: &B, r: &Request) -> Response {
    let id = &r.request_id;
    let (approver, user_id) = match browser_person(backends, r).await {
        Ok(found) => found,
        Err(refused) => return refused,
    };
    let user_id = &user_id;
    if r.idempotency_key
        .as_deref()
        .is_none_or(|k| k.trim().is_empty() || k.len() > 256)
    {
        return Response::error(428, "request.idempotency_key_required", id);
    }
    let credentials = backends.credentials();
    let Ok(request) = serde_json::from_value::<DeviceApproveRequest>(r.body.clone()) else {
        return Response::error(422, "request.invalid", id);
    };
    let directory = backends.directory();
    let Ok(now) = directory.current_time_ms().await else {
        return storage(id);
    };
    let miss = async || miss(backends, user_id, id).await;
    let Some(code) = normalize_user_code(&request.user_code) else {
        return miss().await;
    };
    let user_hash = secret_hash(&code);
    let pending = match credentials.device_code_by_user_code(&user_hash).await {
        Ok(Some(record))
            if record.state == DeviceCodeState::Pending && now < record.expires_at_ms =>
        {
            record
        }
        Ok(_) => return miss().await,
        Err(_) => return storage(id),
    };
    let decision = if request.approve {
        let Ok(snapshot) = directory.snapshot(&approver.org_id).await else {
            return storage(id);
        };
        let held = &approver.scopes;
        let scopes = match &request.scopes {
            Some(names) => {
                let Some(chosen) = parse_scopes(names).filter(|s| !s.is_empty()) else {
                    return Response::error(422, "request.invalid", id);
                };
                if !chosen.is_subset(&pending.requested_scopes) {
                    return Response::error(422, "request.invalid", id);
                }
                if !chosen.is_subset(held) {
                    return Response::error(403, "auth.forbidden", id);
                }
                chosen
            }
            None => pending
                .requested_scopes
                .intersection(held)
                .copied()
                .collect(),
        };
        if scopes.is_empty() {
            return Response::error(403, "auth.forbidden", id);
        }
        let projects = match &request.projects {
            None => None,
            Some(refs) if refs.is_empty() => return Response::error(422, "request.invalid", id),
            Some(refs) => {
                let mut out = BTreeSet::<ProjectId>::new();
                for reference in refs {
                    let found = reference
                        .parse::<ProjectId>()
                        .ok()
                        .filter(|p| snapshot.projects.contains_key(p))
                        .or_else(|| {
                            snapshot
                                .projects
                                .values()
                                .find(|p| &p.slug == reference)
                                .map(|p| p.id.clone())
                        });
                    match found {
                        Some(p) if directory_project_visible(&approver, &snapshot, &p) => {
                            out.insert(p);
                        }
                        _ => return Response::error(404, "resource.not_found", id),
                    }
                }
                Some(out)
            }
        };
        let expires = match request.expires_at_ms {
            None => now + DEVICE_TOKEN_TTL_MS,
            Some(t) if t > now && t <= now + MAX_PERSONAL_LIFETIME_MS + CLOCK_SKEW_MS => t,
            Some(_) => return Response::error(422, "request.invalid", id),
        };
        DeviceDecision::Approve(DeviceGrant {
            org_id: approver.org_id.clone(),
            user_id: user_id.clone(),
            scopes,
            projects,
            token_expires_at_ms: expires,
            decided_at_ms: now,
            fresh_mfa_at_issue: crate::mfa::fresh_at_issue(r, snapshot.security, now),
        })
    } else {
        DeviceDecision::Deny
    };
    match credentials
        .decide_device_code(&approver.org_id, &user_hash, decision, now)
        .await
    {
        Ok(true) => Response::json(204, Value::Null, id),
        Ok(false) => Response::error(404, "resource.not_found", id),
        Err(_) => storage(id),
    }
}
