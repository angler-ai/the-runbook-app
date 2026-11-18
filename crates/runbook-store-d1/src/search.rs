//! B3.3/D22 run search on D1: words over title and tags (FTS5 narrows, the
//! stored words decide), tag and field containment through `json_each`,
//! numeric field ranges, keyset pages newest first.
//!
//! Differences from Postgres, recorded in CF3: text matches whole words as
//! `memory_entries::text_words` splits them (Postgres's `simple` parser also
//! indexes hyphenated compounds whole), and field equality compares JSON
//! scalars by type and value.
use crate::{
    applier::summary_words,
    catalog::{D1Catalog, decode, err, label},
    sql::{D1, Statement, Value, prepare},
};
use runbook_core::{
    StoreError,
    catalog::RunSummary,
    memory_entries::text_words,
    search::{FieldFilter, RunFilters, RunPage, cursor_for, cursor_from},
};
use std::collections::BTreeSet;

/// One field filter's SQL over `r.fields`, with its bindings.
fn field_condition(filter: &FieldFilter) -> (String, Vec<Value>) {
    match filter {
        FieldFilter::Equals { field, value } => {
            let (test, bound): (&str, Vec<Value>) = match value {
                serde_json::Value::String(text) => (
                    "f.type = 'text' AND f.value = ?",
                    vec![text.as_str().into()],
                ),
                serde_json::Value::Bool(true) => ("f.type = 'true'", vec![]),
                serde_json::Value::Bool(false) => ("f.type = 'false'", vec![]),
                serde_json::Value::Null => ("f.type = 'null'", vec![]),
                serde_json::Value::Number(number) => (
                    "f.type IN ('integer', 'real') AND f.value = CAST(? AS REAL)",
                    vec![number.to_string().into()],
                ),
                // Arrays and objects: the stored JSON text must be equal.
                other => ("json(f.value) = json(?)", vec![other.to_string().into()]),
            };
            let mut params = vec![field.as_str().into()];
            params.extend(bound);
            (
                format!("EXISTS (SELECT 1 FROM json_each(r.fields) f WHERE f.key = ? AND {test})"),
                params,
            )
        }
        FieldFilter::Range { field, min, max } => {
            // Numeric only: strings and booleans never satisfy a range (R14).
            let mut sql = String::from(
                "EXISTS (SELECT 1 FROM json_each(r.fields) f WHERE f.key = ? \
                 AND f.type IN ('integer', 'real')",
            );
            let mut params: Vec<Value> = vec![field.as_str().into()];
            if let Some(min) = min {
                sql.push_str(" AND f.value >= CAST(? AS REAL)");
                params.push(min.to_string().into());
            }
            if let Some(max) = max {
                sql.push_str(" AND f.value < CAST(? AS REAL)");
                params.push(max.to_string().into());
            }
            sql.push(')');
            (sql, params)
        }
    }
}

impl<D: D1> D1Catalog<D> {
    pub async fn search_runs(
        &self,
        filters: RunFilters,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<RunPage, StoreError> {
        if limit == 0 {
            return Err(StoreError::InvalidBatch("query.invalid_limit"));
        }
        // D22: only indexed fields (what stored summaries in scope carry).
        let requested: BTreeSet<&str> = filters.fields.iter().map(FieldFilter::field).collect();
        if !requested.is_empty() {
            let mut checks = vec![];
            for field in &requested {
                let mut statement = prepare(format!(
                    "SELECT EXISTS (SELECT 1 FROM run_summary r JOIN execution_owner o \
                     ON o.execution_id = r.execution_id \
                     WHERE o.org_id = ? AND o.project_id = ?{} \
                     AND EXISTS (SELECT 1 FROM json_each(r.fields) f WHERE f.key = ?)) AS known",
                    if filters.runbook_id.is_some() {
                        " AND r.runbook_id = ?"
                    } else {
                        ""
                    }
                ))
                .bind(self.org())
                .bind(self.project());
                if let Some(runbook) = &filters.runbook_id {
                    statement = statement.bind(runbook.as_str());
                }
                checks.push(statement.bind(*field));
            }
            for outcome in self.db.batch(checks).await.map_err(err)? {
                let known = outcome
                    .rows
                    .first()
                    .map(|row| crate::sql::Columns::flag(row, 0))
                    .transpose()
                    .map_err(err)?
                    .unwrap_or(false);
                if !known {
                    return Err(StoreError::InvalidBatch("query.field_not_indexed"));
                }
            }
        }
        let after = cursor
            .map(|c| cursor_from(&self.scope, &filters, &c))
            .transpose()?;
        let mut sql = String::from(
            "SELECT r.body FROM run_summary r JOIN execution_owner o \
             ON o.execution_id = r.execution_id WHERE o.org_id = ? AND o.project_id = ?",
        );
        let mut params: Vec<Value> = vec![self.org().into(), self.project().into()];
        if !filters.statuses.is_empty() {
            sql.push_str(&format!(
                " AND r.status IN ({})",
                crate::sql::placeholders(filters.statuses.len())
            ));
            for status in &filters.statuses {
                params.push(label(status)?.into());
            }
        }
        if let Some(runbook) = &filters.runbook_id {
            sql.push_str(" AND r.runbook_id = ?");
            params.push(runbook.as_str().into());
        }
        for tag in &filters.tags_all {
            sql.push_str(" AND EXISTS (SELECT 1 FROM json_each(r.tags) t WHERE t.value = ?)");
            params.push(tag.as_str().into());
        }
        if let Some(text) = &filters.text {
            let words = text_words(text);
            if !words.is_empty() {
                sql.push_str(
                    " AND r.rowid IN (SELECT rowid FROM run_summary_fts WHERE run_summary_fts MATCH ?)",
                );
                params.push(
                    words
                        .iter()
                        .map(|w| format!("\"{w}\""))
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into(),
                );
                for word in &words {
                    sql.push_str(" AND instr(' ' || r.search_tsv || ' ', ' ' || ? || ' ') > 0");
                    params.push(word.as_str().into());
                }
            }
        }
        if let Some(from) = filters.created_from_ms {
            sql.push_str(" AND r.created_at_ms >= ?");
            params.push(from.into());
        }
        if let Some(to) = filters.created_to_ms {
            sql.push_str(" AND r.created_at_ms < ?");
            params.push(to.into());
        }
        if let Some(actor) = &filters.actor {
            sql.push_str(" AND r.created_by = ?");
            params.push(actor.as_str().into());
        }
        for filter in &filters.fields {
            let (condition, bound) = field_condition(filter);
            sql.push_str(" AND ");
            sql.push_str(&condition);
            params.extend(bound);
        }
        if let Some((created, id)) = &after {
            sql.push_str(
                " AND (r.created_at_ms < ? OR (r.created_at_ms = ? AND r.execution_id < ?))",
            );
            params.push((*created).into());
            params.push((*created).into());
            params.push(id.as_str().into());
        }
        sql.push_str(" ORDER BY r.created_at_ms DESC, r.execution_id DESC LIMIT ?");
        params.push(crate::catalog::count(limit + 1)?);
        let mut statement: Statement = prepare(sql);
        statement.params = params;
        let rows = self.db.all(statement).await.map_err(err)?;
        let mut runs: Vec<RunSummary> = rows
            .iter()
            .map(|row| decode(row, 0))
            .collect::<Result<_, _>>()?;
        let next_cursor =
            (runs.len() > limit).then(|| cursor_for(&self.scope, &filters, &runs[limit - 1]));
        runs.truncate(limit);
        Ok(RunPage { runs, next_cursor })
    }
}

/// Keep the words helper reachable from tests of the stored column.
#[doc(hidden)]
pub fn words_of(summary: &RunSummary) -> String {
    summary_words(summary)
}

/// UI-1 review: the runbook list's run facts for one page in one query
/// (one D1 read, however many rows). Binds the tenant twice, then the cap,
/// then the runbook IDs as a JSON array. Each ID's newest run is one probe
/// of `run_summary_runbook_created` (`runbook_id, created_at_ms DESC,
/// execution_id DESC`); its active runs are counted from the partial
/// `run_summary_runbook_active`, whose predicate is this status list
/// verbatim (SQLite uses a partial index only when the query repeats its
/// terms). UI-1b: the count stops at the cap in SQL (a `LIMIT` inside the
/// counted subquery, which SQLite correlates), as Postgres does, so a
/// runbook with many open runs reads at most the cap's index rows and
/// owner probes. Exposed for the plan test.
#[doc(hidden)]
pub const RUNBOOK_RUN_STATS_SQL: &str = "\
SELECT k.value, \
  (SELECT r.body FROM run_summary r JOIN execution_owner o ON o.execution_id = r.execution_id \
   WHERE r.runbook_id = k.value AND o.org_id = ? AND o.project_id = ? \
   ORDER BY r.created_at_ms DESC, r.execution_id DESC LIMIT 1), \
  (SELECT count(*) FROM ( \
     SELECT 1 FROM run_summary a JOIN execution_owner ao ON ao.execution_id = a.execution_id \
     WHERE a.runbook_id = k.value \
       AND a.status IN ('queued', 'running', 'waiting', 'paused') \
       AND ao.org_id = ? AND ao.project_id = ? LIMIT ?)) \
FROM json_each(?) k";

impl<D: D1> D1Catalog<D> {
    pub async fn runbook_run_stats(
        &self,
        runbook_ids: &[runbook_model::RunbookId],
        active_cap: usize,
    ) -> Result<
        std::collections::BTreeMap<runbook_model::RunbookId, runbook_core::search::RunbookRunStats>,
        StoreError,
    > {
        use crate::sql::Columns;
        use runbook_core::search::{ACTIVE_STATUSES, RunbookRunStats};
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
        let ids =
            serde_json::to_string(&runbook_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>())
                .map_err(|_| StoreError::Backend("d1.encode"))?;
        let statement = prepare(RUNBOOK_RUN_STATS_SQL)
            .bind(self.org())
            .bind(self.project())
            .bind(self.org())
            .bind(self.project())
            .bind(crate::catalog::count(active_cap)?)
            .bind(ids);
        let rows = self.db.all(statement).await.map_err(err)?;
        let mut out = std::collections::BTreeMap::new();
        for row in &rows {
            let id: runbook_model::RunbookId = row
                .text(0)
                .map_err(err)?
                .parse()
                .map_err(|_| StoreError::Backend("d1.invalid_stored_id"))?;
            let last_run: Option<RunSummary> = row.optional_json(1).map_err(err)?;
            let active = usize::try_from(row.integer(2).map_err(err)?).unwrap_or(0);
            out.insert(
                id,
                RunbookRunStats {
                    last_run,
                    active_runs: active.min(active_cap),
                },
            );
        }
        Ok(out)
    }
}
