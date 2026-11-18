//! B3.3/D22 run search on PostgreSQL: tsvector text, GIN tag/field
//! containment and JSONB field comparisons over the title/tags/fields
//! columns written with each derived summary.
use crate::{
    PostgresExecutionStore,
    execution::SNAPSHOT_READ,
    rows::{db, decode, label},
};
use runbook_core::{
    StoreError,
    catalog::RunSummary,
    search::{FieldFilter, RunFilters, RunPage, cursor_for, cursor_from},
};
use std::collections::BTreeSet;
use tokio_postgres::types::ToSql;

impl PostgresExecutionStore {
    pub(crate) async fn search_rows(
        &self,
        filters: RunFilters,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<RunPage, StoreError> {
        if limit == 0 {
            return Err(StoreError::InvalidBatch("query.invalid_limit"));
        }
        let (org, project) = (self.scope.org_id.as_str(), self.scope.project_id.as_str());
        self.transaction(SNAPSHOT_READ, async |c| {
            // The indexed set is what stored summaries in scope project;
            // anything else is explicitly rejected (D22). F36: only the
            // filtered fields are checked, each through the GIN index on
            // `fields`, instead of listing every key of every summary.
            let runbook_param = filters.runbook_id.as_ref().map(|id| id.as_str());
            let requested: BTreeSet<&str> = filters.fields.iter().map(|f| f.field()).collect();
            for field in requested {
                let mut sql = String::from(
                    "SELECT EXISTS (SELECT 1 FROM runbook.run_summary r \
                     JOIN runbook.execution_owner o USING (execution_id) \
                     WHERE r.fields ? $3 AND o.org_id = $1 AND o.project_id = $2",
                );
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![&org, &project, &field];
                if let Some(runbook) = &runbook_param {
                    sql.push_str(" AND r.runbook_id = $4");
                    params.push(runbook);
                }
                sql.push(')');
                let known: bool = c.query_one(&sql, &params).await.map_err(db)?.get(0);
                if !known {
                    return Err(StoreError::InvalidBatch("query.field_not_indexed"));
                }
            }
            let after = cursor
                .map(|c| cursor_from(&self.scope, &filters, &c))
                .transpose()?;

            // Owned parameter values; the reference list borrows them.
            let status_labels: Vec<String> = filters
                .statuses
                .iter()
                .map(label)
                .collect::<Result<_, _>>()?;
            let equals_json: Vec<String> = filters
                .fields
                .iter()
                .filter_map(|f| match f {
                    FieldFilter::Equals { field, value } => {
                        Some(serde_json::json!({ field: value }).to_string())
                    }
                    _ => None,
                })
                .collect();
            let ranges: Vec<(&str, Option<String>, Option<String>)> = filters
                .fields
                .iter()
                .filter_map(|f| match f {
                    FieldFilter::Range { field, min, max } => Some((
                        field.as_str(),
                        min.as_ref().map(|v| v.to_string()),
                        max.as_ref().map(|v| v.to_string()),
                    )),
                    _ => None,
                })
                .collect();
            let fetch = i64::try_from(limit + 1)
                .map_err(|_| StoreError::InvalidBatch("query.invalid_limit"))?;

            let mut sql = String::from(
                "SELECT r.body FROM runbook.run_summary r \
                 JOIN runbook.execution_owner o USING (execution_id) \
                 WHERE o.org_id = $1 AND o.project_id = $2",
            );
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![&org, &project];
            let mut add = |condition: String| {
                sql.push_str(" AND ");
                sql.push_str(&condition);
            };
            let next = |params: &Vec<&(dyn ToSql + Sync)>| params.len() + 1;
            if !status_labels.is_empty() {
                let at = next(&params);
                add(format!("r.status = ANY(${at})"));
                params.push(&status_labels);
            }
            if let Some(runbook) = &runbook_param {
                let at = next(&params);
                add(format!("r.runbook_id = ${at}"));
                params.push(runbook);
            }
            if !filters.tags_all.is_empty() {
                let at = next(&params);
                add(format!("r.tags @> ${at}"));
                params.push(&filters.tags_all);
            }
            if let Some(text) = &filters.text {
                let at = next(&params);
                add(format!("r.search_tsv @@ plainto_tsquery('simple', ${at})"));
                params.push(text);
            }
            if let Some(from) = &filters.created_from_ms {
                let at = next(&params);
                add(format!("r.created_at_ms >= ${at}"));
                params.push(from);
            }
            if let Some(to) = &filters.created_to_ms {
                let at = next(&params);
                add(format!("r.created_at_ms < ${at}"));
                params.push(to);
            }
            if let Some(actor) = &filters.actor {
                let at = next(&params);
                add(format!("r.created_by = ${at}"));
                params.push(actor);
            }
            for json in &equals_json {
                let at = next(&params);
                add(format!("r.fields @> (${at}::text)::jsonb"));
                params.push(json);
            }
            for (field, min, max) in &ranges {
                // Ranges are numeric only: jsonb type ordering would
                // otherwise admit strings and booleans (R14).
                let field_at = next(&params);
                add(format!("jsonb_typeof(r.fields -> ${field_at}) = 'number'"));
                params.push(field);
                if let Some(min) = min {
                    let (field_at, value_at) = (next(&params), next(&params) + 1);
                    add(format!(
                        "(r.fields -> ${field_at}) >= (${value_at}::text)::jsonb"
                    ));
                    params.push(field);
                    params.push(min);
                }
                if let Some(max) = max {
                    let (field_at, value_at) = (next(&params), next(&params) + 1);
                    add(format!(
                        "(r.fields -> ${field_at}) < (${value_at}::text)::jsonb"
                    ));
                    params.push(field);
                    params.push(max);
                }
            }
            // Cursor continues strictly after its (created, id) position.
            let after_created = after.as_ref().map(|(created, _)| *created);
            let after_id = after.as_ref().map(|(_, id)| id.as_str().to_owned());
            if let (Some(created), Some(id)) = (&after_created, &after_id) {
                let (created_at, id_at) = (next(&params), next(&params) + 1);
                add(format!(
                    "(r.created_at_ms < ${created_at} OR \
                      (r.created_at_ms = ${created_at} AND r.execution_id < ${id_at}))"
                ));
                params.push(created);
                params.push(id);
            }
            let limit_at = next(&params);
            sql.push_str(&format!(
                " ORDER BY r.created_at_ms DESC, r.execution_id DESC LIMIT ${limit_at}"
            ));
            params.push(&fetch);
            let rows = c.query(&sql, &params[..]).await.map_err(db)?;
            let mut runs: Vec<RunSummary> = rows
                .iter()
                .map(|row| decode(row.get(0)))
                .collect::<Result<_, _>>()?;
            let next_cursor =
                (runs.len() > limit).then(|| cursor_for(&self.scope, &filters, &runs[limit - 1]));
            runs.truncate(limit);
            Ok(RunPage { runs, next_cursor })
        })
        .await
    }
}

/// UI-1 review: the runbook list's run facts for one page in one query.
/// `$1`/`$2` the tenant, `$3` the runbook IDs, `$4` the active cap. Each
/// ID's newest run is one probe of `run_summary_runbook_created`
/// (`runbook_id, created_at_ms DESC, execution_id DESC`); its active
/// runs are read from the partial `run_summary_runbook_active`, whose
/// predicate is this status list verbatim (the planner proves the
/// implication only from the same list), stopping at the cap. Exposed for
/// the plan test (`tests/search.rs`).
#[doc(hidden)]
pub const RUNBOOK_RUN_STATS_SQL: &str = "\
SELECT ids.runbook_id, last.body, \
  (SELECT count(*) FROM (SELECT 1 FROM runbook.run_summary a \
     JOIN runbook.execution_owner ao USING (execution_id) \
     WHERE a.runbook_id = ids.runbook_id \
       AND a.status IN ('queued', 'running', 'waiting', 'paused') \
       AND ao.org_id = $1 AND ao.project_id = $2 \
     LIMIT $4) capped) AS active \
FROM unnest($3::text[]) AS ids(runbook_id) \
LEFT JOIN LATERAL ( \
  SELECT r.body FROM runbook.run_summary r \
  JOIN runbook.execution_owner o USING (execution_id) \
  WHERE r.runbook_id = ids.runbook_id AND o.org_id = $1 AND o.project_id = $2 \
  ORDER BY r.created_at_ms DESC, r.execution_id DESC LIMIT 1 \
) last ON true";

impl PostgresExecutionStore {
    pub(crate) async fn runbook_run_stats_rows(
        &self,
        runbook_ids: &[runbook_model::RunbookId],
        active_cap: usize,
    ) -> Result<
        std::collections::BTreeMap<runbook_model::RunbookId, runbook_core::search::RunbookRunStats>,
        StoreError,
    > {
        use runbook_core::search::{ACTIVE_STATUSES, RunbookRunStats};
        // The SQL names the active statuses literally; keep it in step.
        debug_assert_eq!(
            ACTIVE_STATUSES
                .iter()
                .map(|s| label(s).unwrap())
                .collect::<Vec<_>>(),
            ["queued", "running", "waiting", "paused"]
        );
        if runbook_ids.is_empty() {
            return Ok(Default::default());
        }
        let (org, project) = (self.scope.org_id.as_str(), self.scope.project_id.as_str());
        let ids: Vec<&str> = runbook_ids.iter().map(|id| id.as_str()).collect();
        let cap = i64::try_from(active_cap)
            .map_err(|_| StoreError::InvalidBatch("query.invalid_limit"))?;
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        let rows = conn
            .client()
            .query(RUNBOOK_RUN_STATS_SQL, &[&org, &project, &ids, &cap])
            .await
            .map_err(db)?;
        let mut out = std::collections::BTreeMap::new();
        for row in rows {
            let id: runbook_model::RunbookId = row
                .get::<_, &str>(0)
                .parse()
                .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?;
            let last_run: Option<RunSummary> =
                row.get::<_, Option<&str>>(1).map(decode).transpose()?;
            let active: i64 = row.get(2);
            out.insert(
                id,
                RunbookRunStats {
                    last_run,
                    active_runs: usize::try_from(active).unwrap_or(0),
                },
            );
        }
        Ok(out)
    }
}
