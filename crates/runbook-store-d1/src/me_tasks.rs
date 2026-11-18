//! R27 `/v1/me/tasks` on D1: one keyset query over the inbox
//! (`assignment`), the same rows, order and cursors as merging every
//! readable project's inbox in memory (`runbook_core::inbox::merge_pages`).
use crate::{
    applier::recipient_key,
    catalog::{count, decode, err},
    sql::{Columns, D1, Value, placeholders, prepare},
};
use runbook_core::{
    StoreError,
    catalog::AssignmentKind,
    inbox::{InboxFilters, MeTaskRow, MeTasksPage, MeTasksScope, me_tasks_after, me_tasks_cursor},
};
use runbook_model::RecipientRef;

fn kind_label(kind: AssignmentKind) -> &'static str {
    match kind {
        AssignmentKind::Work => "work",
        AssignmentKind::Approval => "approval",
        AssignmentKind::Handoff => "handoff",
    }
}

/// Open assignments for `scope` across its projects, newest first. An
/// invalid or foreign cursor is `InvalidBatch("request.invalid_cursor")`.
pub async fn me_tasks<D: D1>(
    db: &D,
    scope: &MeTasksScope,
    filters: &InboxFilters,
    cursor: Option<&str>,
    limit: usize,
) -> Result<MeTasksPage, StoreError> {
    if limit == 0 {
        return Err(StoreError::InvalidBatch("query.invalid_limit"));
    }
    let after = cursor
        .map(|c| me_tasks_after(&scope.org_id, &scope.user, filters, c))
        .transpose()?;
    if scope.projects.is_empty() {
        return Ok(MeTasksPage {
            rows: vec![],
            next_cursor: None,
        });
    }
    let mut recipients = vec![recipient_key(&RecipientRef::User(scope.user.clone()))];
    recipients.extend(scope.teams.iter().map(|t| format!("team:{t}")));
    let mut sql = format!(
        "SELECT a.body, o.project_id, coalesce(s.title, '') AS execution_title \
         FROM assignment a JOIN execution_owner o ON o.execution_id = a.execution_id \
         LEFT JOIN run_summary s ON s.execution_id = a.execution_id \
         WHERE o.org_id = ? AND o.project_id IN ({}) \
         AND EXISTS (SELECT 1 FROM json_each(a.recipients) r WHERE r.value IN ({})) \
         AND NOT EXISTS (SELECT 1 FROM json_each(a.voted_by) v WHERE v.value = ?)",
        placeholders(scope.projects.len()),
        placeholders(recipients.len()),
    );
    let mut params: Vec<Value> = vec![scope.org_id.as_str().into()];
    params.extend(scope.projects.iter().map(|p| Value::from(p.as_str())));
    params.extend(recipients.into_iter().map(Value::from));
    params.push(scope.user.as_str().into());
    if !filters.kinds.is_empty() {
        sql.push_str(&format!(
            " AND a.kind IN ({})",
            placeholders(filters.kinds.len())
        ));
        params.extend(filters.kinds.iter().map(|k| Value::from(kind_label(*k))));
    }
    if let Some((updated, execution, step)) = &after {
        // Byte order matches the Rust `(i64, ExecutionId, StepRunId)` key.
        sql.push_str(
            " AND (a.updated_at_ms < ? OR (a.updated_at_ms = ? AND (a.execution_id < ? \
             OR (a.execution_id = ? AND a.step_run_id < ?))))",
        );
        params.push((*updated).into());
        params.push((*updated).into());
        params.push(execution.as_str().into());
        params.push(execution.as_str().into());
        params.push(step.as_str().into());
    }
    sql.push_str(" ORDER BY a.updated_at_ms DESC, a.execution_id DESC, a.step_run_id DESC LIMIT ?");
    params.push(count(limit.saturating_add(1))?);
    let mut statement = prepare(sql);
    statement.params = params;
    let mut rows = db
        .all(statement)
        .await
        .map_err(err)?
        .iter()
        .map(|row| {
            Ok(MeTaskRow {
                assignment: decode(row, 0)?,
                project_id: row.parse(1).map_err(err)?,
                execution_title: row.string(2).map_err(err)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let next_cursor = if rows.len() > limit {
        Some(me_tasks_cursor(
            &scope.org_id,
            &scope.user,
            filters,
            &rows[limit - 1].assignment,
        )?)
    } else {
        None
    };
    rows.truncate(limit);
    Ok(MeTasksPage { rows, next_cursor })
}
