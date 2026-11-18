//! CF6: monthly execution quotas (PRD §17, D25). Every adapter counts a
//! run's start in its organization's month, refuses the start that would
//! exceed the limit with nothing written, counts a replayed start once,
//! refunds a run cancelled while queued, and rolls the month over by the
//! store clock.
use crate::{SessionFactory, catalog, execution, scope};
use runbook_core::{
    ExecutionStore, StoreError,
    catalog::{AdmissionState, AdmitRequest, CatalogStore},
    quotas::{MONTHLY_CODE, QuotaLimits, QuotaStore, month_of},
    start::{StartCommit, StartOutcome, StartRecord, StartReplay, StartStore},
};
use runbook_model::ExecutionStatus;
use std::future::Future;

/// The quota tier: a store whose organization quota the test sets.
pub trait QuotaFactory:
    SessionFactory<Store: StartStore + CatalogStore + QuotaStore + ExecutionStore>
{
    /// The same storage with `limits` in force (configuration, not data).
    fn with_quota(
        &self,
        store: &Self::Store,
        limits: QuotaLimits,
    ) -> impl Future<Output = Result<Self::Store, StoreError>>;
    /// The same storage with the clock its quota months follow at least
    /// one month later (a test clock moves; Postgres, which has none, takes
    /// a test-only offset).
    fn next_month(
        &self,
        store: &Self::Store,
    ) -> impl Future<Output = Result<Self::Store, StoreError>>;
}

/// A start of run `n`: in pool `pool` (limit 1) when given, under replay
/// key `key` when given.
pub fn start(n: u8, key: Option<&str>, pool: Option<&str>) -> StartCommit {
    let mut projection = execution::projection();
    projection.execution_id = catalog::id(n);
    let record = StartRecord {
        runbook_id: projection.runbook_id.clone(),
        version_id: projection.version_id.clone(),
        version: std::num::NonZeroU32::new(1),
        semantic_hash: [1; 32],
        snapshot_digest: [2; 32],
        context: Default::default(),
        concurrency_key: pool.map(str::to_owned),
        rerun_of: None,
        request_tags: vec![],
    };
    let replay = key.map(|key| StartReplay {
        key: key.into(),
        principal: "human:usr_1".into(),
        request_hash: [n; 32],
        outcome: StartOutcome {
            execution_id: catalog::id(n),
            runbook_version_id: record.version_id.clone(),
            runbook_version: record.version,
            admission: None,
        },
    });
    StartCommit {
        admission: pool.map(|key| AdmitRequest {
            execution_id: catalog::id(n),
            key: key.into(),
            limit: 1,
            idempotency_key: format!("quota-start-{n}"),
        }),
        on_conflict: None,
        derived: catalog::update(n, 0, ExecutionStatus::Queued),
        projection,
        record,
        replay,
    }
}

fn check(ok: bool, what: &'static str) -> Result<(), StoreError> {
    if ok {
        Ok(())
    } else {
        Err(StoreError::Backend(what))
    }
}

/// Starts count once, the start past the limit is refused with nothing
/// written, a run cancelled while queued is refunded, the running count is
/// the admitted unfinished runs, the next month starts from zero, and
/// another organization's usage is its own.
pub async fn monthly_starts_are_counted_capped_and_roll_over<F: QuotaFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let base = f.create(execution::projection()).await?;
    let limits = QuotaLimits {
        executions_monthly: Some(3),
    };
    let s = f.with_quota(&base, limits).await?;
    let org = scope(1).org_id;
    let usage = s.org_usage(&org).await?;
    let month = month_of(f.backend_time_ms(&s).await?);
    check(usage.month == month, "test.month")?;
    check(usage.executions_started == 0, "test.nothing_counted")?;
    check(usage.limits == limits, "test.limits_reported")?;

    // A start retried under its key counts once.
    let first = s.commit_start(start(11, Some("quota-1"), None)).await?;
    check(
        s.commit_start(start(11, Some("quota-1"), None)).await? == first,
        "test.replayed",
    )?;
    // Run 12 holds pool `p`; run 13 waits behind it.
    s.commit_start(start(12, None, Some("p"))).await?;
    let queued = s.commit_start(start(13, None, Some("p"))).await?;
    check(
        queued.admission.map(|a| a.state) == Some(AdmissionState::Queued),
        "test.queued",
    )?;
    check(
        s.org_usage(&org).await?.executions_started == 3,
        "test.three_counted",
    )?;
    // The fourth start this month is refused and writes nothing.
    check(
        s.commit_start(start(14, None, None)).await == Err(StoreError::InvalidBatch(MONTHLY_CODE)),
        "test.refused",
    )?;
    check(
        s.load(&catalog::id(14)).await == Err(StoreError::NotFound),
        "test.refused_wrote_nothing",
    )?;
    check(
        s.org_usage(&org).await?.executions_started == 3,
        "test.refusal_not_counted",
    )?;
    // A run cancelled while queued never counts: its slot is free again.
    check(
        s.cancel_queued(&catalog::id(13), None).await?,
        "test.cancelled_queued",
    )?;
    check(
        s.org_usage(&org).await?.executions_started == 2,
        "test.refunded",
    )?;
    s.commit_start(start(14, None, None)).await?;
    check(
        s.org_usage(&org).await?.executions_started == 3,
        "test.refund_reused",
    )?;
    // Running: the admitted, unfinished runs (12 holds its pool, 11 and 14
    // are unpooled; none has started yet, so none is running).
    check(s.org_usage(&org).await?.running == 0, "test.none_running")?;
    s.commit(catalog::batch(11, 0, 1, ExecutionStatus::Running))
        .await?;
    check(s.org_usage(&org).await?.running == 1, "test.one_running")?;
    s.commit(catalog::batch(11, 1, 2, ExecutionStatus::Succeeded))
        .await?;
    check(
        s.org_usage(&org).await?.running == 0,
        "test.finished_not_running",
    )?;
    // Another organization's usage is its own.
    let other = f
        .with_quota(&f.scoped_client(&s, scope(2)).await?, limits)
        .await?;
    check(
        other.org_usage(&scope(2).org_id).await?.executions_started == 0,
        "test.other_org",
    )?;
    // The next month starts from zero.
    let s = f.next_month(&s).await?;
    let next = s.org_usage(&org).await?;
    check(next.month != month, "test.month_rolled")?;
    check(next.executions_started == 0, "test.new_month_empty")?;
    s.commit_start(start(15, None, None)).await?;
    check(
        s.org_usage(&org).await?.executions_started == 1,
        "test.new_month_counts",
    )?;
    // With no limit, starts count and are never refused.
    let open = f.with_quota(&s, QuotaLimits::default()).await?;
    for n in 16..20 {
        open.commit_start(start(n, None, None)).await?;
    }
    check(
        open.org_usage(&org).await?.executions_started == 5,
        "test.unlimited_counts",
    )?;
    Ok(())
}
