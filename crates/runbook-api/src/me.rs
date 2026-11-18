//! B1 `GET /v1/me` and `DELETE /v1/me/token`: who the presenting
//! credential is, and self-revocation for personal tokens. Identity comes
//! only from the stored credential record and the directory snapshot.
use crate::{Request, backends::Backends, problem::Response};
use runbook_core::{
    auth_links::AuthLinkStore,
    credentials::{ActorKind, CredentialPrincipal, CredentialRecord, CredentialStore, TokenKind},
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    notify::{EmailPreference, NotifyStore, unsubscribe_token_matches},
};
use runbook_model::{
    EmailPreferenceView, EmailPreferencesRequest, EmailPreferencesView, MeCredential, MeOrg,
    MeTeam, MeUser, MeView, UnsubscribeRequest, UnsubscribeView,
};
use serde_json::json;

/// MCP2: the person's connected OAuth apps.
pub const APPS: &str = "/v1/me/apps";

pub fn recognizes(path: &str) -> bool {
    path == "/v1/me"
        || path == "/v1/me/token"
        || path == "/v1/me/email-preferences"
        || path == "/v1/unsubscribe"
        || path == APPS
        || app_id(path).is_some()
}

fn preference_view(preference: EmailPreference) -> EmailPreferenceView {
    match preference {
        EmailPreference::All => EmailPreferenceView::All,
        EmailPreference::ApprovalsOnly => EmailPreferenceView::ApprovalsOnly,
        EmailPreference::DailySummary => EmailPreferenceView::DailySummary,
        EmailPreference::None => EmailPreferenceView::None,
    }
}
fn preference_of(view: EmailPreferenceView) -> EmailPreference {
    match view {
        EmailPreferenceView::All => EmailPreference::All,
        EmailPreferenceView::ApprovalsOnly => EmailPreference::ApprovalsOnly,
        EmailPreferenceView::DailySummary => EmailPreference::DailySummary,
        EmailPreferenceView::None => EmailPreference::None,
    }
}

/// The person behind a personal credential (any actor kind): their own
/// notification preferences are theirs to set.
fn principal_user(record: &CredentialRecord) -> Result<runbook_model::UserId, Failure> {
    match &record.principal {
        CredentialPrincipal::User { user_id, .. } if record.kind == TokenKind::Personal => {
            Ok(user_id.clone())
        }
        _ => Err((403, "auth.forbidden")),
    }
}

/// `{client}` of `/v1/me/apps/{client}`: one segment of the auth service's
/// client ID alphabet.
fn app_id(path: &str) -> Option<&str> {
    path.strip_prefix("/v1/me/apps/").filter(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    })
}

/// MCP2: who may manage connected apps: a person (a human personal token or
/// browser session) with `tokens:manage`, as for their own tokens (D17).
/// Never a service token, an agent token or an OAuth grant (an agent).
fn person(record: &CredentialRecord) -> Result<runbook_model::UserId, Failure> {
    match &record.principal {
        CredentialPrincipal::User {
            user_id,
            actor: ActorKind::Human,
        } if record.kind == TokenKind::Personal
            && record
                .scopes
                .contains(&runbook_core::credentials::Scope::TokensManage) =>
        {
            Ok(user_id.clone())
        }
        _ => Err((403, "auth.forbidden")),
    }
}

fn app_view(grant: &serde_json::Value) -> Option<runbook_model::ConnectedAppView> {
    let strings = |key: &str| -> Vec<String> {
        grant[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(runbook_model::ConnectedAppView {
        client_id: grant["client_id"].as_str()?.to_owned(),
        client_name: grant["client_name"].as_str().map(str::to_owned),
        redirect_hosts: strings("redirect_hosts"),
        scopes: strings("scopes"),
        created_at_ms: grant["created_at"].as_i64()?,
        last_used_at_ms: grant["last_used_at"].as_i64(),
        expires_at_ms: grant["expires_at"].as_i64()?,
    })
}

type Failure = (u16, &'static str);

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok((status, body)) => Response::json(status, body, &r.request_id),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

/// Any live personal or service credential may read its own identity;
/// scopes are reported, not required, here.
async fn authenticate<B: Backends>(
    credentials: &B::Credentials,
    directory: &B::Directory,
    r: &Request,
) -> Result<(CredentialRecord, i64), Failure> {
    let unavailable = (503, "storage.unavailable");
    let record = r
        .credential_record(credentials)
        .await
        .map_err(|e| e.status())?;
    let now = directory.current_time_ms().await.map_err(|_| unavailable)?;
    if !matches!(record.kind, TokenKind::Personal | TokenKind::Service)
        || record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err((401, "auth.invalid_credential"));
    }
    Ok((record, now))
}

fn scope_name(scope: &runbook_core::credentials::Scope) -> String {
    serde_json::to_value(scope)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn dispatch<B: Backends>(
    backends: &B,
    r: &Request,
) -> Result<(u16, serde_json::Value), Failure> {
    let credentials = backends.credentials();
    let directory = backends.directory();
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/me") => {
            let (record, now) = authenticate::<B>(&credentials, &directory, r).await?;
            let unavailable = (503, "storage.unavailable");
            let snapshot = directory
                .snapshot(&record.org_id)
                .await
                .map_err(|_| unavailable)?;
            let (user, role, teams) = match &record.principal {
                CredentialPrincipal::User { user_id, .. } => {
                    let record_user = snapshot
                        .users
                        .get(user_id)
                        .filter(|u| u.active)
                        .ok_or((403, "auth.forbidden"))?;
                    let teams = snapshot
                        .team_members
                        .iter()
                        .filter(|(_, member)| member == user_id)
                        .filter_map(|(team_id, _)| {
                            snapshot.teams.get(team_id).map(|team| MeTeam {
                                id: team_id.clone(),
                                slug: team.slug.clone(),
                            })
                        })
                        .collect();
                    let role = serde_json::to_value(record_user.role)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned));
                    (
                        Some(MeUser {
                            id: user_id.clone(),
                            name: record_user.name.clone(),
                        }),
                        role,
                        teams,
                    )
                }
                CredentialPrincipal::Service { .. } => (None, None, Vec::new()),
            };
            let actor_kind = match &record.principal {
                CredentialPrincipal::User {
                    actor: runbook_core::credentials::ActorKind::Human,
                    ..
                } => Some("human".to_owned()),
                CredentialPrincipal::User {
                    actor: runbook_core::credentials::ActorKind::Agent,
                    ..
                } => Some("agent".to_owned()),
                CredentialPrincipal::Service { .. } => None,
            };
            credentials
                .touch_last_used(&record.org_id, &record.id, now)
                .await
                .map_err(|_| unavailable)?;
            let view = MeView {
                user,
                org: MeOrg {
                    id: record.org_id.clone(),
                    title: snapshot.organization.title.clone(),
                    role,
                },
                teams,
                credential: MeCredential {
                    // D46: a browser session reports itself, not a token kind.
                    kind: match record.kind {
                        _ if r.is_session() => "session".into(),
                        TokenKind::Personal => "personal".into(),
                        TokenKind::Service => "service".into(),
                        TokenKind::Execution => "execution".into(),
                        TokenKind::Refresh => "refresh".into(),
                    },
                    actor_kind,
                    scopes: {
                        let mut scopes: Vec<String> =
                            record.scopes.iter().map(scope_name).collect();
                        scopes.sort();
                        scopes
                    },
                    projects: record
                        .projects
                        .as_ref()
                        .map(|projects| projects.iter().cloned().collect()),
                    expires_at_ms: record.expires_at,
                },
            };
            Ok((200, json!(view)))
        }
        ("PATCH", "/v1/me") => {
            let (record, _) = authenticate::<B>(&credentials, &directory, r).await?;
            // The person themself, signed in or with their own token; an
            // agent acting for them does not rename them.
            let user_id = match &record.principal {
                CredentialPrincipal::User {
                    user_id,
                    actor: ActorKind::Human,
                } if record.kind == TokenKind::Personal => user_id.clone(),
                _ => return Err((403, "auth.forbidden")),
            };
            let body: runbook_model::UpdateMeRequest =
                serde_json::from_value(r.body.clone()).map_err(|_| (422u16, "request.invalid"))?;
            let name = body.name.trim().to_owned();
            if name.is_empty() || name.chars().count() > 100 {
                return Err((422, "request.invalid"));
            }
            let unavailable = (503, "storage.unavailable");
            for _ in 0..3 {
                let snapshot = directory
                    .snapshot(&record.org_id)
                    .await
                    .map_err(|_| unavailable)?;
                if !snapshot.users.get(&user_id).is_some_and(|u| u.active) {
                    return Err((403, "auth.forbidden"));
                }
                let committed = directory
                    .commit(runbook_core::directory::DirectoryCommit {
                        org_id: record.org_id.clone(),
                        expected_revision: snapshot.revision,
                        mutations: vec![runbook_core::directory::DirectoryMutation::SetUserName {
                            user_id: user_id.clone(),
                            name: name.clone(),
                        }],
                    })
                    .await
                    .map_err(|_| unavailable)?;
                if matches!(committed, runbook_core::CommitResult::Committed(_)) {
                    return Ok((200, json!(MeUser { id: user_id, name })));
                }
            }
            Err((409, "directory.conflict"))
        }
        ("DELETE", "/v1/me/token") => {
            let (record, now) = authenticate::<B>(&credentials, &directory, r).await?;
            // Sign-out everywhere for this PAT only; service tokens are
            // revoked through token administration (B1.5), and a browser
            // session signs out through /api/auth/sign-out.
            if record.kind != TokenKind::Personal || r.is_session() {
                return Err((403, "auth.forbidden"));
            }
            credentials
                .revoke(&record.org_id, &record.id, now)
                .await
                .map_err(|_| (503, "storage.unavailable"))?;
            Ok((204, serde_json::Value::Null))
        }
        ("POST", "/v1/unsubscribe") => {
            let notify = backends.notify();
            let body: UnsubscribeRequest = serde_json::from_value(r.body.clone())
                .map_err(|_| (400u16, "notify.invalid_token"))?;
            let (user, _) = body
                .token
                .rsplit_once('.')
                .ok_or((400u16, "notify.invalid_token"))?;
            let user_id: runbook_model::UserId =
                user.parse().map_err(|_| (400u16, "notify.invalid_token"))?;
            let (_, salt) = notify
                .email_preference(&user_id)
                .await
                .map_err(|_| (503u16, "storage.unavailable"))?
                .ok_or((404u16, "notify.invalid_token"))?;
            let salt = salt.ok_or((404u16, "notify.invalid_token"))?;
            if !unsubscribe_token_matches(&salt, &user_id, &body.token) {
                return Err((404, "notify.invalid_token"));
            }
            notify
                .set_email_preference(&user_id, EmailPreference::None)
                .await
                .map_err(|_| (503u16, "storage.unavailable"))?;
            Ok((200, json!(UnsubscribeView { unsubscribed: true })))
        }
        ("GET", "/v1/me/email-preferences") => {
            let (record, _) = authenticate::<B>(&credentials, &directory, r).await?;
            let user = principal_user(&record)?;
            let (preference, _) = backends
                .notify()
                .email_preference(&user)
                .await
                .map_err(|_| (503u16, "storage.unavailable"))?
                .unwrap_or_default();
            Ok((
                200,
                json!(EmailPreferencesView {
                    preference: preference_view(preference)
                }),
            ))
        }
        ("PUT", "/v1/me/email-preferences") => {
            let (record, _) = authenticate::<B>(&credentials, &directory, r).await?;
            let user = principal_user(&record)?;
            let body: EmailPreferencesRequest =
                serde_json::from_value(r.body.clone()).map_err(|_| (422u16, "request.invalid"))?;
            backends
                .notify()
                .set_email_preference(&user, preference_of(body.preference))
                .await
                .map_err(|_| (503u16, "storage.unavailable"))?;
            Ok((
                200,
                json!(EmailPreferencesView {
                    preference: body.preference
                }),
            ))
        }
        ("GET", APPS) => {
            let (record, _) = authenticate::<B>(&credentials, &directory, r).await?;
            let user = person(&record)?;
            let auth_users = directory
                .auth_users_of(&user)
                .await
                .map_err(|_| (503, "storage.unavailable"))?;
            let grants = if auth_users.is_empty() {
                None
            } else {
                backends
                    .oauth_apps("oauth/grants", json!({ "auth_user_ids": auth_users }))
                    .await
                    .map_err(|_| (503, "auth.unavailable"))?
            };
            let items = grants
                .as_ref()
                .and_then(|g| g["grants"].as_array())
                .map(|g| g.iter().filter_map(app_view).collect())
                .unwrap_or_default();
            Ok((200, json!(runbook_model::ConnectedAppPage { items })))
        }
        ("DELETE", path) if app_id(path).is_some() => {
            let client = app_id(path).unwrap_or_default();
            let (record, _) = authenticate::<B>(&credentials, &directory, r).await?;
            let user = person(&record)?;
            let auth_users = directory
                .auth_users_of(&user)
                .await
                .map_err(|_| (503, "storage.unavailable"))?;
            if auth_users.is_empty() {
                return Err((404, "resource.not_found"));
            }
            let answer = backends
                .oauth_apps(
                    "oauth/grants/revoke",
                    json!({ "auth_user_ids": auth_users, "client_id": client }),
                )
                .await
                .map_err(|_| (503, "auth.unavailable"))?;
            match answer {
                Some(v) if v["revoked"] == json!(true) => Ok((204, serde_json::Value::Null)),
                _ => Err((404, "resource.not_found")),
            }
        }
        _ => Err((405, "request.method_not_allowed")),
    }
}
