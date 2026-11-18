//! UP1: the upload conformance cases on PostgreSQL (migration 0028). The two
//! T0 cases run in `t0_pending.rs`. Each case owns one database.
mod support;
use runbook_core::StoreError;
use runbook_store_conformance::portable::p;
use runbook_store_conformance::uploads;
use support::factory::with_factory;

macro_rules! upload_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| uploads::$name(f).await).await;
        }
    )*};
}
upload_cases!(
    upload_ranges_cross_chunk_boundaries,
    stale_slots_are_listed_and_deleted,
    quotas_count_live_slots,
    storage_counts_stored_bytes,
    run_deletion_removes_its_uploads,
);

/// The store test for the `file`/`screenshot` capability (later-plans rule 1).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn reports_attach_finalized_uploads() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        uploads::reports_attach_finalized_uploads(&store, &f.fresh_directory()).await;
        Ok::<(), StoreError>(())
    })
    .await;
}

/// CF12b review round 2: appended lines meet required log evidence.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn appended_logs_satisfy_log_evidence() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        uploads::appended_logs_satisfy_log_evidence(&store, &f.fresh_directory()).await;
        Ok::<(), StoreError>(())
    })
    .await;
}
