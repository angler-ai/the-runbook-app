//! CF3: the composed Cloudflare store (an ExecutionDO store per run plus
//! D1) natively, over rusqlite for both: the catalog-tier conformance cases
//! (catalog, search, inbox, memory proposals) and the execution-scoped ones,
//! one test each, with the derived-row applier as the only way rows reach
//! D1. On workerd the cross-object tiers are CF5.
use runbook_core::{ExecutionProjection, StoreError, WriteStage, catalog::TenantScope};
use runbook_model::ExecutionId;
use runbook_store_conformance::{
    CatalogFactory, SessionFactory, StoreFactory, WorkQueueFactory, scope,
};
use runbook_store_d1::native::NativeD1;
use runbook_store_do::{DoStore, OUTBOX_FIRST_RETRY_MS};
use runbook_worker::compose::{CfStore, Objects};
use runbook_worker_conformance::native::{NativeSql, run};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    time::Duration,
};

const START_MS: i64 = 10_000;

/// Every run's object, by ID, on one shared test clock.
#[derive(Clone, Default)]
struct NativeObjects {
    map: Rc<RefCell<BTreeMap<ExecutionId, NativeSql>>>,
    /// CF11: the webhook subscriptions' objects.
    webhooks: runbook_worker::webhooks::LocalWebhooks<NativeSql>,
    now: Rc<Cell<i64>>,
    fault: Rc<Cell<Option<WriteStage>>>,
}
impl Objects for NativeObjects {
    type Handle = DoStore<NativeSql>;
    type Quota = runbook_worker::quota::NoQuotas;
    fn quota(&self, _org: &runbook_model::OrganizationId) -> Self::Quota {
        runbook_worker::quota::NoQuotas
    }
    type Webhooks = runbook_worker::webhooks::LocalWebhooks<NativeSql>;
    fn webhooks(&self) -> Self::Webhooks {
        self.webhooks.clone()
    }
    fn object(
        &self,
        id: &ExecutionId,
        scope: &TenantScope,
    ) -> Result<DoStore<NativeSql>, StoreError> {
        let sql = self
            .map
            .borrow_mut()
            .entry(id.clone())
            .or_insert_with(|| NativeSql::new(self.now.get()))
            .clone();
        sql.set_time(self.now.get());
        DoStore::open(sql, id, scope.clone())
    }
    fn reachable(&self) -> Vec<ExecutionId> {
        self.map.borrow().keys().cloned().collect()
    }
    fn before_commit(&self, store: &DoStore<NativeSql>) {
        if let Some(stage) = self.fault.take() {
            store.fail_after(stage);
        }
    }
}

type Store = CfStore<NativeD1, NativeObjects, runbook_store_r2::MemoryBucket>;

/// The composed store's factory. Each `create` is a fresh D1 and object map.
struct Composed {
    d1: RefCell<Option<NativeD1>>,
    objects: RefCell<NativeObjects>,
}
impl Composed {
    fn new() -> Self {
        Self {
            d1: RefCell::new(None),
            objects: RefCell::new(NativeObjects::default()),
        }
    }
}

impl StoreFactory for Composed {
    type Store = Store;
    async fn create(&self, initial: ExecutionProjection) -> Result<Store, StoreError> {
        let d1 = NativeD1::new(START_MS);
        runbook_store_d1::migrate::migrate(&d1)
            .await
            .map_err(|_| StoreError::Backend("test.migrate"))?;
        let objects = NativeObjects::default();
        objects.now.set(START_MS);
        *self.d1.borrow_mut() = Some(d1.clone());
        *self.objects.borrow_mut() = objects.clone();
        let store = CfStore::new(
            d1,
            objects.clone(),
            scope(1),
            Default::default(),
            Default::default(),
        );
        store.catalog().claim_owner(&initial.execution_id).await?;
        objects
            .object(&initial.execution_id, &scope(1))?
            .create_execution(initial)?;
        Ok(store)
    }
    async fn fail_after(&self, store: &Store, stage: WriteStage) -> Result<(), StoreError> {
        store.objects().fault.set(Some(stage));
        Ok(())
    }
    /// D56's bound: the host applies a commit's rows before its request
    /// returns, and the alarm retries a failed apply from this delay.
    fn derived_row_delay(&self) -> Duration {
        Duration::from_millis(u64::try_from(OUTBOX_FIRST_RETRY_MS).unwrap_or(10_000))
    }
}
impl SessionFactory for Composed {
    async fn backend_time_ms(&self, store: &Store) -> Result<i64, StoreError> {
        Ok(store.objects().now.get())
    }
    async fn scoped_client(&self, store: &Store, tenant: TenantScope) -> Result<Store, StoreError> {
        Ok(store.scoped(tenant))
    }
    async fn advance_clock(&self, store: &Store, ms: u64) -> Result<(), StoreError> {
        let ms = i64::try_from(ms).map_err(|_| StoreError::Backend("test.clock"))?;
        let now = store.objects().now.get() + ms;
        store.objects().now.set(now);
        if let Some(d1) = self.d1.borrow().as_ref() {
            d1.set_time(now);
        }
        Ok(())
    }
}
impl CatalogFactory for Composed {
    async fn redeliver_derived(
        &self,
        store: &Store,
        id: &ExecutionId,
        sequence: u64,
    ) -> Result<(), StoreError> {
        store.redeliver_derived(id, sequence).await
    }
    /// One drain of the run's object (as the host does after a commit),
    /// then the admission notices; the row must be in D1 afterwards. No
    /// polling: the bound is checked, never waited out.
    async fn await_derived(
        &self,
        store: &Store,
        id: &ExecutionId,
        sequence: u64,
        max_delay: Duration,
    ) -> Result<(), StoreError> {
        assert!(max_delay <= self.derived_row_delay());
        let drained = store.drain_derived(id).await?;
        store.deliver_notices(&drained.promoted).await?;
        let notices = store.catalog().pending_notices(100).await?;
        store.deliver_notices(&notices).await?;
        match runbook_core::catalog::CatalogStore::get_run(store, id).await? {
            Some(run) if run.sequence >= sequence => Ok(()),
            _ => Err(StoreError::Backend("test.derived_row_not_applied")),
        }
    }
}
impl WorkQueueFactory for Composed {
    async fn expire_lease(
        &self,
        store: &Store,
        lease: &runbook_core::work_queue::WorkLease,
    ) -> Result<(), StoreError> {
        store
            .objects()
            .object(&lease.id.execution_id, &lease.scope)?
            .expire_lease(lease)
    }
}

macro_rules! composed {
    ($($test:ident => $module:ident :: $case:ident),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                run(runbook_store_conformance::$module::$case(&Composed::new())).unwrap();
            }
        )*
    };
}
composed!(
    catalog_project_queries_never_return_foreign_rows => catalog::project_queries_never_return_foreign_rows,
    catalog_revision_precondition_rejects_stale_replace => catalog::revision_precondition_rejects_stale_replace,
    catalog_derived_rows_ignore_old_or_duplicate_sequence => catalog::derived_rows_ignore_old_or_duplicate_sequence,
    catalog_concurrency_limit_one_admits_and_queues => catalog::concurrency_limit_one_admits_and_queues,
    catalog_derived_rows_rollback_with_execution => catalog::derived_rows_rollback_with_execution,
    catalog_derived_rows_cannot_release_running_execution => catalog::derived_rows_cannot_release_running_execution,
    catalog_pool_limit_replacement_promotes_waiters => catalog::pool_limit_replacement_promotes_waiters,
    search_indexed_severity_tracks_commits => search::indexed_severity_tracks_commits,
    search_search_is_tenant_scoped_and_paginated => search::search_is_tenant_scoped_and_paginated,
    search_numeric_range_ignores_non_number_values => search::numeric_range_ignores_non_number_values,
    search_inbox_tracks_current_access_and_completion => search::inbox_tracks_current_access_and_completion,
    search_runbook_run_stats_group_by_runbook => search::runbook_run_stats_group_by_runbook,
    memory_entries_proposals_commit_with_the_execution => memory_entries::proposals_commit_with_the_execution,
    memory_entries_proposals_racing_a_full_quota_are_never_lost => memory_entries::proposals_racing_a_full_quota_are_never_lost,
    memory_entries_runs_save_live_entries => memory_entries::runs_save_live_entries,
    memory_entries_agent_writes_commit_a_notice => memory_entries::agent_writes_commit_a_notice,
    execution_atomic_batch_or_nothing => execution::atomic_batch_or_nothing,
    execution_stale_commit_has_no_side_effects => execution::stale_commit_has_no_side_effects,
    execution_one_claim_generation_winner => execution::one_claim_generation_winner,
    execution_idempotent_response_is_payload_bound => execution::idempotent_response_is_payload_bound,
    execution_replay_equals_projection => execution::replay_equals_projection,
    clock_store_clock_controls_deadline_equality => clock::store_clock_controls_deadline_equality,
    clock_session_time_and_snapshot_are_fenced => clock::session_time_and_snapshot_are_fenced,
    clock_session_commits_at_its_begin_time => clock::session_commits_at_its_begin_time,
    queue_due_item_has_one_current_worker => queue::due_item_has_one_current_worker,
    queue_stale_worker_cannot_ack_successor => queue::stale_worker_cannot_ack_successor,
    queue_delivery_leases_are_scoped_and_fenced => queue::delivery_leases_are_scoped_and_fenced,
    queue_timer_replacements_require_new_generation => queue::timer_replacements_require_new_generation,
    // CF5: evidence slots on D1.
    uploads_slots_count_limits_and_finalize_once => uploads::slots_count_limits_and_finalize_once,
    uploads_stale_slots_are_listed_and_deleted => uploads::stale_slots_are_listed_and_deleted,
    uploads_quotas_count_live_slots => uploads::quotas_count_live_slots,
    uploads_storage_counts_stored_bytes => uploads::storage_counts_stored_bytes,
);

// ---- The derived-row applier (CF3, D56) ----

use runbook_core::{
    ExecutionStore,
    catalog::{CatalogStore, DerivedUpdate},
};
use runbook_model::ExecutionStatus;
use runbook_store_conformance::{catalog, execution};
use runbook_store_d1::{D1, D1Error, Statement, sql::Outcome};
use runbook_store_do::{OUTBOX_MAX_RETRY_MS, OutboxStream};

/// A D1 that fails every call while `down` is set (D1 unavailable).
#[derive(Clone)]
struct FlakyD1 {
    inner: NativeD1,
    down: Rc<Cell<bool>>,
}
impl D1 for FlakyD1 {
    async fn all(&self, s: Statement) -> Result<Vec<runbook_store_d1::sql::Row>, D1Error> {
        if self.down.get() {
            return Err(D1Error::Other("test.d1_down".into()));
        }
        self.inner.all(s).await
    }
    async fn run(&self, s: Statement) -> Result<u64, D1Error> {
        if self.down.get() {
            return Err(D1Error::Other("test.d1_down".into()));
        }
        self.inner.run(s).await
    }
    async fn batch(&self, s: Vec<Statement>) -> Result<Vec<Outcome>, D1Error> {
        if self.down.get() {
            return Err(D1Error::Other("test.d1_down".into()));
        }
        self.inner.batch(s).await
    }
    fn now_ms(&self) -> i64 {
        self.inner.now_ms()
    }
}

fn fresh(objects: &NativeObjects) -> (NativeD1, Store) {
    let d1 = NativeD1::new(START_MS);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    objects.now.set(START_MS);
    (
        d1.clone(),
        CfStore::new(
            d1,
            objects.clone(),
            scope(1),
            Default::default(),
            Default::default(),
        ),
    )
}

fn summary(store: &Store, n: u8) -> Option<runbook_core::catalog::RunSummary> {
    run(store.get_run(&catalog::id(n))).unwrap()
}

#[test]
fn applier_replays_are_idempotent_and_never_go_backwards() {
    let objects = NativeObjects::default();
    let (_, store) = fresh(&objects);
    let mut initial = execution::projection();
    initial.execution_id = catalog::id(1);
    objects
        .object(&catalog::id(1), &scope(1))
        .unwrap()
        .create_execution(initial)
        .unwrap();
    for (from, to) in [(0, 1), (1, 2), (2, 3)] {
        run(store.commit(catalog::batch(1, from, to, ExecutionStatus::Running))).unwrap();
    }
    let drained = run(store.drain_derived(&catalog::id(1))).unwrap();
    assert_eq!((drained.applied, drained.through), (3, Some(3)));
    assert_eq!(summary(&store, 1).unwrap().sequence, 3);
    // Redelivering older and current rows writes nothing new.
    for sequence in [1, 3, 2] {
        run(store.redeliver_derived(&catalog::id(1), sequence)).unwrap();
    }
    let stored = summary(&store, 1).unwrap();
    assert_eq!(
        stored,
        catalog::update(1, 3, ExecutionStatus::Running).summary
    );
    let inbox = run(store.list_assignments()).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].sequence, 3);
    // An older row applied directly (out of order) is ignored too.
    let old: DerivedUpdate = catalog::update(1, 1, ExecutionStatus::Running);
    let applied = run(store.catalog().apply_derived(&old, &[])).unwrap();
    assert!(!applied.summary);
    assert_eq!(summary(&store, 1).unwrap().sequence, 3);
    // Draining again finds nothing pending.
    assert_eq!(
        run(store.drain_derived(&catalog::id(1))).unwrap().applied,
        0
    );
}

#[test]
fn applier_applies_in_sequence_order_and_keeps_the_start_time() {
    let objects = NativeObjects::default();
    let (_, store) = fresh(&objects);
    let mut initial = execution::projection();
    initial.execution_id = catalog::id(2);
    objects
        .object(&catalog::id(2), &scope(1))
        .unwrap()
        .create_execution(initial)
        .unwrap();
    run(store.commit(catalog::batch(2, 0, 1, ExecutionStatus::Running))).unwrap();
    run(store.drain_derived(&catalog::id(2))).unwrap();
    let first = summary(&store, 2).unwrap();
    // A later summary with another creation time keeps the stored one.
    let mut later = catalog::batch(2, 1, 2, ExecutionStatus::Succeeded);
    later.derived_outbox[0].summary.created_at_ms = 99_999;
    run(store.commit(later)).unwrap();
    run(store.drain_derived(&catalog::id(2))).unwrap();
    let stored = summary(&store, 2).unwrap();
    assert_eq!(stored.sequence, 2);
    assert_eq!(stored.status, ExecutionStatus::Succeeded);
    assert_eq!(stored.created_at_ms, first.created_at_ms);
    assert!(run(store.list_assignments()).unwrap().is_empty());
}

#[test]
fn a_failed_apply_defers_and_the_alarm_bound_drains_it() {
    let objects = NativeObjects::default();
    let d1 = NativeD1::new(START_MS);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    objects.now.set(START_MS);
    let flaky = FlakyD1 {
        inner: d1,
        down: Rc::default(),
    };
    let store: CfStore<FlakyD1, NativeObjects, runbook_store_r2::MemoryBucket> = CfStore::new(
        flaky.clone(),
        objects.clone(),
        scope(1),
        Default::default(),
        Default::default(),
    );
    let mut initial = execution::projection();
    initial.execution_id = catalog::id(3);
    let object = objects.object(&catalog::id(3), &scope(1)).unwrap();
    object.create_execution(initial).unwrap();
    run(store.commit(catalog::batch(3, 0, 1, ExecutionStatus::Running))).unwrap();
    flaky.down.set(true);
    assert!(run(store.drain_derived(&catalog::id(3))).is_err());
    // Nothing was acknowledged; the stream is backed off within the bound.
    assert_eq!(object.pending_derived(None, 10).unwrap().len(), 1);
    let retry = object
        .outbox_retry_at(OutboxStream::Derived)
        .unwrap()
        .unwrap();
    assert!(retry > START_MS && retry <= START_MS + OUTBOX_FIRST_RETRY_MS);
    // Still down at the retry: the delay doubles, never past the maximum.
    objects.now.set(retry);
    assert!(run(store.drain_derived(&catalog::id(3))).is_err());
    let next = object
        .outbox_retry_at(OutboxStream::Derived)
        .unwrap()
        .unwrap();
    assert!(next > retry && next - retry <= OUTBOX_MAX_RETRY_MS);
    // D1 is back: the next drain applies the row and clears the backoff.
    flaky.down.set(false);
    objects.now.set(next);
    let drained = run(store.drain_derived(&catalog::id(3))).unwrap();
    assert_eq!(drained.through, Some(1));
    assert!(object.pending_derived(None, 10).unwrap().is_empty());
    assert_eq!(
        run(store.get_run(&catalog::id(3)))
            .unwrap()
            .unwrap()
            .sequence,
        1
    );
}

// ---- CF3 review fixes 1 and 2: starts under one replay key ----

fn keyed(n: u8) -> runbook_core::start::StartCommit {
    let mut commit = runbook_worker_conformance::cases::start_commit(n);
    commit.replay = Some(runbook_core::start::StartReplay {
        key: "one-key".into(),
        principal: "human:usr_1".into(),
        request_hash: [9; 32],
        outcome: runbook_core::start::StartOutcome {
            execution_id: catalog::id(n),
            runbook_version_id: commit.record.version_id.clone(),
            runbook_version: commit.record.version,
            admission: None,
        },
    });
    commit
}

#[test]
fn a_second_start_under_one_key_never_initializes_its_own_object() {
    use runbook_core::start::StartStore;
    let objects = NativeObjects::default();
    let (_, store) = fresh(&objects);
    let first = run(store.commit_start(keyed(1))).unwrap();
    assert_eq!(first.execution_id, catalog::id(1));
    // A concurrent request missed the replay and was sent to a fresh object.
    assert_eq!(
        run(store.commit_start(keyed(2))),
        Err(StoreError::InvalidBatch("start.replayed_elsewhere"))
    );
    assert_eq!(
        run(store.load(&catalog::id(2))).map(|_| ()),
        Err(StoreError::NotFound),
        "the second object stays empty"
    );
    // The saved run's own object answers the retry with its outcome.
    assert_eq!(run(store.commit_start(keyed(1))).unwrap(), first);
}

#[test]
fn a_reservation_whose_object_was_lost_finishes_on_retry() {
    use runbook_core::start::StartStore;
    let objects = NativeObjects::default();
    let (_, store) = fresh(&objects);
    // D1 committed; the object write never happened.
    run(store.catalog().reserve_start(&keyed(1))).unwrap();
    assert_eq!(run(store.start_record(&catalog::id(1))).unwrap(), None);
    let outcome = run(store.commit_start(keyed(1))).unwrap();
    assert_eq!(outcome.execution_id, catalog::id(1));
    assert!(run(store.start_record(&catalog::id(1))).unwrap().is_some());
    assert!(run(store.get_run(&catalog::id(1))).unwrap().is_some());
}

#[test]
fn the_reconciler_finishes_or_abandons_half_committed_starts() {
    use runbook_core::catalog::{AdmissionState, AdmitRequest};
    use runbook_core::start::StartStore;
    let objects = NativeObjects::default();
    let (d1, store) = fresh(&objects);
    // Run 1 holds the only slot but its object never initialized; run 2 waits.
    let mut held = keyed(1);
    held.admission = Some(AdmitRequest {
        execution_id: catalog::id(1),
        key: "deploy".into(),
        limit: 1,
        idempotency_key: "k1".into(),
    });
    run(store.catalog().reserve_start(&held)).unwrap();
    let mut waiting = runbook_worker_conformance::cases::start_commit(2);
    waiting.admission = Some(AdmitRequest {
        execution_id: catalog::id(2),
        key: "deploy".into(),
        limit: 1,
        idempotency_key: "k2".into(),
    });
    assert_eq!(
        run(store.commit_start(waiting))
            .unwrap()
            .admission
            .map(|a| a.state),
        Some(AdmissionState::Queued)
    );
    // Run 3 initialized, but its derived rows never reached D1.
    let mut lagging = runbook_worker_conformance::cases::start_commit(3);
    lagging.replay = keyed(3).replay.map(|mut r| {
        r.key = "other-key".into();
        r
    });
    run(store.catalog().reserve_start(&lagging)).unwrap();
    objects
        .object(&catalog::id(3), &scope(1))
        .unwrap()
        .initialize_start(runbook_store_do::StartInit {
            projection: lagging.projection.clone(),
            record: lagging.record.clone(),
            admission: None,
            derived: lagging.derived.clone(),
        })
        .unwrap();
    // Within the grace period nothing is touched.
    let early = run(runbook_worker::reconcile::pass(&store, START_MS, 60_000)).unwrap();
    assert_eq!(early, Default::default());
    d1.set_time(START_MS + 120_000);
    let late = run(runbook_worker::reconcile::pass(
        &store,
        START_MS + 120_000,
        60_000,
    ))
    .unwrap();
    assert_eq!(late.finished, vec![catalog::id(3)]);
    assert_eq!(late.abandoned, vec![catalog::id(1)]);
    assert!(run(store.get_run(&catalog::id(3))).unwrap().is_some());
    // The abandoned run released its slot, its key is free again, and it
    // reads as deleted (410), not as a run that never answers.
    assert_eq!(
        run(store.admission(&catalog::id(2)))
            .unwrap()
            .map(|a| a.state),
        Some(AdmissionState::Admitted)
    );
    assert_eq!(run(store.find_replay("one-key")).unwrap(), None);
    assert!(
        run(store.tombstone_project(&scope(1).org_id, &catalog::id(1)))
            .unwrap()
            .is_some()
    );
    // A second pass has nothing left to do.
    let again = run(runbook_worker::reconcile::pass(
        &store,
        START_MS + 120_000,
        60_000,
    ))
    .unwrap();
    assert_eq!(again, Default::default());
}

/// Gate fix D1: a promotion that reaches a half-committed start's empty
/// object is kept (the run's owner row exists, no tombstone), and the
/// retried start, replayed from its saved `Queued` outcome, comes up
/// admitted because D1 already promoted it. Before the fix the notice was
/// dropped and the run waited forever with the pool wedged.
#[test]
fn a_promotion_that_reaches_an_empty_object_is_kept_and_the_retried_start_runs() {
    use runbook_core::catalog::{AdmissionState, AdmitRequest};
    use runbook_core::start::StartStore;
    let pool = |n: u8| AdmitRequest {
        execution_id: catalog::id(n),
        key: "deploy".into(),
        limit: 1,
        idempotency_key: format!("k{n}"),
    };
    let objects = NativeObjects::default();
    let (_, store) = fresh(&objects);
    // Run 1 holds the only slot.
    let mut holder = runbook_worker_conformance::cases::start_commit(1);
    holder.admission = Some(pool(1));
    run(store.commit_start(holder)).unwrap();
    // Run 2 half-commits: D1 queues it; its object never initializes.
    let mut waiting = keyed(2);
    waiting.admission = Some(pool(2));
    let reserved = run(store.catalog().reserve_start(&waiting)).unwrap();
    assert_eq!(
        reserved.outcome.admission.as_ref().map(|a| a.state),
        Some(AdmissionState::Queued)
    );
    // Run 1 leaves; D1 promotes run 2; its notice meets an empty object.
    let promoted = run(store.catalog().release(&catalog::id(1))).unwrap();
    assert_eq!(promoted.len(), 1);
    run(store.deliver_notices(&promoted)).unwrap();
    assert_eq!(
        run(store.catalog().pending_notices(10)).unwrap(),
        promoted,
        "the notice stays while the run may still initialize"
    );
    // The retry initializes from the saved Queued outcome, then follows D1.
    let outcome = run(store.commit_start(waiting)).unwrap();
    assert_eq!(
        outcome.admission.map(|a| a.state),
        Some(AdmissionState::Admitted)
    );
    let object = objects.object(&catalog::id(2), &scope(1)).unwrap();
    let at = promoted[0].admitted_at_ms;
    assert!(
        matches!(
            object.admit(&catalog::id(2), at + 1),
            Err(StoreError::InvalidBatch("start.already_admitted"))
        ),
        "the object is admitted"
    );
    // The kept notice then lands and is forgotten.
    run(store.deliver_notices(&promoted)).unwrap();
    assert!(run(store.catalog().pending_notices(10)).unwrap().is_empty());
}

/// Gate fix D1: a notice for a run with no owner row (abandoned, or never
/// started) or a tombstone is dropped, as before.
#[test]
fn a_notice_for_a_run_that_will_never_initialize_is_dropped() {
    let objects = NativeObjects::default();
    let (d1, store) = fresh(&objects);
    let notice = runbook_store_d1::admission::Promotion {
        execution_id: catalog::id(7),
        admitted_at_ms: START_MS,
    };
    run(runbook_store_d1::D1::run(
        &d1,
        runbook_store_d1::prepare(
            "INSERT INTO admission_notice (execution_id, org_id, project_id, admitted_at_ms) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(notice.execution_id.as_str())
        .bind(scope(1).org_id.as_str())
        .bind(scope(1).project_id.as_str())
        .bind(notice.admitted_at_ms),
    ))
    .unwrap();
    run(store.deliver_notices(std::slice::from_ref(&notice))).unwrap();
    assert!(run(store.catalog().pending_notices(10)).unwrap().is_empty());
}

/// Gate fix D2: a run a portable import installs held arms no alarm and
/// hands out no derived rows or notifications, so nothing settles, drains
/// into D1 or publishes before the import's D1 commit; releasing it
/// resumes all of that.
#[test]
fn an_installed_held_run_does_nothing_until_released() {
    use runbook_core::ExecutionStore;
    use runbook_core::start::StartStore;
    let source = NativeObjects::default();
    let (_, store) = fresh(&source);
    // A started run whose start row has not reached D1 yet.
    let commit = runbook_worker_conformance::cases::start_commit(4);
    run(store.catalog().reserve_start(&commit)).unwrap();
    let object = source.object(&catalog::id(4), &scope(1)).unwrap();
    object
        .initialize_start(runbook_store_do::StartInit {
            projection: commit.projection.clone(),
            record: commit.record.clone(),
            admission: None,
            derived: commit.derived.clone(),
        })
        .unwrap();
    let stored = run(store.load(&catalog::id(4))).unwrap();
    assert!(
        !stored.derived_outbox.is_empty(),
        "the start row is pending"
    );
    let start = run(store.start_record(&catalog::id(4))).unwrap();

    // Installed held into another instance's object.
    let target = NativeObjects::default();
    let held = target.object(&catalog::id(4), &scope(1)).unwrap();
    held.install(runbook_store_do::portable::RunInstall {
        run: stored,
        actor_replays: vec![],
        work: vec![],
        start,
        claim_logs: vec![],
        activation_at_ms: Some(START_MS),
        held: true,
        import_id: "imp_test".into(),
        replace_held: false,
    })
    .unwrap();
    assert!(held.held().unwrap());
    // Only the hold's expiry wakes it (gate round 2), not the run's work.
    assert_eq!(
        runbook_store_do::next_alarm_at(&held).unwrap(),
        held.hold()
            .unwrap()
            .map(|(_, at)| at + runbook_store_do::portable::HOLD_EXPIRY_MS)
    );
    assert!(held.pending_derived(None, 10).unwrap().is_empty());
    assert!(held.pending_notifications(None, 10).unwrap().is_empty());

    assert!(held.release(&catalog::id(4)).unwrap());
    assert!(!held.held().unwrap());
    assert!(!held.release(&catalog::id(4)).unwrap(), "idempotent");
    assert!(runbook_store_do::next_alarm_at(&held).unwrap().is_some());
    assert!(!held.pending_derived(None, 10).unwrap().is_empty());
}

type Hook = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// A D1 that runs `hook` once, just before its `nth` batch: another host's
/// write landing between a read and the write it decided.
#[derive(Clone)]
struct RacingD1 {
    inner: NativeD1,
    batches: Rc<Cell<usize>>,
    nth: usize,
    hook: Hook,
}
impl D1 for RacingD1 {
    async fn all(&self, s: Statement) -> Result<Vec<runbook_store_d1::sql::Row>, D1Error> {
        self.inner.all(s).await
    }
    async fn run(&self, s: Statement) -> Result<u64, D1Error> {
        self.inner.run(s).await
    }
    async fn batch(&self, s: Vec<Statement>) -> Result<Vec<Outcome>, D1Error> {
        self.batches.set(self.batches.get() + 1);
        if self.batches.get() == self.nth
            && let Some(hook) = self.hook.borrow_mut().take()
        {
            hook();
        }
        self.inner.batch(s).await
    }
    fn now_ms(&self) -> i64 {
        self.inner.now_ms()
    }
}

/// Gate fix D4: a remote drain (`deliver_supersede`) and the run's own drain
/// can both read the same summary sequence; the one with the older row
/// committing last must not overwrite the newer summary or inbox.
#[test]
fn an_older_derived_row_never_overwrites_a_newer_one_that_raced_it() {
    let d1 = NativeD1::new(START_MS);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let plain = runbook_store_d1::D1Catalog::new(d1.clone(), scope(1));
    let base = catalog::update(5, 1, runbook_model::ExecutionStatus::Running);
    assert!(!base.assignments.is_empty(), "the fixture has an inbox row");
    run(plain.claim_owner(&base.summary.execution_id)).unwrap();
    run(plain.apply_derived(&base, &[])).unwrap();
    let mut older = base.clone();
    older.summary.sequence = base.summary.sequence + 6;
    older.summary.title = "older".into();
    let mut newer = base.clone();
    newer.summary.sequence = base.summary.sequence + 7;
    newer.summary.title = "newer".into();
    newer.assignments.clear();
    let racing = RacingD1 {
        inner: d1.clone(),
        batches: Rc::default(),
        // The apply's reads are its first batch, its writes the second.
        nth: 2,
        hook: Rc::new(RefCell::new(Some(Box::new(move || {
            run(plain.apply_derived(&newer, &[])).unwrap();
        })))),
    };
    let catalog = runbook_store_d1::D1Catalog::new(racing, scope(1));
    run(catalog.apply_derived(&older, &[])).unwrap();
    let check = runbook_store_d1::D1Catalog::new(d1, scope(1));
    let summary = run(check.get_run(&base.summary.execution_id))
        .unwrap()
        .unwrap();
    assert_eq!(summary.title, "newer");
    assert_eq!(summary.sequence, base.summary.sequence + 7);
    assert!(
        run(check.list_assignments()).unwrap().is_empty(),
        "the newer inbox stays"
    );
}

/// Gate fix (low): a finished run deleted before its last derived row
/// applied keeps that row's memory proposals, as natively (they are the
/// project's memory, not the run's).
#[test]
fn a_deleted_runs_last_memory_proposals_are_kept() {
    use runbook_core::memory_entries::{MemoryProposal, MemoryStore};
    let d1 = NativeD1::new(START_MS);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let plain = runbook_store_d1::D1Catalog::new(d1.clone(), scope(1));
    let mut update = catalog::update(5, 3, runbook_model::ExecutionStatus::Succeeded);
    run(plain.claim_owner(&update.summary.execution_id)).unwrap();
    // The run was deleted (its tombstone recorded) before this row applied.
    run(runbook_store_d1::D1::run(
        &d1,
        runbook_store_d1::prepare(
            "INSERT INTO tombstone (execution_id, org_id, project_id, body) VALUES (?, ?, ?, '{}')",
        )
        .bind(update.summary.execution_id.as_str())
        .bind(scope(1).org_id.as_str())
        .bind(scope(1).project_id.as_str()),
    ))
    .unwrap();
    let mut proposed =
        runbook_store_conformance::memory_entries::entry(&scope(1), 9, START_MS, None);
    proposed.status = runbook_model::MemoryStatus::Proposed;
    proposed.source = runbook_model::MemorySource::StepRun {
        execution_id: update.summary.execution_id.clone(),
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        actor: runbook_model::ActorRef::System,
    };
    update.memory = vec![MemoryProposal {
        execution_id: update.summary.execution_id.clone(),
        sequence: 3,
        entry: proposed,
    }];
    run(plain.apply_derived(&update, &[])).unwrap();
    let memory = runbook_store_d1::D1Memory::new(d1);
    assert!(
        run(memory.get_memory(
            &scope(1),
            &runbook_store_conformance::memory_entries::mem(9)
        ))
        .unwrap()
        .is_some(),
        "the proposal was kept"
    );
}

/// Gate fix (low): the minute cron's sweeps no longer reread the same
/// oldest page. A purge or supersede notice that fails backs off on its
/// own (one minute, doubling, up to an hour), so the next ones get their
/// turn; unsettled starts page past the ones already tried.
#[test]
fn cron_sweeps_page_past_failures() {
    let d1 = NativeD1::new(START_MS);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let exec = |sql: &'static str, id: &runbook_model::ExecutionId, at: i64| {
        run(runbook_store_d1::D1::run(
            &d1,
            runbook_store_d1::prepare(sql)
                .bind(id.as_str())
                .bind(scope(1).org_id.as_str())
                .bind(scope(1).project_id.as_str())
                .bind(at),
        ))
        .unwrap();
    };
    let ids: Vec<_> = (100..130u8).map(catalog::id).collect();
    for (i, id) in ids.iter().enumerate() {
        let at = START_MS + i as i64;
        exec(
            "INSERT INTO object_purge (execution_id, org_id, project_id, created_at_ms, keep_uploads) \
             VALUES (?, ?, ?, ?, 0)",
            id,
            at,
        );
    }
    let now = START_MS + 1_000;
    let first = run(runbook_store_d1::retention::pending_purges(&d1, now, 25)).unwrap();
    assert_eq!(first.len(), 25);
    // All 25 fail: each backs off, and the next pass reaches the other 5.
    for purge in &first {
        run(runbook_store_d1::retention::defer_purge(
            &d1,
            &purge.execution_id,
            now,
        ))
        .unwrap();
    }
    let next = run(runbook_store_d1::retention::pending_purges(&d1, now, 25)).unwrap();
    assert_eq!(
        next.iter()
            .map(|p| p.execution_id.clone())
            .collect::<Vec<_>>(),
        ids[25..].to_vec()
    );
    // A minute on they are due again; a second failure doubles the wait.
    assert_eq!(
        run(runbook_store_d1::retention::pending_purges(
            &d1,
            now + 60_000,
            50
        ))
        .unwrap()
        .len(),
        30
    );
    run(runbook_store_d1::retention::defer_purge(
        &d1,
        &ids[0],
        now + 60_000,
    ))
    .unwrap();
    let due = |at| {
        run(runbook_store_d1::retention::pending_purges(&d1, at, 50))
            .unwrap()
            .iter()
            .any(|p| p.execution_id == ids[0])
    };
    assert!(!due(now + 60_000 + 119_999));
    assert!(due(now + 60_000 + 120_000));

    // Unsettled starts (no summary, no tombstone): the cursor pages past
    // the ones a pass already tried.
    for (i, id) in ids.iter().enumerate() {
        let at = START_MS + i as i64;
        exec(
            "INSERT INTO execution_owner (execution_id, org_id, project_id) \
             SELECT ?1, ?2, ?3 WHERE ?4 IS NOT NULL",
            id,
            at,
        );
        run(runbook_store_d1::D1::run(
            &d1,
            runbook_store_d1::prepare(
                "INSERT INTO start_replay (org_id, project_id, idempotency_key, principal, \
                 request_hash, execution_id, outcome, created_at_ms) \
                 VALUES (?, ?, ?, 'human:usr_1', zeroblob(32), ?, '{}', ?)",
            )
            .bind(scope(1).org_id.as_str())
            .bind(scope(1).project_id.as_str())
            .bind(format!("key-{i}"))
            .bind(id.as_str())
            .bind(at),
        ))
        .unwrap();
    }
    let page = run(runbook_store_d1::start::unsettled_starts_after(
        &d1, now, None, 25,
    ))
    .unwrap();
    assert_eq!(page.len(), 25);
    let (_, last, at) = page.last().unwrap().clone();
    let rest = run(runbook_store_d1::start::unsettled_starts_after(
        &d1,
        now,
        Some((at, &last)),
        25,
    ))
    .unwrap();
    assert_eq!(
        rest.into_iter().map(|(_, id, _)| id).collect::<Vec<_>>(),
        ids[25..].to_vec()
    );
}

/// Gate round 2: a held run is stamped with its import and wakes an hour
/// after it was held (to expire if its import never committed); a later
/// import, its D1 guards passed, may replace a stale held object, never a
/// fresh one (another import may still be running) nor a live run.
#[test]
fn a_stale_held_object_can_be_replaced_and_expires() {
    use runbook_core::ExecutionStore;
    use runbook_core::start::StartStore;
    let source = NativeObjects::default();
    let (_, store) = fresh(&source);
    let commit = runbook_worker_conformance::cases::start_commit(6);
    run(store.catalog().reserve_start(&commit)).unwrap();
    source
        .object(&catalog::id(6), &scope(1))
        .unwrap()
        .initialize_start(runbook_store_do::StartInit {
            projection: commit.projection.clone(),
            record: commit.record.clone(),
            admission: None,
            derived: commit.derived.clone(),
        })
        .unwrap();
    let stored = run(store.load(&catalog::id(6))).unwrap();
    let start = run(store.start_record(&catalog::id(6))).unwrap();
    let unit = |import_id: &str, replace_held: bool| runbook_store_do::portable::RunInstall {
        run: stored.clone(),
        actor_replays: vec![],
        work: vec![],
        start: start.clone(),
        claim_logs: vec![],
        activation_at_ms: Some(START_MS),
        held: true,
        import_id: import_id.into(),
        replace_held,
    };
    let target = NativeObjects::default();
    target.now.set(START_MS);
    let held = target.object(&catalog::id(6), &scope(1)).unwrap();
    held.install(unit("imp_a", false)).unwrap();
    assert_eq!(held.hold().unwrap(), Some(("imp_a".to_owned(), START_MS)));
    assert_eq!(
        runbook_store_do::next_alarm_at(&held).unwrap(),
        Some(START_MS + runbook_store_do::portable::HOLD_EXPIRY_MS)
    );
    // A fresh hold is another import's, perhaps still running.
    assert_eq!(
        held.install(unit("imp_b", true)),
        Err(StoreError::InvalidBatch("import.in_progress"))
    );
    // Stale: a later import replaces it.
    target
        .now
        .set(START_MS + runbook_store_do::portable::HOLD_STALE_MS);
    let held = target.object(&catalog::id(6), &scope(1)).unwrap();
    held.install(unit("imp_b", true)).unwrap();
    assert_eq!(
        held.hold().unwrap().map(|(id, _)| id),
        Some("imp_b".to_owned())
    );
    // A released (live) run is never replaced.
    held.release(&catalog::id(6)).unwrap();
    assert_eq!(
        held.install(unit("imp_c", true)),
        Err(StoreError::InvalidBatch("store.execution_exists"))
    );
}

/// Gate round 2: an import's undo deletes its runs' rows in its own tenant
/// only, never another tenant's rows under the same run ID.
#[test]
fn an_import_undo_deletes_only_its_tenants_run_rows() {
    let d1 = NativeD1::new(START_MS);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let other = scope(2);
    let id = catalog::id(70);
    let other_catalog = runbook_store_d1::D1Catalog::new(d1.clone(), other.clone());
    run(other_catalog.claim_owner(&id)).unwrap();
    let mut update = catalog::update(70, 1, runbook_model::ExecutionStatus::Running);
    update.summary.execution_id = id.clone();
    run(other_catalog.apply_derived(&update, &[])).unwrap();
    run(runbook_store_d1::D1::batch(
        &d1,
        runbook_store_d1::portable::forget_run_rows(&scope(1), std::slice::from_ref(&id)),
    ))
    .unwrap();
    assert!(
        run(other_catalog.get_run(&id)).unwrap().is_some(),
        "the other tenant's run stays"
    );
    assert_eq!(run(other_catalog.list_assignments()).unwrap().len(), 1);
}
