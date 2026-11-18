//! CF6: the OrgDO's store, one organization's quota counters (PRD §17,
//! D25, D56). A Durable Object runs one request's synchronous work at a
//! time and each call here is one transaction, so a check and its
//! increment can never interleave with another start's: concurrent starts
//! cannot overshoot the limit. Reservations are idempotent per execution,
//! and refunds give back exactly the month they took.
use crate::{
    DoSql,
    migrate::{Migration, migrate_with},
    sql::{Columns, Value, int},
};
use runbook_core::{
    StoreError,
    quotas::{MONTHLY_CODE, QuotaLimits, has_room},
};
use runbook_model::{ExecutionId, OrganizationId};

/// Every OrgDO migration, in order.
pub const ORG_MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "0001_quotas.sql",
    sql: include_str!("../../../cloudflare/migrations/orgdo/0001_quotas.sql"),
}];

/// One organization's quota storage.
#[derive(Clone)]
pub struct OrgStore<B: DoSql> {
    sql: B,
}

impl<B: DoSql> OrgStore<B> {
    /// Open the store, bringing its schema up to date.
    pub fn open(sql: B) -> Result<Self, StoreError> {
        migrate_with(&sql, ORG_MIGRATIONS)?;
        Ok(Self { sql })
    }

    /// Count `execution_id`'s start in `month` unless it is counted
    /// already, refusing it (`InvalidBatch(MONTHLY_CODE)`, nothing written)
    /// when the month is full under `limits`. True when newly counted.
    pub fn reserve(
        &self,
        org: &OrganizationId,
        execution_id: &ExecutionId,
        month: &str,
        now_ms: i64,
        limits: QuotaLimits,
    ) -> Result<bool, StoreError> {
        let (sql, org, id, month) = (
            self.sql.clone(),
            org.as_str().to_owned(),
            execution_id.as_str().to_owned(),
            month.to_owned(),
        );
        self.sql.transaction(move || {
            if !sql
                .query(
                    "SELECT 1 FROM quota_reservation WHERE execution_id = ?",
                    &[id.as_str().into()],
                )?
                .is_empty()
            {
                return Ok(false);
            }
            let used = used(&sql, &org, &month)?;
            if !has_room(&limits, used) {
                return Err(StoreError::InvalidBatch(MONTHLY_CODE));
            }
            sql.exec(
                "INSERT INTO usage_monthly (org_id, month, executions_started) VALUES (?, ?, 1) \
                 ON CONFLICT (org_id, month) DO UPDATE \
                 SET executions_started = executions_started + 1",
                &[org.as_str().into(), month.as_str().into()],
            )?;
            sql.exec(
                "INSERT INTO quota_reservation (execution_id, org_id, month, created_at_ms) \
                 VALUES (?, ?, ?, ?)",
                &[
                    id.as_str().into(),
                    org.as_str().into(),
                    month.as_str().into(),
                    int(now_ms)?,
                ],
            )?;
            Ok(true)
        })
    }

    /// Give back `execution_id`'s count, from the month it was counted in.
    /// False when it was not counted (or already refunded).
    pub fn refund(&self, execution_id: &ExecutionId) -> Result<bool, StoreError> {
        let (sql, id) = (self.sql.clone(), execution_id.as_str().to_owned());
        self.sql.transaction(move || {
            let rows = sql.query(
                "SELECT org_id, month FROM quota_reservation WHERE execution_id = ?",
                &[id.as_str().into()],
            )?;
            let Some(row) = rows.first() else {
                return Ok(false);
            };
            let (org, month) = (row.text(0)?.to_owned(), row.text(1)?.to_owned());
            sql.exec(
                "DELETE FROM quota_reservation WHERE execution_id = ?",
                &[id.as_str().into()],
            )?;
            sql.exec(
                "UPDATE usage_monthly SET executions_started = executions_started - 1 \
                 WHERE org_id = ? AND month = ? AND executions_started > 0",
                &[org.as_str().into(), month.as_str().into()],
            )?;
            Ok(true)
        })
    }

    /// Starts counted in `month`.
    pub fn started(&self, org: &OrganizationId, month: &str) -> Result<u64, StoreError> {
        used(&self.sql, org.as_str(), month)
    }

    /// Up to `limit` reservations made before `before_ms`, oldest first
    /// (the cron refunds those whose start never reached D1).
    pub fn reservations_before(
        &self,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.sql
            .query(
                "SELECT execution_id FROM quota_reservation WHERE created_at_ms < ? \
                 ORDER BY created_at_ms, execution_id LIMIT ?",
                &[
                    int(before_ms)?,
                    Value::Integer(i64::try_from(limit).unwrap_or(1_000).min(1_000)),
                ],
            )?
            .iter()
            .map(|row| {
                row.text(0)?
                    .parse()
                    .map_err(|_| StoreError::Backend("do.invalid_stored_id"))
            })
            .collect()
    }

    /// Forget reservations older than `before_ms` whose runs are counted
    /// for good (the month's count stays): keeps the table small.
    pub fn settle_reservations(&self, ids: &[ExecutionId]) -> Result<(), StoreError> {
        let (sql, ids) = (self.sql.clone(), ids.to_vec());
        self.sql.transaction(move || {
            for id in &ids {
                sql.exec(
                    "DELETE FROM quota_reservation WHERE execution_id = ?",
                    &[id.as_str().into()],
                )?;
            }
            Ok(())
        })
    }
}

fn used<B: DoSql>(sql: &B, org: &str, month: &str) -> Result<u64, StoreError> {
    sql.query(
        "SELECT executions_started FROM usage_monthly WHERE org_id = ? AND month = ?",
        &[org.into(), month.into()],
    )?
    .first()
    .map(|row| row.unsigned(0))
    .transpose()
    .map(|n| n.unwrap_or(0))
}
