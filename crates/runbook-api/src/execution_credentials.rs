//! B7b execution credential routes: mint, refresh and revoke (one-time
//! responses). The host supplies entropy and storage through [`Backends`].
//! Never log requests, entropy or pairs.
pub const ROUTES: &[(&str, &str)] = &[
    ("POST", "/v1/executions/{exe}/tokens"),
    ("POST", "/v1/executions/{exe}/tokens/refresh"),
    ("DELETE", "/v1/executions/{exe}/credentials/{family}"),
];
pub fn recognizes(path: &str) -> bool {
    ROUTES
        .iter()
        .any(|(_, p)| crate::directory::match_path(p, path).is_some())
}

use crate::{
    Request,
    backends::{Backends, new_id, org_store},
    executions::ExecutionApiError as Error,
    problem::Response,
};
use runbook_core::{
    catalog::{CatalogStore, TenantScope},
    credentials::*,
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    execution_credentials::*,
};
use runbook_model::*;
use serde_json::{Value, json};

pub fn failure(error: ExecutionCredentialError) -> Error {
    let code = error.code();
    Error::new(
        match code {
            "request.idempotency_key_required" => 428,
            "credentials.forbidden" => 403,
            "credentials.invalid" | "credentials.expired" | "credentials.revoked" => 401,
            "execution.not_found" => 404,
            "store.backend" => 503,
            _ => 409,
        },
        code,
    )
}
pub fn storage(_: impl std::fmt::Debug) -> Error {
    Error::new(503, "storage.unavailable")
}
fn forbidden() -> Error {
    Error::new(403, "auth.forbidden")
}
fn invalid() -> Error {
    Error::new(401, "auth.invalid_credential")
}
fn json_value(v: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(storage)
}
fn entropy<B: Backends>(backends: &B, now: i64) -> Result<MintEntropy, Error> {
    let mut access_random = [0; 32];
    let mut refresh_random = [0; 32];
    backends.fill_random(&mut access_random).map_err(storage)?;
    backends.fill_random(&mut refresh_random).map_err(storage)?;
    Ok(MintEntropy {
        family_id: new_id(backends, "fam_", now).map_err(storage)?,
        access_token_id: new_id(backends, "tok_", now)
            .map_err(storage)?
            .parse()
            .map_err(storage)?,
        refresh_token_id: new_id(backends, "tok_", now)
            .map_err(storage)?
            .parse()
            .map_err(storage)?,
        access_random,
        refresh_random,
    })
}
fn pair(pair: IssuedPair) -> ExecutionTokenPair {
    ExecutionTokenPair {
        family_id: pair.family_id,
        generation: pair.generation,
        access_token: pair.access.secret,
        refresh_token: pair.refresh.secret,
        expires_at_ms: pair.access.expires_at_ms,
        refresh_expires_at_ms: pair.refresh.expires_at_ms,
    }
}
pub struct Access {
    pub scope: TenantScope,
    pub record: ExecutionCredentialRecord,
    pub principal: ExecutionPrincipal,
}
pub async fn access<B: Backends>(backends: &B, secret: &str) -> Result<Access, Error> {
    let creds = backends.credentials();
    let directory = backends.directory();
    if parse_secret(secret).map_err(|_| invalid())?.0 != TokenKind::Execution {
        return Err(invalid());
    }
    let scope = backends
        .execution_scope(&secret_hash(secret))
        .await
        .map_err(storage)?
        .ok_or_else(invalid)?;
    let store = backends.execution(scope.clone());
    let now = directory.current_time_ms().await.map_err(storage)?;
    let principal = verify_execution_token(&store, &creds, &directory, secret, now)
        .await
        .map_err(failure)?;
    let record = store
        .credential_by_hash(&secret_hash(secret))
        .await
        .map_err(storage)?
        .ok_or_else(invalid)?;
    Ok(Access {
        scope,
        record,
        principal,
    })
}
pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok((status, body)) => Response::json(status, body, &r.request_id),
        Err(e) => {
            let mut out = Response::error(e.status, e.code, &r.request_id);
            out.body["errors"] = json!(e.diagnostics);
            out
        }
    }
}
async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<(u16, Value), Error> {
    let (method, params) = crate::execution_credentials::ROUTES
        .iter()
        .find_map(|(m, p)| crate::directory::match_path(p, &r.path).map(|params| (*m, params)))
        .ok_or_else(|| Error::new(404, "resource.not_found"))?;
    if r.method != method {
        return Err(Error::new(405, "request.method_not_allowed"));
    }
    let execution: ExecutionId = params["exe"]
        .parse()
        .map_err(|_| Error::new(404, "resource.not_found"))?;
    let creds = backends.credentials();
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(storage)?;
    if r.path.ends_with("/refresh") {
        // Refresh authenticates itself and deliberately never enters POST replay.
        crate::actor_requests::post_identity(r, "refresh")?;
        let request: RefreshRequest = serde_json::from_value(r.body.clone())
            .map_err(|_| Error::new(422, "request.invalid"))?;
        if parse_secret(&request.refresh_token)
            .map_err(|_| invalid())?
            .0
            != TokenKind::Refresh
        {
            return Err(invalid());
        }
        let scope = backends
            .execution_scope(&secret_hash(&request.refresh_token))
            .await
            .map_err(storage)?
            .ok_or_else(invalid)?;
        let store = backends.execution(scope);
        let row = store
            .credential_by_hash(&secret_hash(&request.refresh_token))
            .await
            .map_err(storage)?
            .ok_or_else(invalid)?;
        if row.execution_id != execution {
            return Err(forbidden());
        }
        return Ok((
            201,
            json_value(pair(
                refresh_execution_pair(
                    &store,
                    &creds,
                    &directory,
                    &request,
                    entropy(backends, now)?,
                )
                .await
                .map_err(failure)?,
            ))?,
        ));
    }
    // D46: a browser session never mints or revokes execution credentials.
    // D31 rechecks the issuing token on every use, so the launcher must hold
    // a stored PAT or service token.
    if r.is_session() {
        return Err(forbidden());
    }
    let secret = r.bearer().ok_or_else(invalid)?;
    let kind = parse_secret(secret).map_err(|_| invalid())?.0;
    if !matches!(kind, TokenKind::Personal | TokenKind::Service) {
        return Err(forbidden());
    }
    let issuer = creds
        .find_by_hash(&secret_hash(secret))
        .await
        .map_err(storage)?
        .filter(|c| c.kind == kind)
        .ok_or_else(invalid)?;
    let org_store = org_store(backends, &issuer.org_id);
    let project = org_store
        .execution_project(&issuer.org_id, &execution)
        .await
        .map_err(storage)?
        .ok_or_else(|| Error::new(404, "resource.not_found"))?;
    let snapshot = directory.snapshot(&issuer.org_id).await.map_err(storage)?;
    let principal =
        authorize(&issuer, &snapshot, now, &project, Scope::ExecutionsAct).map_err(|e| {
            if e == AuthError::Forbidden {
                forbidden()
            } else {
                invalid()
            }
        })?;
    let store = backends.execution(TenantScope {
        org_id: issuer.org_id.clone(),
        project_id: project.clone(),
    });
    creds
        .touch_last_used(&issuer.org_id, &issuer.id, now)
        .await
        .map_err(storage)?;
    if let Some(id) = params.get("family") {
        let family = store
            .credential_family(id)
            .await
            .map_err(storage)?
            .filter(|f| f.execution_id == execution)
            .ok_or_else(|| Error::new(404, "resource.not_found"))?;
        let same = match (&issuer.principal, &family.issuer) {
            (
                CredentialPrincipal::User {
                    user_id: a,
                    actor: a_kind,
                },
                CredentialPrincipal::User {
                    user_id: b,
                    actor: b_kind,
                },
            ) => a == b && a_kind == b_kind,
            (CredentialPrincipal::Service { .. }, CredentialPrincipal::Service { .. }) => {
                issuer.id == family.issuer_token_id
            }
            _ => false,
        };
        let admin = matches!(&principal,Principal::Human{user_id} if snapshot.project_role(&project,user_id)==Some(ProjectRole::Admin));
        if !same && !admin {
            return Err(forbidden());
        }
        revoke_execution_family(&store, &family)
            .await
            .map_err(failure)?;
        return Ok((200, Value::Null));
    }
    let key = match &principal {
        Principal::Human { user_id } => format!("human:{user_id}"),
        Principal::Agent { sponsor } => format!("agent:{sponsor}"),
        Principal::Service { token_id, .. } => format!("service:{token_id}"),
    };
    let replay = crate::actor_requests::post_identity(r, &key)?;
    // D20 identity is checked before decoding a changed body. Core repeats this
    // lookup in the atomic mint path, so racing requests cannot issue twice.
    if let Some(existing) = store
        .family_by_idempotency_key(&execution, &replay.key)
        .await
        .map_err(storage)?
    {
        return Err(Error::new(
            409,
            if existing.request_hash == replay.request_hash {
                "credentials.already_issued"
            } else {
                "request.idempotency_key_reused"
            },
        ));
    }

    let request: MintTokenRequest =
        serde_json::from_value(r.body.clone()).map_err(|_| Error::new(422, "request.invalid"))?;
    if request.session_id.trim().is_empty() {
        return Err(Error::new(422, "request.invalid"));
    }
    // CF12b: the session is copied into the credential's row and every
    // event the agent signs.
    crate::field_caps::text(
        "/session_id",
        &request.session_id,
        crate::field_caps::NAME_BYTES,
    )?;
    Ok((
        201,
        json_value(pair(
            mint_execution_pair(
                &store,
                &creds,
                &directory,
                &issuer,
                &execution,
                &request,
                &replay.key,
                replay.request_hash,
                entropy(backends, now)?,
            )
            .await
            .map_err(failure)?,
        ))?,
    ))
}
