//! Session-level MFA policy, common to both editions.
use crate::{
    Credential, Request, ResolvedSession, backends::Backends, directory::match_path,
    problem::Response, session::MfaMethod,
};
use runbook_core::{
    CommitResult,
    auth_links::AuthLinkStore,
    credentials::{ActorKind, CredentialPrincipal},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectorySnapshot, DirectoryStore, OrgRole, OrgSecurity,
    },
    directory_operations::DirectoryOperations,
};
use serde_json::json;

pub const ORG_SECURITY: &str = "/v1/orgs/{org}/security";
pub const OWNER_RESET: &str = "/v1/orgs/{org}/users/{user}/mfa/reset";
pub const INTERNAL_POLICY: &str = "/internal/mfa-policy";
pub const MY_SECURITY: &str = "/v1/me/security";

fn route_policy(method: &str, path: &str) -> Option<crate::routes::Mfa> {
    crate::routes::MANIFEST
        .iter()
        .find(|route| route.method == method && match_path(route.path, path).is_some())
        .map(|route| route.mfa)
}

pub fn sensitive(method: &str, path: &str) -> bool {
    if let Some(path) = crate::runner_proxy::proxied_path(path) {
        return crate::runner_proxy::route_for(method, &path).is_some_and(|route| route.sensitive);
    }
    matches!(
        route_policy(method, path),
        Some(crate::routes::Mfa::Sensitive(_))
    )
}

fn decision_step(method: &str, path: &str) -> Option<runbook_model::StepRunId> {
    let params = crate::routes::MANIFEST.iter().find_map(|route| {
        (route.method == method && matches!(route.mfa, crate::routes::Mfa::DestructiveDecision(_)))
            .then(|| match_path(route.path, path))
            .flatten()
    })?;
    crate::actors::parse_step_run_id(params.get("sr")?)
}

fn allowed(policy: OrgSecurity, method: MfaMethod) -> bool {
    match method {
        MfaMethod::Passkey => policy.allow_passkeys,
        MfaMethod::Totp | MfaMethod::BackupCode => policy.allow_authenticator,
    }
}

fn verified_at(session: &ResolvedSession, policy: OrgSecurity, now: i64) -> Option<i64> {
    session.mfa_verified_at.filter(|time| {
        session.mfa_enrolled == Some(true)
            && *time <= now
            && session
                .mfa_method
                .is_some_and(|method| allowed(policy, method))
    })
}

pub fn partial(session: &ResolvedSession, policy: OrgSecurity, role: OrgRole, now: i64) -> bool {
    policy.require_mfa.applies_to(role) && verified_at(session, policy, now).is_none()
}

pub(crate) fn fresh(session: &ResolvedSession, policy: OrgSecurity, now: i64) -> bool {
    verified_at(session, policy, now)
        .is_some_and(|time| now.saturating_sub(time) <= i64::from(policy.fresh_minutes) * 60_000)
}

pub(crate) fn fresh_at_issue(r: &Request, policy: OrgSecurity, now: i64) -> bool {
    matches!(&r.credential, Some(Credential::Session(session)) if session.scopes.is_none() && fresh(session, policy, now))
}

pub(crate) fn step_up(id: &str, policy: OrgSecurity) -> Response {
    let mut response = Response::error(403, "auth.mfa_step_up", id);
    response.body["fresh_minutes"] = json!(policy.fresh_minutes);
    response
}

pub(crate) fn security_view(
    session: &ResolvedSession,
    policy: OrgSecurity,
    role: OrgRole,
    now: i64,
) -> serde_json::Value {
    json!({
        "mfa_required": policy.require_mfa.applies_to(role),
        "mfa_partial": partial(session, policy, role, now),
        "mfa_enrolled": session.mfa_enrolled.unwrap_or(false),
        "mfa_verified_at": session.mfa_verified_at,
        "mfa_method": session.mfa_method,
        "allow_passkeys": policy.allow_passkeys,
        "allow_authenticator": policy.allow_authenticator,
        "fresh_minutes": policy.fresh_minutes,
    })
}

/// Only the auth service calls this over its authenticated host channel.
pub async fn internal_policy<B: Backends>(backends: &B, r: Request) -> Response {
    let id = &r.request_id;
    let Some(mut session) = crate::session::parse_session(&r.body) else {
        return Response::error(400, "request.invalid", id);
    };
    if !session.email_verified {
        return Response::error(403, "auth.forbidden", id);
    }
    if let Err((status, code)) = crate::session::link(backends, &mut session).await {
        return Response::error(status, code, id);
    }
    let Some(record) = session.principal.as_deref() else {
        return Response::error(403, "auth.forbidden", id);
    };
    let CredentialPrincipal::User { user_id, .. } = &record.principal else {
        return Response::error(403, "auth.forbidden", id);
    };
    let directory = backends.directory();
    let (Ok(snapshot), Ok(now)) = (
        directory.snapshot(&record.org_id).await,
        directory.current_time_ms().await,
    ) else {
        return Response::error(503, "storage.unavailable", id);
    };
    let Some(user) = snapshot.users.get(user_id).filter(|user| user.active) else {
        return Response::error(403, "auth.forbidden", id);
    };
    Response::json(
        200,
        security_view(&session, snapshot.security, user.role, now),
        id,
    )
}

// Standalone tokens present issuance proof when required by policy. Minting
// long-lived credentials for an enrolled user still requires a browser.
async fn bearer_gate<B: Backends>(backends: &B, r: &mut Request) -> Option<Response> {
    let mint = r.method == "POST"
        && (r.path == "/v1/tokens"
            || match_path("/v1/orgs/{org}/runner/bootstrap-tokens", &r.path).is_some());
    let sensitive = sensitive(&r.method, &r.path);
    let decision = decision_step(&r.method, &r.path);
    if !mint && !sensitive && decision.is_none() {
        return None;
    }
    let secret = r.bearer()?;
    let Ok((kind, _)) = runbook_core::credentials::parse_secret(secret) else {
        return None;
    };
    if !matches!(
        kind,
        runbook_core::credentials::TokenKind::Personal
            | runbook_core::credentials::TokenKind::Service
    ) {
        return None;
    }
    let caller = match r.credential_record(&backends.credentials()).await {
        Ok(caller) => caller,
        Err(error) => {
            let (status, code) = error.status();
            return Some(Response::error(status, code, &r.request_id));
        }
    };
    if let Some(Credential::Bearer(secret)) = &r.credential {
        r.credential = Some(Credential::ResolvedBearer(
            secret.clone(),
            Box::new(caller.clone()),
        ));
    }
    let directory = backends.directory();
    let unavailable = || Response::error(503, "storage.unavailable", &r.request_id);
    let snapshot = match directory.snapshot(&caller.org_id).await {
        Ok(snapshot) => snapshot,
        Err(_) => return Some(unavailable()),
    };
    let now = match directory.current_time_ms().await {
        Ok(now) => now,
        Err(_) => return Some(unavailable()),
    };
    if caller.validate().is_err()
        || caller.revoked_at.is_some()
        || caller.expires_at.is_some_and(|time| time <= now)
    {
        // The route keeps its usual invalid-credential response.
        return None;
    }
    let required = match &caller.principal {
        CredentialPrincipal::User { user_id, .. } => {
            let role = snapshot.users.get(user_id).filter(|user| user.active)?.role;
            snapshot.security.require_mfa.applies_to(role)
        }
        CredentialPrincipal::Service { .. } => {
            snapshot.security.require_mfa != runbook_core::directory::MfaRequirement::Off
        }
    };
    if required {
        let destructive = if let Some(step) = decision {
            let actor = crate::actor_store::StoreActorBackend {
                backends: backends.clone(),
            };
            match actor.destructive_approval(&caller.org_id, &step).await {
                Ok(found) => found.is_some(),
                Err(error) => {
                    return Some(Response::error(error.status, error.code, &r.request_id));
                }
            }
        } else {
            false
        };
        if mint || sensitive || destructive {
            match backends.token_metadata(&caller.org_id, &caller.id).await {
                Ok(metadata) if metadata.fresh_mfa_at_issue => (),
                Ok(_) => {
                    return Some(Response::error(
                        403,
                        "auth.mfa_token_replacement_required",
                        &r.request_id,
                    ));
                }
                Err(_) => return Some(unavailable()),
            }
        }
    }
    let CredentialPrincipal::User { user_id, .. } = &caller.principal else {
        return None;
    };
    if !mint {
        return None;
    }
    if required {
        return Some(step_up(&r.request_id, snapshot.security));
    }
    let identities = match directory.auth_users_of(user_id).await {
        Ok(identities) => identities,
        Err(_) => return Some(unavailable()),
    };
    if identities.is_empty() {
        return None;
    }
    match backends
        .auth_internal("mfa/enrollment", json!({ "auth_user_ids": identities }))
        .await
    {
        Ok(Some(body)) => match body["enrolled"].as_bool() {
            Some(false) => None,
            Some(true) => Some(step_up(&r.request_id, snapshot.security)),
            None => Some(Response::error(503, "auth.unavailable", &r.request_id)),
        },
        // Legacy hosts with sign-in disabled can still use their headless tokens.
        Ok(None) if backends.auth_methods().is_empty() => None,
        _ => Some(Response::error(503, "auth.unavailable", &r.request_id)),
    }
}

pub(crate) async fn gate<B: Backends>(backends: &B, r: &mut Request) -> Option<Response> {
    let session = match &r.credential {
        Some(Credential::Session(session)) => session,
        Some(Credential::Bearer(_) | Credential::ResolvedBearer(_, _)) => {
            return bearer_gate(backends, r).await;
        }
        None => return None,
    };
    let principal = session.principal.as_deref()?;
    let CredentialPrincipal::User { user_id, .. } = &principal.principal else {
        return None;
    };
    let directory = backends.directory();
    let snapshot = match directory.snapshot(&principal.org_id).await {
        Ok(snapshot) => snapshot,
        Err(_) => return Some(Response::error(503, "storage.unavailable", &r.request_id)),
    };
    let role = snapshot.users.get(user_id)?.role;
    let policy = snapshot.security;
    let now = match directory.current_time_ms().await {
        Ok(now) => now,
        Err(_) => return Some(Response::error(503, "storage.unavailable", &r.request_id)),
    };
    let sensitive = sensitive(&r.method, &r.path);
    if sensitive && session.scopes.is_some() {
        return Some(step_up(&r.request_id, policy));
    }
    if session.scopes.is_none() && r.method == "GET" && r.path == MY_SECURITY {
        let mut view = security_view(session, policy, role, now);
        // The session's own sign-in identity: the account its security
        // page manages. Null when the sign-in service cannot say.
        let own = factors(backends, std::slice::from_ref(&session.auth_user_id))
            .await
            .ok()
            .map(|found| {
                found
                    .get(&session.auth_user_id)
                    .cloned()
                    .unwrap_or_default()
            });
        view["authenticator_enrolled"] = json!(own.as_ref().map(|f| f.authenticator));
        view["passkey_count"] = json!(own.as_ref().map(|f| f.passkeys));
        return Some(Response::json(200, view, &r.request_id));
    }
    if partial(session, policy, role, now) {
        let view = security_view(session, policy, role, now);
        if session.scopes.is_none() && r.method == "GET" && r.path == "/v1/me" {
            return Some(Response::json(
                200,
                json!({
                    "user": { "id": user_id, "email": session.email },
                    "org": { "id": principal.org_id },
                    "security": view,
                }),
                &r.request_id,
            ));
        }
        let mut response = Response::error(403, "auth.mfa_required", &r.request_id);
        response.body["security"] = view;
        return Some(response);
    }
    let enrollment_unknown = session.mfa_enrolled.is_none();
    let unavailable = || Response::error(503, "auth.unavailable", &r.request_id);
    if sensitive && enrollment_unknown {
        return Some(unavailable());
    }
    let needs_check = session.scopes.is_some()
        || enrollment_unknown
        || ((session.mfa_enrolled == Some(true) || policy.require_mfa.applies_to(role))
            && !fresh(session, policy, now));
    if needs_check && sensitive {
        return Some(step_up(&r.request_id, policy));
    }
    if needs_check
        && r.method == "POST"
        && let Some(step) = decision_step(&r.method, &r.path)
    {
        let actor = crate::actor_store::StoreActorBackend {
            backends: backends.clone(),
        };
        match actor.destructive_approval(&principal.org_id, &step).await {
            Ok(Some(_)) if enrollment_unknown => return Some(unavailable()),
            Ok(Some(_)) => return Some(step_up(&r.request_id, policy)),
            Ok(None) => (),
            Err(error) => return Some(Response::error(error.status, error.code, &r.request_id)),
        }
    }
    None
}

/// One auth identity's enrolled factors (`/internal/mfa/factors`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Factors {
    pub passkeys: u64,
    pub authenticator: bool,
}

/// Enrolled factors of each known auth identity, keyed by auth user ID.
/// Unknown identities are absent. `Err` when the sign-in service cannot
/// answer; a host without one knows no factors.
pub(crate) async fn factors<B: Backends>(
    backends: &B,
    ids: &[String],
) -> Result<std::collections::BTreeMap<String, Factors>, ()> {
    let mut found = std::collections::BTreeMap::new();
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    let unique: Vec<&String> = unique.into_iter().collect();
    for chunk in unique.chunks(256) {
        let answer = backends
            .auth_internal("mfa/factors", json!({ "auth_user_ids": chunk }))
            .await
            .map_err(|_| ())?;
        let Some(body) = answer else {
            return if backends.auth_methods().is_empty() {
                Ok(found)
            } else {
                Err(())
            };
        };
        for item in body["factors"].as_array().ok_or(())? {
            let id = item["auth_user_id"].as_str().ok_or(())?;
            found.insert(
                id.to_owned(),
                Factors {
                    passkeys: item["passkeys"].as_u64().ok_or(())?,
                    authenticator: item["authenticator"].as_bool().ok_or(())?,
                },
            );
        }
    }
    Ok(found)
}

/// Why a member could not finish signing in under a proposed policy.
fn lockout(policy: OrgSecurity, passkeys: u64, authenticator: bool) -> Option<&'static str> {
    let usable =
        (policy.allow_passkeys && passkeys > 0) || (policy.allow_authenticator && authenticator);
    if usable {
        None
    } else if passkeys == 0 && !authenticator {
        // Required enrollment starts with an allowed passkey (design §8).
        (!policy.allow_passkeys).then_some("cannot_enroll")
    } else {
        Some("factor_not_allowed")
    }
}

/// Members who would be unable to sign in or enroll an allowed factor.
async fn preview<B: Backends>(
    backends: &B,
    r: &Request,
    snapshot: &DirectorySnapshot,
    policy: OrgSecurity,
) -> Result<serde_json::Value, Box<Response>> {
    let error = |status, code| Box::new(Response::error(status, code, &r.request_id));
    let mut members: std::collections::BTreeMap<_, _> = snapshot
        .users
        .values()
        .filter(|user| user.active && policy.require_mfa.applies_to(user.role))
        .map(|user| (&user.id, (user, Vec::new())))
        .collect();
    if !members.is_empty() {
        for link in backends
            .directory()
            .auth_links_in_org(&snapshot.org_id)
            .await
            .map_err(|_| error(503, "storage.unavailable"))?
        {
            if let Some((_, identities)) = members.get_mut(&link.user_id) {
                identities.push(link);
            }
        }
    }
    let all: Vec<String> = members
        .values()
        .flat_map(|(_, identities)| identities.iter().map(|link| link.auth_user_id.clone()))
        .collect();
    let known = factors(backends, &all)
        .await
        .map_err(|_| error(503, "auth.unavailable"))?;
    let mut affected = Vec::new();
    for (user, identities) in members.into_values() {
        let mine: Vec<&Factors> = identities
            .iter()
            .filter_map(|link| known.get(&link.auth_user_id))
            .collect();
        let passkeys = mine.iter().map(|f| f.passkeys).sum();
        let authenticator = mine.iter().any(|f| f.authenticator);
        let Some(reason) = lockout(policy, passkeys, authenticator) else {
            continue;
        };
        let email = identities.first().map(|link| &link.email);
        affected.push(json!({
            "user_id": user.id, "name": user.name, "email": email, "reason": reason,
        }));
    }
    Ok(json!({
        "dry_run": true,
        "policy": policy,
        "affected": affected,
    }))
}

pub(crate) async fn owner_reset<B: Backends>(backends: &B, r: &Request) -> Response {
    match reset(backends, r).await {
        Ok(()) => Response::json(200, json!({"reset": true}), &r.request_id),
        Err(response) => *response,
    }
}

async fn reset<B: Backends>(backends: &B, r: &Request) -> Result<(), Box<Response>> {
    let error = |status, code| Box::new(Response::error(status, code, &r.request_id));
    let unavailable = || error(503, "storage.unavailable");
    let Some(Credential::Session(session)) = &r.credential else {
        let (status, code) = if r.credential.is_none() {
            (401, "auth.required")
        } else {
            (403, "auth.forbidden")
        };
        return Err(error(status, code));
    };
    let record = session
        .principal
        .as_deref()
        .ok_or_else(|| error(403, "auth.forbidden"))?;
    let actor = match &record.principal {
        CredentialPrincipal::User {
            user_id,
            actor: ActorKind::Human,
        } => user_id,
        _ => return Err(error(403, "auth.forbidden")),
    };
    let params =
        match_path(OWNER_RESET, &r.path).ok_or_else(|| error(404, "resource.not_found"))?;
    if params["org"] != record.org_id.as_str() {
        return Err(error(404, "resource.not_found"));
    }
    let target = params["user"]
        .parse()
        .map_err(|_| error(404, "resource.not_found"))?;
    let directory = backends.directory();
    let snapshot = directory
        .snapshot(&record.org_id)
        .await
        .map_err(|_| unavailable())?;
    if !snapshot
        .users
        .get(actor)
        .is_some_and(|user| user.active && user.role == OrgRole::Owner)
        || actor == &target
    {
        return Err(error(403, "auth.forbidden"));
    }
    if !snapshot.users.get(&target).is_some_and(|user| user.active) {
        return Err(error(404, "resource.not_found"));
    }
    let now = directory
        .current_time_ms()
        .await
        .map_err(|_| unavailable())?;
    if !fresh(session, snapshot.security, now) {
        return Err(Box::new(step_up(&r.request_id, snapshot.security)));
    }
    let auth_users = directory
        .owner_reset_auth_users(&record.org_id, &target)
        .await
        .map_err(|_| unavailable())?;
    if auth_users.is_empty() {
        return Err(error(409, "auth.mfa_operator_reset_required"));
    }
    let answer = backends
        .auth_internal(
            "mfa/reset",
            json!({
                "auth_user_ids": auth_users, "actor": actor,
            }),
        )
        .await
        .map_err(|_| error(503, "auth.unavailable"))?;
    match answer
        .as_ref()
        .and_then(|body| body.get("reset"))
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => Ok(()),
        Some(false) => Err(error(404, "resource.not_found")),
        None => Err(error(503, "auth.unavailable")),
    }
}

pub(crate) async fn org_security<B: Backends>(backends: &B, r: &Request) -> Response {
    match update(backends, r).await {
        Ok(body) => Response::json(200, body, &r.request_id),
        Err(response) => *response,
    }
}

fn dry_run(r: &Request) -> bool {
    r.query
        .get("dry_run")
        .is_some_and(|value| value == "true" || value == "1")
}

async fn update<B: Backends>(
    backends: &B,
    r: &Request,
) -> Result<serde_json::Value, Box<Response>> {
    let error = |status, code| Box::new(Response::error(status, code, &r.request_id));
    let unavailable = || error(503, "storage.unavailable");
    let directory = backends.directory();
    let record = r
        .credential_record(&backends.credentials())
        .await
        .map_err(|e| {
            let (status, code) = e.status();
            error(status, code)
        })?;
    if let Some(params) = match_path(ORG_SECURITY, &r.path)
        && params["org"] != record.org_id.as_str()
    {
        return Err(error(404, "resource.not_found"));
    }
    let CredentialPrincipal::User {
        user_id,
        actor: ActorKind::Human,
    } = &record.principal
    else {
        return Err(error(403, "auth.forbidden"));
    };
    let now = directory
        .current_time_ms()
        .await
        .map_err(|_| unavailable())?;
    if record.revoked_at.is_some() || record.expires_at.is_some_and(|time| time <= now) {
        return Err(error(401, "auth.invalid_credential"));
    }
    let snapshot: DirectorySnapshot = directory
        .snapshot(&record.org_id)
        .await
        .map_err(|_| unavailable())?;
    if !snapshot
        .users
        .get(user_id)
        .is_some_and(|user| user.active && user.role == OrgRole::Owner)
    {
        return Err(error(403, "auth.forbidden"));
    }
    if r.method == "GET" {
        return Ok(json!(snapshot.security));
    }
    let policy: OrgSecurity =
        serde_json::from_value(r.body.clone()).map_err(|_| error(400, "request.invalid"))?;
    policy
        .validate()
        .map_err(|_| error(400, "request.invalid"))?;
    let Some(Credential::Session(session)) = &r.credential else {
        return Err(Box::new(step_up(&r.request_id, snapshot.security)));
    };
    if session.scopes.is_some()
        || !fresh(session, snapshot.security, now)
        || (policy.require_mfa.applies_to(OrgRole::Owner) && !fresh(session, policy, now))
    {
        return Err(Box::new(step_up(&r.request_id, snapshot.security)));
    }
    if dry_run(r) {
        return preview(backends, r, &snapshot, policy).await;
    }
    match directory
        .commit(DirectoryCommit {
            org_id: record.org_id.clone(),
            expected_revision: snapshot.revision,
            mutations: vec![DirectoryMutation::SetOrgSecurity {
                policy,
                actor: user_id.clone(),
                occurred_at_ms: now,
            }],
        })
        .await
        .map_err(|_| unavailable())?
    {
        CommitResult::Committed(_) => Ok(json!(policy)),
        CommitResult::Conflict => Err(error(409, "directory.concurrent_change")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runner_decisions_do_not_enter_destructive_approval_step_up() {
        for action in ["decide", "abstain"] {
            let path = format!("/v1/runner/decisions/clm_{:026}/{action}", 1);
            assert!(matches!(
                super::route_policy("POST", &path),
                Some(crate::routes::Mfa::Standard(_))
            ));
            assert!(!super::sensitive("POST", &path));
            assert_eq!(super::decision_step("POST", &path), None);
        }
    }

    #[test]
    fn decision_aliases_use_decoded_path_parameters() {
        let id = format!("sr_{:026}", 1);
        let expected = id.parse().ok();
        for alias in ["decision", "decisions"] {
            let path = format!("/v1/step-runs/{id}/{alias}");
            assert_eq!(super::decision_step("POST", &path), expected);
            let encoded = format!("/v1/step-runs/{}/{alias}", id.replace('_', "%5F"));
            assert_eq!(super::decision_step("POST", &encoded), expected);
        }
    }
}
