//! CF3 review fixes: D1 cases beyond the shared suite, run on any host's
//! D1 (rusqlite natively, Miniflare's D1 inside workerd): admission
//! interleavings driven deterministically, the start reservation under one
//! key, and the derived-row applier's ordering and replays.
//!
//! [`Interleave`] runs another writer's operation between a plan's read and
//! its batch: the next batch whose SQL contains a marker first awaits the
//! armed operation (which writes through the inner database), then runs.
use runbook_core::{
    StoreError,
    catalog::{AdmissionState, AdmitRequest, DerivedUpdate},
};
use runbook_model::{ExecutionId, ExecutionStatus};
use runbook_store_conformance::{catalog, scope};
use runbook_store_d1::{
    D1, D1Catalog, D1Error, Statement,
    sql::{Outcome, Row},
};
use std::{cell::RefCell, future::Future, pin::Pin, rc::Rc};

type Armed = Option<(&'static str, Pin<Box<dyn Future<Output = ()>>>)>;

/// A D1 that can run one interloper before a chosen batch.
#[derive(Clone)]
pub struct Interleave<D: D1> {
    pub inner: D,
    armed: Rc<RefCell<Armed>>,
}
impl<D: D1> Interleave<D> {
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            armed: Rc::default(),
        }
    }
    /// Before the next batch whose SQL contains `marker`, await `op`.
    pub fn before_batch(&self, marker: &'static str, op: impl Future<Output = ()> + 'static) {
        *self.armed.borrow_mut() = Some((marker, Box::pin(op)));
    }
    pub fn fired(&self) -> bool {
        self.armed.borrow().is_none()
    }
}
impl<D: D1> D1 for Interleave<D> {
    async fn all(&self, statement: Statement) -> Result<Vec<Row>, D1Error> {
        self.inner.all(statement).await
    }
    async fn run(&self, statement: Statement) -> Result<u64, D1Error> {
        self.inner.run(statement).await
    }
    async fn batch(&self, statements: Vec<Statement>) -> Result<Vec<Outcome>, D1Error> {
        let due = self
            .armed
            .borrow()
            .as_ref()
            .is_some_and(|(marker, _)| statements.iter().any(|s| s.sql.contains(marker)));
        if due {
            let taken = self.armed.borrow_mut().take();
            if let Some((_, op)) = taken {
                op.await;
            }
        }
        self.inner.batch(statements).await
    }
    fn now_ms(&self) -> i64 {
        self.inner.now_ms()
    }
}

fn request(n: u8, key: &str, limit: u32) -> AdmitRequest {
    AdmitRequest {
        execution_id: catalog::id(n),
        key: key.into(),
        limit,
        idempotency_key: format!("start-{n}"),
    }
}
fn check(ok: bool, what: &'static str) -> Result<(), StoreError> {
    if ok {
        Ok(())
    } else {
        Err(StoreError::Backend(what))
    }
}
async fn state<D: D1>(c: &D1Catalog<D>, n: u8) -> Result<AdmissionState, StoreError> {
    Ok(c.admission(&catalog::id(n))
        .await?
        .ok_or(StoreError::NotFound)?
        .state)
}
/// Every slot the pool counts is held by an admitted run.
async fn consistent<D: D1>(db: &D, key: &str) -> Result<(), StoreError> {
    use runbook_store_d1::sql::{Columns, prepare};
    let rows = db
        .all(
            prepare(
                "SELECT p.active, p.concurrency_limit, \
                 (SELECT count(*) FROM admission a WHERE a.key = p.key AND a.state = 'admitted'), \
                 (SELECT count(*) FROM admission a WHERE a.key = p.key AND a.state = 'queued') \
                 FROM admission_pool p WHERE p.key = ?",
            )
            .bind(key),
        )
        .await
        .map_err(|_| StoreError::Backend("test.read"))?;
    let row = rows.first().ok_or(StoreError::NotFound)?;
    let n = |i| {
        row.integer(i)
            .map_err(|_| StoreError::Backend("test.decode"))
    };
    let (active, limit, admitted, queued) = (n(0)?, n(1)?, n(2)?, n(3)?);
    check(active == admitted, "test.active_count_drifted")?;
    check(!(queued > 0 && active < limit), "test.waiter_stranded")
}

/// Review fix 3: a release and a queued start read the same `(limit,
/// active)`; the start's batch rewrites the same values. The release must
/// still see the new waiter (the pool's revision moved) and promote it.
pub async fn a_release_never_strands_a_waiter_queued_meanwhile<D: D1>(
    db: D,
) -> Result<(), StoreError> {
    let db = Interleave::new(db);
    let c = D1Catalog::new(db.clone(), scope(1));
    let inner = D1Catalog::new(db.inner.clone(), scope(1));
    check(
        c.admit(request(1, "pool", 1)).await?.state == AdmissionState::Admitted,
        "test.first_admitted",
    )?;
    db.before_batch("SET state = 'released'", async move {
        let queued = inner.admit(request(2, "pool", 1)).await;
        assert_eq!(queued.map(|a| a.state), Ok(AdmissionState::Queued));
    });
    c.release(&catalog::id(1)).await?;
    check(db.fired(), "test.interloper_never_ran")?;
    check(
        state(&c, 2).await? == AdmissionState::Admitted,
        "test.waiter_not_promoted",
    )?;
    consistent(&db.inner, "pool").await
}

/// The same race the other way: a start reads the pool, a release lands
/// before its batch; the start must not count a slot the release freed.
pub async fn an_admission_rechecks_a_pool_released_meanwhile<D: D1>(
    db: D,
) -> Result<(), StoreError> {
    let db = Interleave::new(db);
    let c = D1Catalog::new(db.clone(), scope(1));
    let inner = D1Catalog::new(db.inner.clone(), scope(1));
    c.admit(request(1, "pool", 1)).await?;
    db.before_batch("INSERT INTO admission (", async move {
        assert!(inner.release(&catalog::id(1)).await.is_ok());
    });
    let second = c.admit(request(2, "pool", 1)).await?;
    check(db.fired(), "test.interloper_never_ran")?;
    check(
        second.state == AdmissionState::Admitted,
        "test.second_not_admitted",
    )?;
    consistent(&db.inner, "pool").await
}

/// Review fix 1 at the D1 half: two starts under one replay key reserve
/// one run; the second is told the first's run, never its own.
pub async fn one_replay_key_reserves_one_run<D: D1>(db: D) -> Result<(), StoreError> {
    use runbook_core::start::StartReplay;
    let c = D1Catalog::new(db.clone(), scope(1));
    let commit = |n: u8| {
        let mut commit = crate::cases::start_commit(n);
        commit.replay = Some(StartReplay {
            key: "same-key".into(),
            principal: "human:usr_1".into(),
            request_hash: [7; 32],
            outcome: runbook_core::start::StartOutcome {
                execution_id: catalog::id(n),
                runbook_version_id: commit.record.version_id.clone(),
                runbook_version: commit.record.version,
                admission: None,
            },
        });
        commit
    };
    let first = c.reserve_start(&commit(1)).await?;
    let second = c.reserve_start(&commit(2)).await?;
    check(!first.replayed && second.replayed, "test.replay_flags")?;
    check(
        second.outcome.execution_id == catalog::id(1),
        "test.second_named_another_run",
    )?;
    check(
        runbook_store_d1::catalog::execution_owner(&db, &catalog::id(2))
            .await?
            .is_none(),
        "test.second_owner_written",
    )
}

/// The applier on this host's D1: rows apply in order, replays and older
/// rows write nothing, a release promotes the waiter.
pub async fn the_applier_is_ordered_idempotent_and_releases<D: D1>(
    db: D,
) -> Result<(), StoreError> {
    let c = D1Catalog::new(db.clone(), scope(1));
    c.admit(request(1, "api", 1)).await?;
    c.admit(request(2, "api", 1)).await?;
    let row = |seq: u64, status| -> DerivedUpdate { catalog::update(1, seq, status) };
    for seq in [1, 2] {
        c.apply_derived(&row(seq, ExecutionStatus::Running), &[])
            .await?;
    }
    let older = c
        .apply_derived(&row(1, ExecutionStatus::Running), &[])
        .await?;
    check(!older.summary, "test.older_row_applied")?;
    check(
        c.get_run(&catalog::id(1)).await?.map(|r| r.sequence) == Some(2),
        "test.sequence",
    )?;
    let done = c
        .apply_derived(&row(3, ExecutionStatus::Succeeded), &[])
        .await?;
    check(
        done.promoted
            .iter()
            .map(|p| &p.execution_id)
            .collect::<Vec<&ExecutionId>>()
            == vec![&catalog::id(2)],
        "test.release_promoted",
    )?;
    let replay = c
        .apply_derived(&row(3, ExecutionStatus::Succeeded), &[])
        .await?;
    check(
        !replay.summary && replay.promoted.is_empty(),
        "test.replay_wrote",
    )?;
    check(
        c.list_assignments().await?.is_empty(),
        "test.finished_run_in_inbox",
    )?;
    consistent(&db, "api").await
}
