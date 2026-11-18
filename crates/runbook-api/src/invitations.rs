//! AUTH2 (D45) `/v1/orgs/{org}/invitations`: an org owner or admin invites
//! an email address with an org role and project roles, lists and revokes
//! invitations. Signing in with that verified email consumes the invitation
//! (session linking). Admins never invite owners (the D17 rank rule).
use crate::{Request, backends::Backends, problem::Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{
    auth_links::{AuthLinkStore, INVITATION_TTL_MS, InvitationRecord, InvitationState},
    credentials::{AuthError, Principal, Scope, authorize_organization},
    directory::{DirectoryError, DirectorySnapshot, DirectoryStore, OrgRole, email_key},
    directory_operations::DirectoryOperations,
    identity::OrgAction,
};
use runbook_model::{
    CreateInvitationRequest, DirectoryPage, INVITATION_ROUTES, InvitationId,
    InvitationProjectGrant, InvitationView, OrganizationId, OrganizationRole, ProjectId,
};
use serde_json::{Value, json};

type Failure = (u16, &'static str);
const UNAVAILABLE: Failure = (503, "storage.unavailable");
const NOT_FOUND: Failure = (404, "resource.not_found");

pub fn recognizes(path: &str) -> bool {
    INVITATION_ROUTES
        .iter()
        .any(|route| crate::directory::match_path(route.path, path).is_some())
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok((status, body)) => Response::json(status, body, &r.request_id),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

fn rank(role: OrgRole) -> u8 {
    match role {
        OrgRole::Member => 0,
        OrgRole::Admin => 1,
        OrgRole::Owner => 2,
    }
}
fn org_role(role: &OrganizationRole) -> OrgRole {
    match role {
        OrganizationRole::Owner => OrgRole::Owner,
        OrganizationRole::Admin => OrgRole::Admin,
        OrganizationRole::Member => OrgRole::Member,
    }
}
fn wire_role(role: OrgRole) -> OrganizationRole {
    match role {
        OrgRole::Owner => OrganizationRole::Owner,
        OrgRole::Admin => OrganizationRole::Admin,
        OrgRole::Member => OrganizationRole::Member,
    }
}
fn view(record: &InvitationRecord, now: i64) -> InvitationView {
    InvitationView {
        id: record.id.clone(),
        email: record.email.clone(),
        role: wire_role(record.role),
        projects: record
            .projects
            .iter()
            .map(|(project, role)| InvitationProjectGrant {
                project: project.to_string(),
                role: *role,
            })
            .collect(),
        state: match record.state {
            InvitationState::Pending if record.expires_at_ms <= now => {
                runbook_model::InvitationState::Expired
            }
            InvitationState::Pending => runbook_model::InvitationState::Pending,
            InvitationState::Accepted => runbook_model::InvitationState::Accepted,
            InvitationState::Revoked => runbook_model::InvitationState::Revoked,
        },
        invited_by: record.invited_by.clone(),
        created_at_ms: record.created_at_ms,
        expires_at_ms: record.expires_at_ms,
        name: record.name.clone(),
    }
}
fn directory(e: DirectoryError) -> Failure {
    match e {
        DirectoryError::NotFound => NOT_FOUND,
        DirectoryError::Invalid(_) => (422, "request.invalid"),
        DirectoryError::Duplicate => (409, "invitation.exists"),
        _ => UNAVAILABLE,
    }
}
/// A project ID or slug in the organization.
fn project(s: &DirectorySnapshot, reference: &str) -> Result<ProjectId, Failure> {
    if let Ok(id) = reference.parse::<ProjectId>()
        && s.projects.contains_key(&id)
    {
        return Ok(id);
    }
    s.projects
        .values()
        .find(|p| p.slug == reference)
        .map(|p| p.id.clone())
        .ok_or(NOT_FOUND)
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<(u16, Value), Failure> {
    let mut recognized = false;
    let mut matched = None;
    for route in INVITATION_ROUTES {
        if let Some(p) = crate::directory::match_path(route.path, &r.path) {
            recognized = true;
            if route.method == r.method {
                matched = Some((route, p));
            }
        }
    }
    let (_, params) = matched.ok_or(if recognized {
        (405, "request.method_not_allowed")
    } else {
        NOT_FOUND
    })?;
    let credentials = backends.credentials();
    let directory_store = backends.directory();
    let record = r
        .credential_record(&credentials)
        .await
        .map_err(|e| e.status())?;
    let org: OrganizationId = params["org"].parse().map_err(|_| NOT_FOUND)?;
    if org != record.org_id {
        return Err(NOT_FOUND);
    }
    let now = directory_store
        .current_time_ms()
        .await
        .map_err(|_| UNAVAILABLE)?;
    let snapshot = directory_store.snapshot(&org).await.map_err(directory)?;
    let principal = authorize_organization(&record, &snapshot, now, OrgAction::ManageMemberships)
        .map_err(|e| match e {
        AuthError::Forbidden if !record.scopes.contains(&Scope::ProjectAdmin) => {
            (403, "auth.scope_missing")
        }
        AuthError::Forbidden => (403, "auth.forbidden"),
        _ => (401, "auth.invalid_credential"),
    })?;
    let Principal::Human { user_id: caller } = principal else {
        return Err((403, "auth.forbidden"));
    };
    let caller_role = snapshot
        .users
        .get(&caller)
        .map(|u| u.role)
        .ok_or((403, "auth.forbidden"))?;
    match (r.method.as_str(), params.get("inv")) {
        ("GET", None) => {
            let limit = match r.query.get("limit") {
                Some(v) => v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=100).contains(n))
                    .ok_or((422, "request.invalid"))?,
                None => 100,
            };
            let after = r
                .query
                .get("cursor")
                .map(|c| {
                    URL_SAFE_NO_PAD
                        .decode(c)
                        .ok()
                        .and_then(|b| String::from_utf8(b).ok())
                        .filter(|id| id.parse::<InvitationId>().is_ok())
                        .ok_or((400, "cursor.invalid"))
                })
                .transpose()?;
            let all = directory_store
                .list_invitations(&org)
                .await
                .map_err(directory)?;
            let start = match &after {
                Some(id) => all
                    .iter()
                    .position(|i| i.id.as_str() == id)
                    .map(|p| p + 1)
                    .ok_or((400, "cursor.invalid"))?,
                None => 0,
            };
            let page: Vec<_> = all.iter().skip(start).take(limit).collect();
            let next_cursor = (start + page.len() < all.len())
                .then(|| page.last().map(|i| URL_SAFE_NO_PAD.encode(i.id.as_str())))
                .flatten();
            Ok((
                200,
                json!(DirectoryPage {
                    items: page.into_iter().map(|i| view(i, now)).collect(),
                    next_cursor,
                }),
            ))
        }
        ("POST", None) => {
            r.idempotency_key
                .as_deref()
                .filter(|k| !k.trim().is_empty() && k.len() <= 256)
                .ok_or((428, "request.idempotency_key_required"))?;
            let request: CreateInvitationRequest =
                serde_json::from_value(r.body.clone()).map_err(|_| (422, "request.invalid"))?;
            let name = match request.name.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(name) if name.chars().count() <= 100 => Some(name.to_owned()),
                Some(_) => return Err((422, "request.invalid")),
            };
            crate::field_caps::text("/email", &request.email, crate::field_caps::NAME_BYTES)?;
            let email = email_key(&request.email).map_err(|_| (422, "request.invalid"))?;
            let role = request.role.as_ref().map_or(OrgRole::Member, org_role);
            // D17: never invite above your own rank (admins never owners).
            if rank(role) > rank(caller_role) {
                return Err((403, "auth.forbidden"));
            }
            let mut projects = Vec::new();
            for grant in request.projects.iter().flatten() {
                let id = project(&snapshot, &grant.project)?;
                if projects.iter().any(|(p, _)| p == &id) {
                    return Err((422, "request.invalid"));
                }
                projects.push((id, grant.role));
            }
            if let Some(user) = directory_store
                .user_by_verified_email(&email)
                .await
                .map_err(directory)?
                && snapshot.users.contains_key(&user.id)
            {
                return Err((409, "invitation.already_member"));
            }
            let record = InvitationRecord {
                id: format!("inv_{}", r.new_id)
                    .parse()
                    .map_err(|_| UNAVAILABLE)?,
                org_id: org.clone(),
                email: email.clone(),
                role,
                projects,
                invited_by: caller.clone(),
                created_at_ms: now,
                expires_at_ms: now.saturating_add(INVITATION_TTL_MS),
                state: InvitationState::Pending,
                accepted_by: None,
                closed_at_ms: None,
                name,
            };
            match directory_store
                .create_invitation(&org, record.clone())
                .await
            {
                Ok(()) => {
                    // NOTIFY1: the invitation mail rides the same outbox.
                    // Best-effort: the invitation exists either way and an
                    // admin can share the sign-in link by hand.
                    let _ = record_invitation_mail(backends, &snapshot, &record, now).await;
                    Ok((201, json!(view(&record, now))))
                }
                // A retry of the same invitation answers it again; anything
                // else naming a pending email is a conflict.
                Err(DirectoryError::Duplicate) => {
                    let existing = directory_store
                        .invitations_for_email(&email, now)
                        .await
                        .map_err(directory)?
                        .into_iter()
                        .find(|i| i.org_id == org)
                        .ok_or((409, "invitation.exists"))?;
                    if existing.role == record.role
                        && existing.projects == record.projects
                        && existing.invited_by == caller
                    {
                        Ok((201, json!(view(&existing, now))))
                    } else {
                        Err((409, "invitation.exists"))
                    }
                }
                Err(e) => Err(directory(e)),
            }
        }
        ("DELETE", Some(inv)) => {
            let id: InvitationId = inv.parse().map_err(|_| NOT_FOUND)?;
            if directory_store
                .revoke_invitation(&org, &id, now)
                .await
                .map_err(directory)?
            {
                Ok((204, Value::Null))
            } else {
                Err(NOT_FOUND)
            }
        }
        _ => Err((405, "request.method_not_allowed")),
    }
}

/// NOTIFY1: queue the invitation email for a just-created invitation.
/// The recipient has no account yet, so the row is address-only.
async fn record_invitation_mail<B: Backends>(
    backends: &B,
    snapshot: &DirectorySnapshot,
    record: &InvitationRecord,
    now: i64,
) -> Result<(), runbook_core::StoreError> {
    use runbook_core::notify::{EmailRecord, EmailState, NotifyStore, render_invitation};
    let invited_by = snapshot
        .users
        .get(&record.invited_by)
        .map(|user| user.name.clone())
        .unwrap_or_else(|| "An administrator".into());
    let origin = backends.origin();
    let message = render_invitation(
        &record.email,
        &snapshot.organization.title,
        &invited_by,
        &format!("{}/sign-in", origin.trim_end_matches('/')),
    );
    backends
        .notify()
        .record_emails(vec![EmailRecord {
            id: runbook_core::webhooks::derived_id(
                "eml_",
                record.created_at_ms,
                &[&format!("invitation:{}", record.id), &record.email],
            ),
            org_id: record.org_id.clone(),
            user_id: None,
            email: record.email.clone(),
            kind: "invitation".into(),
            execution_id: None,
            event_id: format!("invitation:{}", record.id),
            subject: message.subject,
            text: message.text,
            html: message.html,
            state: EmailState::Pending,
            attempts: 0,
            next_attempt_at_ms: now,
            created_at_ms: now,
            sent_at_ms: None,
            failure: None,
        }])
        .await?;
    Ok(())
}
