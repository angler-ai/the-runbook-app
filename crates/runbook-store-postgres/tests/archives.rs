//! ARC2: the archive conformance cases on PostgreSQL (migration 0029). The
//! two T0 cases run in `t0_pending.rs`. Each case owns one database.
mod support;
use runbook_store_conformance::archives;
use support::factory::with_factory;

macro_rules! archive_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| archives::$name(f).await).await;
        }
    )*};
}
archive_cases!(
    stale_marks_follow_commits_and_deletions,
    retention_queries_respect_archives_and_tenants,
    retention_deletion_rechecks_its_file,
    evidence_follows_the_archive_and_retained_runs_can_be_purged,
    finalized_evidence_marks_its_file_stale,
    failing_days_back_off,
    versions_are_swept_only_behind_the_index,
    privacy_deletions_find_their_file_after_the_summary_purge,
    a_record_never_clears_a_later_mark_or_misses_evidence,
    retained_evidence_counts_until_its_file_expires,
    expiry_deletes_every_upload_under_its_retained_runs,
    a_live_built_folder_is_refused_for_a_retained_run,
);

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_index_swings_only_from_the_expected_file() {
    use runbook_store_conformance::{StoreFactory, execution::projection};
    with_factory(async |f| {
        archives::the_index_swings_only_from_the_expected_file(&f.create(projection()).await?).await
    })
    .await;
}

/// Re-check finding 8: a deadlock or serialization failure in
/// `record_archive` is retried (up to three attempts in all), and a fourth
/// failure surfaces without changing the index.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn record_archive_retries_deadlocks() {
    use runbook_core::{StoreError, archives::ArchiveIndex};
    use runbook_store_conformance::{catalog, scope};
    with_factory(async |f| {
        let s = archives::seeded_day(f).await?;
        let rb = runbook_store_conformance::execution::projection().runbook_id;
        let _ = catalog::id(1);
        s.inject_record_retries(2);
        archives::record_file(&s, 2, 1, 0).await?;
        assert_eq!(s.record_retries_taken(), 2);
        s.inject_record_retries(3);
        assert_eq!(
            archives::record_file(&s, 3, 2, 0).await,
            Err(StoreError::Backend("postgres.retry_transaction"))
        );
        assert_eq!(
            s.get_archive(&scope(1), &rb, 0).await?.unwrap().digest,
            archives::day0_digest(2)
        );
        Ok(())
    })
    .await;
}

/// Re-check finding 8: the lock order is the run's head, then the file's
/// row. A transaction holding a listed run's head (as a commit does) and then
/// marking the file stale runs to completion while `record_archive` waits on
/// the head: no deadlock, no retry, and the record keeps the mark.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn record_archive_takes_heads_before_the_file_row() {
    use runbook_core::archives::ArchiveIndex;
    use runbook_store_conformance::{catalog, scope};
    with_factory(async |f| {
        let s = archives::seeded_day(f).await?;
        let rb = runbook_store_conformance::execution::projection().runbook_id;
        let admin = f.admin.client();
        admin.batch_execute("BEGIN").await.unwrap();
        admin
            .query(
                "SELECT 1 FROM runbook.execution WHERE execution_id = $1 FOR UPDATE",
                &[&catalog::id(1).as_str()],
            )
            .await
            .unwrap();
        let record = archives::record_file(&s, 2, 1, 0);
        let commit = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            // As a commit's stale mark: the file row, after the head.
            admin
                .execute(
                    "UPDATE runbook.archive SET stale = true, stale_marks = stale_marks + 1 \
                     WHERE runbook_id = $1 AND day = 0",
                    &[&rb.as_str()],
                )
                .await
                .map(|_| ())
                .and(admin.batch_execute("COMMIT").await)
        };
        let (recorded, committed) = tokio::join!(record, commit);
        committed.expect("the head holder never deadlocks");
        recorded?;
        assert_eq!(s.record_retries_taken(), 0, "no deadlock was retried");
        let file = s.get_archive(&scope(1), &rb, 0).await?.unwrap();
        assert_eq!(file.digest, archives::day0_digest(2));
        assert!(file.stale, "the record keeps the mark that landed first");
        Ok(())
    })
    .await;
}

/// Wait (bounded) until `paused()` holds.
async fn until(paused: impl Fn() -> bool) {
    for _ in 0..1000 {
        if paused() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("never reached the pause");
}

/// Re-check 3, finding 2: two first writes of a day (no index row yet) on
/// two connections. The second is held after its row "lock" (which locks
/// nothing while the row is absent) while the first records; exactly one
/// wins and the other gets `archive.conflict`, never an overwrite.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn two_first_writes_of_a_day_serialize() {
    use runbook_core::{StoreError, archives::ArchiveIndex};
    use runbook_store_conformance::scope;
    with_factory(async |f| {
        let a = archives::seeded_runs(f).await?;
        let b = f.fresh_store(scope(1)).await;
        let rb = runbook_store_conformance::execution::projection().runbook_id;
        b.pause_record_after_lock();
        let (second, first) = tokio::join!(archives::record_first(&b, 2), async {
            until(|| b.record_paused()).await;
            let (first, ()) = tokio::join!(archives::record_first(&a, 1), async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                b.release_record();
            });
            first
        });
        let named = a.get_archive(&scope(1), &rb, 0).await?.unwrap().digest;
        match (first, second) {
            (Ok(()), Err(StoreError::InvalidBatch("archive.conflict"))) => {
                assert_eq!(named, archives::day0_digest(1));
            }
            (Err(StoreError::InvalidBatch("archive.conflict")), Ok(())) => {
                assert_eq!(named, archives::day0_digest(2));
            }
            other => panic!("one first write must lose: {other:?}"),
        }
        Ok(())
    })
    .await;
}

/// Re-check 3, finding 2: the stored-blob check runs under the row lock. A
/// sweep holding the row deletes the writer's version while the writer waits
/// on the lock; the writer then fails `archive.blob_missing` and the index
/// keeps the file it had.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_blob_check_runs_under_the_row_lock() {
    use runbook_core::{StoreError, archives::ArchiveIndex};
    use runbook_store_conformance::scope;
    with_factory(async |f| {
        let s = archives::seeded_day(f).await?;
        let rb = runbook_store_conformance::execution::projection().runbook_id;
        archives::put_version(&s, 2).await?;
        let admin = f.admin.client();
        admin.batch_execute("BEGIN").await.unwrap();
        admin
            .query(
                "SELECT 1 FROM runbook.archive WHERE runbook_id = $1 AND day = 0 FOR UPDATE",
                &[&rb.as_str()],
            )
            .await
            .unwrap();
        let record = archives::record_file(&s, 2, 1, 0);
        let sweep = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            admin
                .execute(
                    "DELETE FROM runbook.blob WHERE key = (SELECT key FROM runbook.blob \
                     WHERE created_at_us IS NOT NULL ORDER BY created_at_us DESC LIMIT 1)",
                    &[],
                )
                .await
                .map(|_| ())
                .and(admin.batch_execute("COMMIT").await)
        };
        let (recorded, swept) = tokio::join!(record, sweep);
        swept.unwrap();
        assert_eq!(
            recorded,
            Err(StoreError::InvalidBatch("archive.blob_missing"))
        );
        let file = s.get_archive(&scope(1), &rb, 0).await?.unwrap();
        assert_eq!(file.digest, archives::day0_digest(1));
        Ok(())
    })
    .await;
}

/// Re-check 3, test gap: `record_archive` reads a retained run's tombstone
/// `FOR SHARE`. A single-run deletion holding the tombstone (its reason
/// changed, not yet committed) makes the record wait and then fail
/// `archive.run_gone` instead of writing the run back into the file.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_record_waits_for_a_purge_holding_the_tombstone() {
    use runbook_core::{StoreError, archives::RETENTION_REASON, retention::RunStore};
    use runbook_store_conformance::catalog;
    with_factory(async |f| {
        let s = archives::seeded_day(f).await?;
        s.delete_execution(&catalog::id(1), RETENTION_REASON)
            .await?;
        let admin = f.admin.client();
        admin.batch_execute("BEGIN").await.unwrap();
        let changed = admin
            .execute(
                "UPDATE runbook.tombstone SET body = replace(body, $2, '\"privacy request\"') \
                 WHERE execution_id = $1",
                &[&catalog::id(1).as_str(), &format!("\"{RETENTION_REASON}\"")],
            )
            .await
            .unwrap();
        assert_eq!(changed, 1);
        let record = archives::record_file(&s, 2, 1, 0);
        let purge = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            admin.batch_execute("COMMIT").await
        };
        let (recorded, purged) = tokio::join!(record, purge);
        purged.unwrap();
        assert_eq!(recorded, Err(StoreError::InvalidBatch("archive.run_gone")));
        Ok(())
    })
    .await;
}

/// Re-check 3, test gap: `record_archive` reads a retained run's summary
/// row `FOR SHARE`. A purge holding the row's deletion open makes the record
/// wait; the purged row is not restored.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_record_never_restores_a_summary_row_purged_meanwhile() {
    use runbook_core::{
        archives::{ArchiveIndex, RETENTION_REASON},
        retention::RunStore,
    };
    use runbook_store_conformance::{catalog, scope};
    with_factory(async |f| {
        let s = archives::seeded_day(f).await?;
        s.delete_execution(&catalog::id(1), RETENTION_REASON)
            .await?;
        let admin = f.admin.client();
        admin.batch_execute("BEGIN").await.unwrap();
        admin
            .execute(
                "DELETE FROM runbook.archive_run WHERE execution_id = $1",
                &[&catalog::id(1).as_str()],
            )
            .await
            .unwrap();
        let record = archives::record_file(&s, 2, 1, 0);
        let purge = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            admin.batch_execute("COMMIT").await
        };
        let (recorded, purged) = tokio::join!(record, purge);
        purged.unwrap();
        recorded?;
        assert_eq!(s.archived_run(&scope(1), &catalog::id(1)).await?, None);
        assert!(s.archived_run(&scope(1), &catalog::id(2)).await?.is_some());
        Ok(())
    })
    .await;
}

/// Re-check 3, test gap: finalize takes the run's head `FOR UPDATE`. A
/// first write of the day holding the head `FOR SHARE` (before its
/// membership exists) makes a finalize wait, so the finalize's stale mark
/// finds the new file.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_finalize_waits_for_a_record_holding_the_head() {
    use runbook_core::archives::ArchiveIndex;
    use runbook_store_conformance::scope;
    with_factory(async |f| {
        let a = archives::seeded_runs(f).await?;
        archives::pending_upload(&a, 1).await?;
        let b = f.fresh_store(scope(1)).await;
        let rb = runbook_store_conformance::execution::projection().runbook_id;
        b.pause_record_after_lock();
        let (recorded, finalized) = tokio::join!(archives::record_first(&b, 1), async {
            until(|| b.record_paused()).await;
            let (finalized, ()) = tokio::join!(archives::finalize_upload(&a, 1), async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                b.release_record();
            });
            finalized
        });
        recorded?;
        finalized?;
        let file = a.get_archive(&scope(1), &rb, 0).await?.unwrap();
        assert!(file.stale, "the finalize's mark found the new file");
        Ok(())
    })
    .await;
}

/// Re-check 3, findings 1 and 3: the grace-period branch on PostgreSQL. A
/// leftover version newer than the named file but older than the grace
/// period goes on a time-driven pass; a younger one and the named file stay.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn leftovers_past_the_grace_period_are_swept_without_a_write() {
    use runbook_core::archives::ArchiveIndex;
    use runbook_store_conformance::scope;
    with_factory(async |f| {
        let s = archives::seeded_day(f).await?;
        let admin = f.admin.client();
        let age = async |hours: i64| {
            admin
                .execute(
                    "UPDATE runbook.blob SET created_at_us = created_at_us - $1 \
                     WHERE created_at_us IS NOT NULL",
                    &[&(hours * 3_600_000_000)],
                )
                .await
        };
        age(1).await.unwrap();
        archives::put_version(&s, 3).await?;
        // The named file is now three hours old, the leftover two.
        age(2).await.unwrap();
        archives::put_version(&s, 4).await?;
        assert_eq!(s.sweep_leftover_versions(&scope(1), 10).await?, 1);
        assert!(archives::version_stored(&s, 1).await?, "the named file");
        assert!(!archives::version_stored(&s, 3).await?, "the old leftover");
        assert!(archives::version_stored(&s, 4).await?, "the young leftover");
        assert_eq!(s.sweep_leftover_versions(&scope(1), 10).await?, 0);
        Ok(())
    })
    .await;
}

/// Re-check 3, low finding: two concurrent single-run deletions of a run
/// retention already deleted both return the tombstone (as the memory store
/// does), never `NotFound` for the one that waited.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn duplicate_privacy_deletions_of_a_retained_run_both_return_the_tombstone() {
    use runbook_core::{archives::RETENTION_REASON, retention::RunStore};
    use runbook_store_conformance::{catalog, scope};
    with_factory(async |f| {
        let s = archives::seeded_day(f).await?;
        s.delete_execution(&catalog::id(1), RETENTION_REASON)
            .await?;
        let other = f.fresh_store(scope(1)).await;
        let admin = f.admin.client();
        admin.batch_execute("BEGIN").await.unwrap();
        admin
            .query(
                "SELECT 1 FROM runbook.tombstone WHERE execution_id = $1 FOR UPDATE",
                &[&catalog::id(1).as_str()],
            )
            .await
            .unwrap();
        let release = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            admin.batch_execute("COMMIT").await
        };
        let run = catalog::id(1);
        let (one, two, released) = tokio::join!(
            s.delete_execution(&run, "privacy request"),
            other.delete_execution(&run, "privacy request"),
            release
        );
        released.unwrap();
        assert_eq!(one?.reason, "privacy request");
        assert_eq!(two?.reason, "privacy request");
        Ok(())
    })
    .await;
}
