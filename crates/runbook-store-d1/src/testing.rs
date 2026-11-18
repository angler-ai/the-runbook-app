//! Test seams (never called by the stores): empty every D1 table so one
//! database serves case after case, as workerd's single local D1 must.
use crate::sql::{Columns, D1, D1Error, prepare};

/// The application tables, from `sqlite_master` (not SQLite's, D1's,
/// the guard, the migration history or FTS shadow tables).
pub async fn tables<D: D1>(db: &D) -> Result<Vec<String>, D1Error> {
    db.all(prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         AND name NOT LIKE '_cf_%' AND name NOT LIKE 'd1_%' AND name <> 'schema_migration' \
         AND name NOT LIKE '%_fts%' ORDER BY name",
    ))
    .await?
    .iter()
    .map(|row| row.string(0))
    .collect()
}

/// Delete every row of every application table in one batch (foreign keys
/// checked at its end). The schema and its history stay.
#[doc(hidden)]
pub async fn reset<D: D1>(db: &D) -> Result<(), D1Error> {
    let mut batch = vec![prepare("PRAGMA defer_foreign_keys = ON")];
    for table in tables(db).await? {
        batch.push(prepare(format!("DELETE FROM \"{table}\"")));
    }
    db.batch(batch).await.map(|_| ())
}

/// Rows per application table (test inventory).
#[doc(hidden)]
pub async fn counts<D: D1>(db: &D) -> Result<Vec<(String, u64)>, D1Error> {
    let mut out = vec![];
    for table in tables(db).await? {
        let rows = db
            .all(prepare(format!("SELECT count(*) FROM \"{table}\"")))
            .await?;
        out.push((
            table,
            rows.first()
                .map(|r| r.unsigned(0))
                .transpose()?
                .unwrap_or(0),
        ));
    }
    Ok(out)
}

/// Rows per application table that name `execution_id` (test inventory of
/// what one run owns in D1). Tables that record a deletion and so outlive
/// it are left out: `tombstone`, `step_run_tombstone` and `archive_member`
/// (the archive file's membership).
#[doc(hidden)]
pub async fn run_counts<D: D1>(
    db: &D,
    execution_id: &runbook_model::ExecutionId,
) -> Result<Vec<(String, u64)>, D1Error> {
    let mut out = vec![];
    for table in tables(db).await? {
        if ["tombstone", "step_run_tombstone", "archive_member"].contains(&table.as_str()) {
            continue;
        }
        let columns = db
            .all(prepare(format!(
                "SELECT name FROM pragma_table_info('{table}')"
            )))
            .await?;
        if !columns
            .iter()
            .any(|row| row.text(0).is_ok_and(|name| name == "execution_id"))
        {
            continue;
        }
        let rows = db
            .all(
                prepare(format!(
                    "SELECT count(*) FROM \"{table}\" WHERE execution_id = ?"
                ))
                .bind(execution_id.as_str()),
            )
            .await?;
        out.push((
            table,
            rows.first()
                .map(|r| r.unsigned(0))
                .transpose()?
                .unwrap_or(0),
        ));
    }
    Ok(out)
}
