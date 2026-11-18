//! Inbox query (B8f): open assignments intersecting the caller's current
//! directory access — self plus team memberships, D16-active only —
//! within the store's project scope. Newest assignment first, D33-style
//! scope-and-filter-bound cursors.
use crate::{
    StoreError,
    catalog::{Assignment, AssignmentKind, CatalogStore, TenantScope},
    directory::DirectorySnapshot,
};
use runbook_model::{RecipientRef, UserId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxFilters {
    #[serde(default)]
    pub kinds: Vec<AssignmentKind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxPage {
    pub assignments: Vec<Assignment>,
    pub next_cursor: Option<String>,
}

fn filters_hash(scope: &TenantScope, user: &UserId, filters: &InboxFilters) -> String {
    let canonical = serde_json::to_string(&(scope, user, filters)).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());
    digest[..8]
        .iter()
        .fold(String::with_capacity(16), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}
/// Keyset position `(updated_at_ms, execution_id, step_run_id)`: rows
/// strictly below it (newest first, byte order of the ids) come next.
pub type CursorKey = (i64, runbook_model::ExecutionId, runbook_model::StepRunId);
fn cursor_for(
    scope: &TenantScope,
    user: &UserId,
    filters: &InboxFilters,
    last: &Assignment,
) -> String {
    format!(
        "v1.{}.{}.{}.{}",
        last.updated_at_ms,
        last.execution_id,
        last.step_run_id,
        filters_hash(scope, user, filters)
    )
}
fn cursor_from(
    scope: &TenantScope,
    user: &UserId,
    filters: &InboxFilters,
    cursor: &str,
) -> Result<CursorKey, StoreError> {
    let invalid = || StoreError::InvalidBatch("request.invalid_cursor");
    let mut parts = cursor.splitn(5, '.');
    if parts.next() != Some("v1") {
        return Err(invalid());
    }
    let updated: i64 = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let execution: runbook_model::ExecutionId = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let step: runbook_model::StepRunId = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if parts.next() != Some(filters_hash(scope, user, filters).as_str()) || parts.next().is_some() {
        return Err(invalid());
    }
    Ok((updated, execution, step))
}
fn key_of(assignment: &Assignment) -> CursorKey {
    (
        assignment.updated_at_ms,
        assignment.execution_id.clone(),
        assignment.step_run_id.clone(),
    )
}

/// The caller's current recipient identities: self plus teams, in this
/// directory snapshot. Deactivated or unknown users see an empty inbox.
pub async fn inbox(
    store: &impl CatalogStore,
    scope: &TenantScope,
    directory: &DirectorySnapshot,
    user: &UserId,
    filters: InboxFilters,
    cursor: Option<String>,
    limit: usize,
) -> Result<InboxPage, StoreError> {
    if limit == 0 {
        return Err(StoreError::InvalidBatch("query.invalid_limit"));
    }
    let after = cursor
        .map(|c| cursor_from(scope, user, &filters, &c))
        .transpose()?;
    let active = directory
        .users
        .get(user)
        .is_some_and(|record| record.active)
        && directory.project_role(&scope.project_id, user).is_some();
    if !active {
        return Ok(InboxPage {
            assignments: vec![],
            next_cursor: None,
        });
    }
    let teams: std::collections::BTreeSet<String> = directory
        .team_members
        .iter()
        .filter(|(_, member)| member == user)
        .map(|(team, _)| team.to_string())
        .collect();
    let visible = |assignment: &Assignment| {
        (filters.kinds.is_empty() || filters.kinds.contains(&assignment.kind))
            && !assignment.voted_by.contains(user)
            && assignment
                .recipients
                .iter()
                .any(|recipient| match recipient {
                    RecipientRef::User(recipient) => recipient == user,
                    RecipientRef::Team(team) => teams.contains(team),
                })
    };
    let mut assignments: Vec<Assignment> = store
        .list_assignments()
        .await?
        .into_iter()
        .filter(visible)
        .filter(|assignment| {
            after
                .as_ref()
                .is_none_or(|cursor| key_of(assignment) < *cursor)
        })
        .collect();
    // One assignment per (execution, step run) by construction, so a user
    // matching several of its teams still sees it exactly once.
    assignments.sort_by_key(|assignment| std::cmp::Reverse(key_of(assignment)));
    let next_cursor = (assignments.len() > limit)
        .then(|| cursor_for(scope, user, &filters, &assignments[limit - 1]));
    assignments.truncate(limit);
    Ok(InboxPage {
        assignments,
        next_cursor,
    })
}

/// The merged `/v1/me/tasks` cursor binds the organization, user and
/// filters, never one project (D33): a fixed placeholder project keeps the
/// encoding identical to the per-project inbox cursor.
fn merged_scope(org_id: &runbook_model::OrganizationId) -> Result<TenantScope, StoreError> {
    Ok(TenantScope {
        org_id: org_id.clone(),
        project_id: "prj_00000000000000000000000000"
            .parse()
            .map_err(|_| StoreError::Backend("inbox.scope"))?,
    })
}

/// Cursor for the cross-project page ending at `last`.
pub fn me_tasks_cursor(
    org_id: &runbook_model::OrganizationId,
    user: &UserId,
    filters: &InboxFilters,
    last: &Assignment,
) -> Result<String, StoreError> {
    Ok(cursor_for(&merged_scope(org_id)?, user, filters, last))
}

/// Decode a cross-project cursor; it must carry this org, user and filters.
pub fn me_tasks_after(
    org_id: &runbook_model::OrganizationId,
    user: &UserId,
    filters: &InboxFilters,
    cursor: &str,
) -> Result<CursorKey, StoreError> {
    cursor_from(&merged_scope(org_id)?, user, filters, cursor)
}

/// Who and where `/v1/me/tasks` looks (R27), applying exactly the
/// per-project [`inbox`] rules: an active user, only readable projects the
/// user is a member of, and the user's own id plus current team ids as
/// recipient identities. Voters and kind filters apply per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeTasksScope {
    pub org_id: runbook_model::OrganizationId,
    pub user: UserId,
    pub teams: Vec<String>,
    pub projects: Vec<runbook_model::ProjectId>,
}

pub fn me_tasks_scope(
    directory: &DirectorySnapshot,
    user: &UserId,
    readable: &[runbook_model::ProjectId],
) -> MeTasksScope {
    let active = directory.users.get(user).is_some_and(|r| r.active);
    let projects = readable
        .iter()
        .filter(|project| active && directory.project_role(project, user).is_some())
        .cloned()
        .collect();
    let teams = directory
        .team_members
        .iter()
        .filter(|(_, member)| member == user)
        .map(|(team, _)| team.to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    MeTasksScope {
        org_id: directory.org_id.clone(),
        user: user.clone(),
        teams,
        projects,
    }
}

/// One `/v1/me/tasks` row: the assignment, its owning project and the
/// run title (empty when the run has no summary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeTaskRow {
    pub assignment: Assignment,
    pub project_id: runbook_model::ProjectId,
    pub execution_title: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeTasksPage {
    pub rows: Vec<MeTaskRow>,
    pub next_cursor: Option<String>,
}

/// Merge already-filtered per-project inbox lists into one cross-project
/// page: newest assignment first, each step run once, with a cursor bound
/// to the organization, user and filters (D33). `/v1/me/tasks` now runs one
/// indexed query instead (R27); this stays as the reference semantics the
/// Postgres parity test compares against.
pub fn merge_pages(
    org_id: &runbook_model::OrganizationId,
    user: &UserId,
    filters: &InboxFilters,
    lists: Vec<Vec<Assignment>>,
    cursor: Option<String>,
    limit: usize,
) -> Result<InboxPage, StoreError> {
    if limit == 0 {
        return Err(StoreError::InvalidBatch("query.invalid_limit"));
    }
    let scope = merged_scope(org_id)?;
    let after = cursor
        .map(|c| cursor_from(&scope, user, filters, &c))
        .transpose()?;
    let mut seen = std::collections::BTreeSet::new();
    let mut assignments: Vec<Assignment> = lists
        .into_iter()
        .flatten()
        .filter(|a| seen.insert(a.step_run_id.clone()))
        .filter(|a| after.as_ref().is_none_or(|cursor| key_of(a) < *cursor))
        .collect();
    assignments.sort_by_key(|a| std::cmp::Reverse(key_of(a)));
    let next_cursor = (assignments.len() > limit)
        .then(|| cursor_for(&scope, user, filters, &assignments[limit - 1]));
    assignments.truncate(limit);
    Ok(InboxPage {
        assignments,
        next_cursor,
    })
}
