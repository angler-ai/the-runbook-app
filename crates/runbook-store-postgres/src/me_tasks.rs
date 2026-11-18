//! R27: `/v1/me/tasks` as one indexed keyset query. Same rows, order and
//! cursors as merging every readable project's `inbox` in memory
//! (`runbook_core::inbox::merge_pages`); the parity test pins that.
use crate::{
    PostgresAuthoring,
    catalog::assignment_kind_label,
    rows::{connection, db, decode},
};
use runbook_core::{
    StoreError,
    inbox::{InboxFilters, MeTaskRow, MeTasksPage, MeTasksScope, me_tasks_after, me_tasks_cursor},
};

const SELECT: &str = "SELECT a.body, o.project_id, COALESCE(s.title, '') \
     FROM runbook.assignment a \
     JOIN runbook.execution_owner o USING (execution_id) \
     LEFT JOIN runbook.run_summary s USING (execution_id) \
     WHERE o.org_id = $1 AND o.project_id = ANY($2::text[]) \
       AND a.recipients && $3::text[] \
       AND NOT ($4::text = ANY(a.voted_by)) \
       AND (cardinality($5::text[]) = 0 OR a.kind = ANY($5::text[]))";
const AFTER: &str = " AND (a.updated_at_ms, a.execution_id COLLATE \"C\", a.step_run_id COLLATE \"C\") \
     < ($7::bigint, $8::text COLLATE \"C\", $9::text COLLATE \"C\")";
// Byte order matches the Rust `(i64, ExecutionId, StepRunId)` cursor key.
const ORDER: &str = " ORDER BY a.updated_at_ms DESC, a.execution_id COLLATE \"C\" DESC, \
     a.step_run_id COLLATE \"C\" DESC LIMIT $6";

impl PostgresAuthoring {
    /// Open assignments for `scope` across its projects, newest first. An
    /// invalid or foreign cursor is `InvalidBatch("request.invalid_cursor")`.
    pub async fn me_tasks(
        &self,
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
        let projects: Vec<&str> = scope.projects.iter().map(|p| p.as_str()).collect();
        let mut recipients = vec![format!("user:{}", scope.user)];
        recipients.extend(scope.teams.iter().map(|t| format!("team:{t}")));
        let kinds: Vec<&str> = filters
            .kinds
            .iter()
            .map(|k| assignment_kind_label(*k))
            .collect();
        let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        let org = scope.org_id.as_str();
        let user = scope.user.as_str();
        let conn = self.pool.get().await.map_err(connection)?;
        let found = match &after {
            None => {
                conn.client()
                    .query(
                        &format!("{SELECT}{ORDER}"),
                        &[&org, &projects, &recipients, &user, &kinds, &fetch],
                    )
                    .await
            }
            Some((updated, execution, step)) => {
                conn.client()
                    .query(
                        &format!("{SELECT}{AFTER}{ORDER}"),
                        &[
                            &org,
                            &projects,
                            &recipients,
                            &user,
                            &kinds,
                            &fetch,
                            updated,
                            &execution.as_str(),
                            &step.as_str(),
                        ],
                    )
                    .await
            }
        }
        .map_err(db)?;
        let mut rows = found
            .iter()
            .map(|row| {
                Ok(MeTaskRow {
                    assignment: decode(row.get(0))?,
                    project_id: row
                        .get::<_, &str>(1)
                        .parse()
                        .map_err(|_| StoreError::Backend("catalog.invalid_project"))?,
                    execution_title: row.get(2),
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
}
