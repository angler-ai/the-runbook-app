//! B8f `GET /v1/me/tasks`: the caller's open human work and approvals across
//! every project they can read. Only an active human using a human-kind
//! credential may call it; agent, service and execution credentials cannot.
//! Visibility grants no claim or vote authority: mutations reauthorize.
use crate::{Request, backends::Backends, problem::Response};
use runbook_core::{
    StoreError,
    catalog::AssignmentKind,
    credentials::{
        ActorKind, AuthError, CredentialPrincipal, CredentialStore, Principal, Scope, authorize,
    },
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    inbox::{InboxFilters, me_tasks_scope},
};
use runbook_model::{InboxPage, InboxTaskKind, InboxTaskView, ProjectId};

pub const PATH: &str = "/v1/me/tasks";
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok(page) => Response::json(200, serde_json::json!(page), &r.request_id),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

type Failure = (u16, &'static str);

fn query(r: &Request) -> Result<(InboxFilters, Option<String>, usize), Failure> {
    let bad = (400, "request.invalid_query");
    let mut filters = InboxFilters::default();
    let mut cursor = None;
    let mut limit = DEFAULT_LIMIT;
    for (key, value) in &r.query {
        match key.as_str() {
            "kind" => {
                for kind in value.split(',') {
                    let kinds: &[AssignmentKind] = match kind {
                        "approval" => &[AssignmentKind::Approval],
                        "human_task" => &[AssignmentKind::Work, AssignmentKind::Handoff],
                        _ => return Err(bad),
                    };
                    for kind in kinds {
                        if !filters.kinds.contains(kind) {
                            filters.kinds.push(*kind);
                        }
                    }
                }
                filters.kinds.sort_by_key(|k| format!("{k:?}"));
            }
            "cursor" => cursor = Some(value.clone()),
            "limit" => {
                limit = value.parse().map_err(|_| bad)?;
                if !(1..=MAX_LIMIT).contains(&limit) {
                    return Err(bad);
                }
            }
            _ => return Err(bad),
        }
    }
    Ok((filters, cursor, limit))
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<InboxPage, Failure> {
    if r.method != "GET" {
        return Err((405, "request.method_not_allowed"));
    }
    let unavailable = (503, "storage.unavailable");
    let credentials = backends.credentials();
    let record = r
        .credential_record(&credentials)
        .await
        .map_err(|e| e.status())?;
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(|_| unavailable)?;
    if record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err((401, "auth.invalid_credential"));
    }
    // A person's own inbox: never a service token or an agent-kind PAT.
    let CredentialPrincipal::User {
        user_id,
        actor: ActorKind::Human,
    } = &record.principal
    else {
        return Err((403, "auth.forbidden"));
    };
    let (filters, cursor, limit) = query(r)?;
    let snapshot = directory
        .snapshot(&record.org_id)
        .await
        .map_err(|_| unavailable)?;
    if !snapshot.users.get(user_id).is_some_and(|user| user.active) {
        return Err((403, "auth.forbidden"));
    }
    // Directory project access intersected with the credential's
    // project restrictions and executions:read (team membership alone
    // grants no project access).
    let mut readable: Vec<ProjectId> = vec![];
    let mut scope_missing = false;
    for project in snapshot
        .projects
        .keys()
        .filter(|project| snapshot.project_role(project, user_id).is_some())
    {
        match authorize(&record, &snapshot, now, project, Scope::ExecutionsRead) {
            Ok(Principal::Human { .. }) => readable.push(project.clone()),
            Ok(_) => return Err((403, "auth.forbidden")),
            Err(AuthError::Forbidden) if !record.scopes.contains(&Scope::ExecutionsRead) => {
                scope_missing = true;
            }
            Err(AuthError::Forbidden) => {}
            Err(_) => return Err((401, "auth.invalid_credential")),
        }
    }
    if scope_missing && readable.is_empty() {
        return Err((403, "auth.scope_missing"));
    }
    // R27: one indexed keyset query over the readable projects, with the
    // per-project inbox rules (active member, self plus teams, voters).
    let scope = me_tasks_scope(&snapshot, user_id, &readable);
    credentials
        .touch_last_used(&record.org_id, &record.id, now)
        .await
        .map_err(|_| unavailable)?;
    let page = backends
        .me_tasks(&scope, &filters, cursor.as_deref(), limit)
        .await
        .map_err(|e| match e {
            StoreError::InvalidBatch(_) => (400, "request.invalid_cursor"),
            _ => unavailable,
        })?;
    Ok(InboxPage {
        items: page
            .rows
            .into_iter()
            .map(|row| {
                let a = row.assignment;
                InboxTaskView {
                    project_id: row.project_id,
                    kind: match a.kind {
                        AssignmentKind::Approval => InboxTaskKind::Approval,
                        AssignmentKind::Work | AssignmentKind::Handoff => InboxTaskKind::HumanTask,
                    },
                    // UI-2: the assignment's last change (0 in rows
                    // written before B8f kept it: unknown, so omitted).
                    since_ms: (a.updated_at_ms > 0).then_some(a.updated_at_ms),
                    execution_id: a.execution_id,
                    step_run_id: a.step_run_id,
                    path: a.path,
                    title: a.title,
                    execution_title: row.execution_title,
                }
            })
            .collect(),
        next_cursor: page.next_cursor,
    })
}
