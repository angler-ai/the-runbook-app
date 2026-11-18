//! B4.1/C2 `/v1/tokens`: create, list and revoke personal and service
//! tokens with `tokens:manage`. OS entropy stays in the host; only the
//! SHA-256 of a secret is stored, and the secret is returned once, here.
//! Rules live in `runbook_core::credentials::issuance`. Never log requests,
//! responses or secrets.
use crate::{
    Request,
    backends::{Backends, IssueOutcome, TokenIssue, new_id},
    problem::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{
    credentials::{issuance::*, *},
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
};
use runbook_model::{CreateTokenRequest, TOKEN_ROUTES, TokenId, TokenPage, TokenSecretView};
use serde_json::{Value, json};

type Error = (u16, &'static str);

pub fn recognizes(path: &str) -> bool {
    path == "/v1/tokens" || path.starts_with("/v1/tokens/")
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok((status, body)) => Response::json(status, body, &r.request_id),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

fn storage(_: impl std::fmt::Debug) -> Error {
    (503, "storage.unavailable")
}
fn auth(e: AuthError) -> Error {
    match e {
        AuthError::Forbidden => (403, "auth.forbidden"),
        _ => (401, "auth.invalid_credential"),
    }
}
fn issue_error(e: IssueError) -> Error {
    match e {
        IssueError::Invalid(_) => (422, "request.invalid"),
        IssueError::Forbidden => (403, "auth.forbidden"),
        IssueError::ExceedsCaller(_) => (403, "tokens.exceeds_caller"),
        IssueError::NotFound => (404, "resource.not_found"),
    }
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<(u16, Value), Error> {
    let mut recognized = false;
    let mut matched = None;
    for route in TOKEN_ROUTES {
        if let Some(p) = crate::directory::match_path(route.path, &r.path) {
            recognized = true;
            if route.method == r.method {
                matched = Some((route, p));
            }
        }
    }
    let (route, params) = matched.ok_or(if recognized {
        (405, "request.method_not_allowed")
    } else {
        (404, "resource.not_found")
    })?;
    let credentials = backends.credentials();
    let directory = backends.directory();
    let caller = r
        .credential_record(&credentials)
        .await
        .map_err(|e| e.status())?;
    let now = directory.current_time_ms().await.map_err(storage)?;
    let org = caller.org_id.clone();
    let snapshot = directory.snapshot(&org).await.map_err(storage)?;
    let manager = authorize_management(&caller, &snapshot, now).map_err(|e| match e {
        ManageError::MissingScope => (403, "auth.scope_missing"),
        ManageError::Auth(e) => auth(e),
    })?;
    credentials
        .touch_last_used(&org, &caller.id, now)
        .await
        .map_err(storage)?;
    match route.method {
        "GET" => list(backends, &manager, &org, r).await,
        "POST" => create(backends, &caller, &manager, &snapshot, now, r).await,
        _ => {
            // Unknown, foreign, revoked or not the caller's: all 404.
            let id: TokenId = params["id"]
                .parse()
                .map_err(|_| (404, "resource.not_found"))?;
            let target = credentials
                .find_by_id(&id)
                .await
                .map_err(storage)?
                .filter(|t| t.org_id == org && may_revoke(&manager, &snapshot, t))
                .ok_or((404, "resource.not_found"))?;
            if !credentials
                .revoke(&org, &target.id, now)
                .await
                .map_err(storage)?
            {
                return Err((404, "resource.not_found"));
            }
            Ok((204, Value::Null))
        }
    }
}

async fn create<B: Backends>(
    backends: &B,
    caller: &CredentialRecord,
    manager: &Manager,
    snapshot: &runbook_core::directory::DirectorySnapshot,
    now: i64,
    r: &Request,
) -> Result<(u16, Value), Error> {
    let key = r
        .idempotency_key
        .as_deref()
        .filter(|k| !k.trim().is_empty() && k.len() <= 256)
        .ok_or((428, "request.idempotency_key_required"))?;
    let principal = manager.principal();
    // D20: authorization (above) precedes replay; a replay never reissues.
    match backends
        .token_issue_replay(&caller.org_id, &principal, key, &r.body)
        .await
        .map_err(storage)?
    {
        Some(IssueOutcome::KeyReused) => return Err((409, "request.idempotency_key_reused")),
        Some(_) => return Err((409, "tokens.secret_already_issued")),
        None => {}
    }
    let request: CreateTokenRequest =
        serde_json::from_value(r.body.clone()).map_err(|_| (422, "request.invalid"))?;
    let id: TokenId = new_id(backends, "tok_", now)
        .map_err(storage)?
        .parse()
        .map_err(storage)?;
    let mut random = [0u8; 32];
    backends
        .fill_random(&mut random)
        .map_err(|_| (503, "server.entropy"))?;
    let kind = match request.kind {
        Some(runbook_model::ManagedTokenKind::Service) => TokenKind::Service,
        _ => TokenKind::Personal,
    };
    let secret = encode_secret(kind, random);
    let (record, metadata) = plan_issue(
        caller,
        manager,
        snapshot,
        now,
        &request,
        id,
        secret_hash(&secret),
    )
    .map_err(issue_error)?;
    let token = view(&record, &metadata);
    let issue = TokenIssue {
        record,
        metadata,
        principal,
        key: key.into(),
        payload: r.body.clone(),
        response: json!(token),
    };
    match backends.token_issue(&issue).await.map_err(storage)? {
        IssueOutcome::Issued => Ok((201, json!(TokenSecretView { token, secret }))),
        IssueOutcome::AlreadyIssued => Err((409, "tokens.secret_already_issued")),
        IssueOutcome::KeyReused => Err((409, "request.idempotency_key_reused")),
    }
}

async fn list<B: Backends>(
    backends: &B,
    manager: &Manager,
    org: &runbook_model::OrganizationId,
    r: &Request,
) -> Result<(u16, Value), Error> {
    let limit = r
        .query
        .get("limit")
        .map(|s| s.parse::<usize>().map_err(|_| (422, "request.invalid")))
        .transpose()?
        .unwrap_or(100);
    if !(1..=100).contains(&limit) {
        return Err((422, "request.invalid"));
    }
    // The cursor is the last token ID of the previous page.
    let after = r
        .query
        .get("cursor")
        .map(|c| {
            URL_SAFE_NO_PAD
                .decode(c)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .filter(|id| id.parse::<TokenId>().is_ok())
                .ok_or((400, "cursor.invalid"))
        })
        .transpose()?
        .unwrap_or_default();
    let mut items: Vec<_> = backends
        .tokens_of_org(org)
        .await
        .map_err(storage)?
        .into_iter()
        .filter(|(record, _)| listed(manager, record) && record.id.as_str() > after.as_str())
        .map(|(record, metadata)| view(&record, &metadata))
        .collect();
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = more
        .then(|| items.last().map(|t| URL_SAFE_NO_PAD.encode(t.id.as_str())))
        .flatten();
    Ok((200, json!(TokenPage { items, next_cursor })))
}
