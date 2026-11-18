//! ArchiveIndex and archive-blob cases (PRD §14, D53–D54). Registered red at
//! T0; ARC1 makes them pass on both adapters and ARC2 adds the retention
//! queries and the in-transaction stale marks.
use crate::{QueueFactory, catalog, execution::projection, retention, scope};
use runbook_core::{
    ExecutionStore, StoreError,
    archives::{
        ARCHIVE_SCHEMA_VERSION, ArchiveIndex, ArchiveRecord, ArchivedRun, ArchivedRunSummary,
        RETENTION_REASON, backoff_ms,
    },
    blob::{BlobKey, BlobPrefix, BlobStore},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    retention::RunStore,
    uploads::{UploadOutcome, UploadQuota, UploadStore, VerifiedUpload},
};
use runbook_model::{ExecutionId, ExecutionStatus};

const DAY: i64 = 20_000;
const NOW: i64 = (DAY + 2) * 86_400_000;
fn record(scope: &TenantScope, digest: u8) -> ArchiveRecord {
    day_record(scope, DAY, digest)
}
fn day_record(scope: &TenantScope, day: i64, n: u8) -> ArchiveRecord {
    ArchiveRecord {
        scope: scope.clone(),
        runbook_id: projection().runbook_id,
        day,
        run_count: 1,
        size_bytes: file_bytes(day, n).len() as u64,
        digest: digest(day, n),
        schema_version: ARCHIVE_SCHEMA_VERSION,
        written_at_ms: (day + 1) * 86_400_000,
        stale: false,
        expires_at_ms: None,
        stale_marks: 0,
    }
}
/// The bytes of file `n` of `day` (any bytes: the index does not parse them).
fn file_bytes(day: i64, n: u8) -> Vec<u8> {
    format!("PK daily file {day} {n}").into_bytes()
}
fn digest(day: i64, n: u8) -> [u8; 32] {
    runbook_core::portable::sha256(&file_bytes(day, n))
}
fn day_key(day: i64, digest: [u8; 32]) -> BlobKey {
    BlobKey::Archive {
        runbook_id: projection().runbook_id,
        day,
        digest,
    }
}
/// Store file `n` of `day`, then record it with the stale-mark count the
/// writer read, as the job does.
async fn rec<S: ArchiveIndex + BlobStore>(
    s: &S,
    scope: &TenantScope,
    _: &TenantScope,
    day: i64,
    n: u8,
    runs: Vec<ArchivedRun>,
    expected: Option<[u8; 32]>,
) -> Result<(), StoreError> {
    s.put(day_key(day, digest(day, n)), &file_bytes(day, n))
        .await?;
    let mut record = day_record(scope, day, n);
    record.stale_marks = s
        .get_archive(scope, &projection().runbook_id, day)
        .await?
        .map_or(0, |r| r.stale_marks);
    s.record_archive(scope, record, runs, expected).await
}
fn summary(completed_at_ms: i64) -> ArchivedRunSummary {
    ArchivedRunSummary {
        version_id: projection().version_id,
        version: std::num::NonZeroU32::new(1),
        status: ExecutionStatus::Succeeded,
        title: "Verify run".into(),
        created_by: "operator".into(),
        created_at_ms: 1_000,
        completed_at_ms,
    }
}
fn listed(id: ExecutionId, day: i64, completed_at_ms: i64) -> ArchivedRun {
    // The seeded runs' revisions (`retention::seed`): run 1 at 2, run 2 at 1.
    let revision = if id == catalog::id(1) { 2 } else { 1 };
    ArchivedRun {
        folder: format!("{id}/"),
        execution_id: id,
        runbook_id: projection().runbook_id,
        day,
        revision,
        summary: summary(completed_at_ms),
        evidence: vec![],
        gathered_live: false,
    }
}
fn run() -> ArchivedRun {
    ArchivedRun {
        revision: 0,
        ..listed(projection().execution_id, DAY, DAY * 86_400_000 + 5)
    }
}

/// Record, list, locate a run, mark stale once, rewrite, delete; another
/// tenant sees none of it.
pub async fn index_round_trip_and_stale_marks<A: ArchiveIndex + BlobStore + CatalogStore>(
    a: &A,
) -> Result<(), StoreError> {
    own_runbook(a).await?;
    let (mine, theirs) = (scope(1), scope(2));
    let rb = projection().runbook_id;
    rec(a, &mine, &mine, DAY, 1, vec![run()], None).await?;
    assert_eq!(
        a.get_archive(&mine, &rb, DAY).await?,
        Some(record(&mine, 1))
    );
    assert_eq!(a.get_archive(&theirs, &rb, DAY).await?, None);
    assert!(a.list_archives(&theirs, &rb, None, None).await?.is_empty());
    assert_eq!(
        a.archived_run(&mine, &projection().execution_id).await?,
        Some(run())
    );
    assert_eq!(
        a.archived_run(&theirs, &projection().execution_id).await?,
        None
    );
    assert!(a.mark_archive_stale(&mine, &rb, DAY).await?);
    assert!(!a.mark_archive_stale(&mine, &rb, DAY).await?);
    assert!(!a.mark_archive_stale(&theirs, &rb, DAY).await?);
    assert_eq!(a.stale_archives(&mine, NOW, 10).await?.len(), 1);
    assert!(a.stale_archives(&theirs, NOW, 10).await?.is_empty());
    rec(a, &mine, &mine, DAY, 2, vec![run()], Some(digest(DAY, 1))).await?;
    assert!(a.stale_archives(&mine, NOW, 10).await?.is_empty());
    let listed = a.list_archives(&mine, &rb, Some(DAY), Some(DAY)).await?;
    // Every mark counts, the second on an already-stale file too.
    assert_eq!(
        listed,
        [ArchiveRecord {
            stale_marks: 2,
            ..record(&mine, 2)
        }]
    );
    assert!(
        a.list_archives(&mine, &rb, Some(DAY + 1), None)
            .await?
            .is_empty()
    );
    assert!(
        a.list_archives(&mine, &rb, None, Some(DAY - 1))
            .await?
            .is_empty()
    );
    // A run listed for another tenant is refused, nothing changes.
    assert!(
        rec(a, &theirs, &theirs, DAY, 3, vec![run()], None)
            .await
            .is_err()
    );
    assert_eq!(a.get_archive(&theirs, &rb, DAY).await?, None);
    assert!(!a.delete_archive(&theirs, &rb, DAY, digest(DAY, 2)).await?);
    // Deleting a file is conditional: on the file the caller read, and
    // never while a listed run is live (it would only be archived again).
    assert_eq!(
        a.delete_archive(&mine, &rb, DAY, digest(DAY, 1)).await,
        Err(StoreError::InvalidBatch("archive.conflict"))
    );
    assert_eq!(
        a.delete_archive(&mine, &rb, DAY, digest(DAY, 2)).await,
        Err(StoreError::InvalidBatch("archive.member_live"))
    );
    assert!(a.get_archive(&mine, &rb, DAY).await?.is_some());
    assert_eq!(
        a.file_members(&mine, &rb, DAY).await?,
        [projection().execution_id]
    );
    assert!(a.file_members(&theirs, &rb, DAY).await?.is_empty());
    Ok(())
}

/// The index swings only from the file the writer read: a record whose
/// expected previous digest is not the current one changes nothing.
pub async fn the_index_swings_only_from_the_expected_file<
    A: ArchiveIndex + BlobStore + CatalogStore,
>(
    a: &A,
) -> Result<(), StoreError> {
    own_runbook(a).await?;
    let mine = scope(1);
    let rb = projection().runbook_id;
    let conflict = Err(StoreError::InvalidBatch("archive.conflict"));
    assert_eq!(
        rec(a, &mine, &mine, DAY, 1, vec![run()], Some(digest(DAY, 9))).await,
        conflict
    );
    assert_eq!(a.get_archive(&mine, &rb, DAY).await?, None);
    rec(a, &mine, &mine, DAY, 1, vec![run()], None).await?;
    // A second first writer, and a writer that read an older file, lose.
    assert_eq!(
        rec(a, &mine, &mine, DAY, 2, vec![run()], None).await,
        conflict
    );
    assert_eq!(
        rec(a, &mine, &mine, DAY, 3, vec![run()], Some(digest(DAY, 2))).await,
        conflict
    );
    assert_eq!(
        a.get_archive(&mine, &rb, DAY).await?.unwrap().digest,
        digest(DAY, 1)
    );
    rec(a, &mine, &mine, DAY, 2, vec![run()], Some(digest(DAY, 1))).await?;
    assert_eq!(
        a.get_archive(&mine, &rb, DAY).await?.unwrap().digest,
        digest(DAY, 2)
    );
    Ok(())
}

/// The runs' runbook in scope 1 (archive files belong to it). Idempotent.
async fn own_runbook<S: CatalogStore>(s: &S) -> Result<(), StoreError> {
    if s.read_runbook(&projection().runbook_id).await?.is_some() {
        return Ok(());
    }
    s.create_runbook(
        RunbookRecord {
            runbook_id: projection().runbook_id,
            slug: "verify".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        "draft".into(),
    )
    .await
    .map(|_| ())
}

fn file_key(digest: [u8; 32]) -> BlobKey {
    BlobKey::Archive {
        runbook_id: projection().runbook_id,
        day: DAY,
        digest,
    }
}

/// Archive ZIPs are blobs of their runbook's tenant only, addressed by
/// their content's digest, immutable, read by range, listed per day.
pub async fn archive_blobs_are_tenant_scoped<F: QueueFactory>(f: &F) -> Result<(), StoreError> {
    let s = f.create(projection()).await?;
    own_runbook(&s).await?;
    let foreign = f.scoped_client(&s, scope(2)).await?;
    let bytes = b"PK\x05\x06".to_vec();
    let key = file_key(runbook_core::portable::sha256(&bytes));
    s.put(key.clone(), &bytes).await?;
    assert_eq!(s.get(&key).await?, bytes);
    assert_eq!(s.get_range(&key, 0..2).await?, b"PK");
    assert_eq!(s.head(&key).await?.size, 4);
    assert_eq!(foreign.get(&key).await, Err(StoreError::NotFound));
    assert_eq!(foreign.head(&key).await, Err(StoreError::NotFound));
    assert!(foreign.put(key.clone(), &bytes).await.is_err());
    // Bytes must match the key's digest; a key never changes content.
    assert_eq!(
        s.put(file_key([0; 32]), &bytes).await,
        Err(StoreError::InvalidBatch("blob.digest_mismatch"))
    );
    let other = b"PK rewritten".to_vec();
    let next = file_key(runbook_core::portable::sha256(&other));
    s.put(next.clone(), &other).await?;
    let prefix = BlobPrefix::Archive {
        runbook_id: projection().runbook_id,
        day: DAY,
    };
    let mut listed = s.list_prefix(&prefix).await?;
    listed.sort();
    let mut both = vec![key.clone(), next.clone()];
    both.sort();
    assert_eq!(listed, both);
    assert!(foreign.list_prefix(&prefix).await?.is_empty());
    assert!(foreign.delete(&key).await.is_err());
    s.delete(&key).await?;
    assert_eq!(s.get(&key).await, Err(StoreError::NotFound));
    assert_eq!(s.get(&next).await?, other);
    Ok(())
}

/// The stores mark a file stale in the same transaction that makes it out
/// of date: new events on a listed run (an amendment), or a deletion for any
/// reason but retention, which also drops the run's summary row. Retention's
/// own deletion keeps the row: the file is now the copy. A rewrite may list
/// a run only while it is live or retained by retention.
pub async fn stale_marks_follow_commits_and_deletions<F: QueueFactory>(
    f: &F,
) -> Result<(), StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let rb = projection().runbook_id;
    let both = || vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)];
    rec(&s, &mine, &mine, 0, 1, both(), None).await?;
    assert!(s.stale_archives(&mine, NOW, 10).await?.is_empty());
    // An amendment-like commit to a finished, archived run.
    s.commit(catalog::batch(1, 2, 3, ExecutionStatus::Succeeded))
        .await?;
    assert_eq!(
        s.stale_archives(&mine, NOW, 10).await?,
        [ArchiveRecord {
            stale: true,
            stale_marks: 1,
            ..day_record(&mine, 0, 1)
        }]
    );
    // A rewrite built before the amendment (run 1 still at revision 2) is
    // recorded, but the file stays stale: the next pass rewrites it again.
    rec(&s, &mine, &mine, 0, 2, both(), Some(digest(0, 1))).await?;
    assert_eq!(s.stale_archives(&mine, NOW, 10).await?.len(), 1);
    let current = || {
        let mut runs = both();
        runs[0].revision = 3;
        runs
    };
    rec(&s, &mine, &mine, 0, 4, current(), Some(digest(0, 2))).await?;
    assert!(s.stale_archives(&mine, NOW, 10).await?.is_empty());
    // Retention deletion: the file stays current and keeps the summary row.
    s.delete_execution(&catalog::id(2), RETENTION_REASON)
        .await?;
    assert!(s.stale_archives(&mine, NOW, 10).await?.is_empty());
    assert_eq!(
        s.archived_run(&mine, &catalog::id(2)).await?,
        Some(current()[1].clone())
    );
    // A single-run deletion: stale, and the run's row is gone at once.
    s.delete_execution(&catalog::id(1), "privacy request")
        .await?;
    assert_eq!(s.stale_archives(&mine, NOW, 10).await?.len(), 1);
    assert_eq!(s.archived_run(&mine, &catalog::id(1)).await?, None);
    // The rewrite may not bring the deleted run back; the retained one stays.
    assert_eq!(
        rec(&s, &mine, &mine, 0, 3, current(), Some(digest(0, 4))).await,
        Err(StoreError::InvalidBatch("archive.run_gone"))
    );
    assert_eq!(s.stale_archives(&mine, NOW, 10).await?.len(), 1);
    rec(
        &s,
        &mine,
        &mine,
        0,
        3,
        vec![current()[1].clone()],
        Some(digest(0, 4)),
    )
    .await?;
    assert!(s.stale_archives(&mine, NOW, 10).await?.is_empty());
    assert_eq!(
        s.list_archives(&mine, &rb, None, None).await?[0].digest,
        digest(0, 3)
    );
    Ok(())
}

/// The job's queries: unarchived days, archived live runs past a cutoff
/// (never while the file is stale), expired files and summary purges, all
/// tenant-filtered.
pub async fn retention_queries_respect_archives_and_tenants<F: QueueFactory>(
    f: &F,
) -> Result<(), StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let (mine, theirs) = (scope(1), scope(2));
    let rb = projection().runbook_id;
    // Runs 1 and 2 finished at 10 ms: UTC day 0.
    assert_eq!(
        s.unarchived_days(&mine, 1, NOW, 10).await?,
        [(rb.clone(), 0)]
    );
    assert!(s.unarchived_days(&mine, 0, NOW, 10).await?.is_empty());
    assert!(s.unarchived_days(&theirs, 1, NOW, 10).await?.is_empty());
    assert!(s.archived_live_runs(&mine, 11, 10).await?.is_empty());
    rec(
        &s,
        &mine,
        &mine,
        0,
        1,
        vec![listed(catalog::id(1), 0, 10)],
        None,
    )
    .await?;
    // Run 2 is not listed yet: its day still needs a (re)write.
    assert_eq!(
        s.unarchived_days(&mine, 1, NOW, 10).await?,
        [(rb.clone(), 0)]
    );
    rec(
        &s,
        &mine,
        &mine,
        0,
        2,
        vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)],
        Some(digest(0, 1)),
    )
    .await?;
    assert!(s.unarchived_days(&mine, 1, NOW, 10).await?.is_empty());
    assert_eq!(
        s.archived_live_runs(&mine, 11, 10).await?,
        [catalog::id(1), catalog::id(2)]
    );
    assert_eq!(s.archived_live_runs(&mine, 11, 1).await?, [catalog::id(1)]);
    assert!(s.archived_live_runs(&mine, 10, 10).await?.is_empty());
    assert!(s.archived_live_runs(&theirs, 11, 10).await?.is_empty());
    assert!(s.mark_archive_stale(&mine, &rb, 0).await?);
    assert!(
        s.archived_live_runs(&mine, 11, 10).await?.is_empty(),
        "never delete live detail while its file owes a rewrite"
    );
    rec(
        &s,
        &mine,
        &mine,
        0,
        3,
        vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)],
        Some(digest(0, 2)),
    )
    .await?;
    assert_eq!(s.expired_archives(&mine, 1, 10).await?.len(), 1);
    assert!(s.expired_archives(&mine, 0, 10).await?.is_empty());
    assert!(s.expired_archives(&theirs, 1, 10).await?.is_empty());
    // Summary rows go only for runs no longer live.
    assert_eq!(s.purge_summaries(&mine, 11, 10).await?, 0);
    s.delete_execution(&catalog::id(1), RETENTION_REASON)
        .await?;
    assert_eq!(s.archived_live_runs(&mine, 11, 10).await?, [catalog::id(2)]);
    assert_eq!(s.purge_summaries(&theirs, 11, 10).await?, 0);
    assert_eq!(s.purge_summaries(&mine, 10, 10).await?, 0);
    assert_eq!(s.purge_summaries(&mine, 11, 10).await?, 1);
    assert_eq!(s.archived_run(&mine, &catalog::id(1)).await?, None);
    assert!(s.archived_run(&mine, &catalog::id(2)).await?.is_some());
    // A purged summary does not make the day look unarchived, and a
    // rewrite does not bring it back.
    assert!(s.unarchived_days(&mine, 1, NOW, 10).await?.is_empty());
    rec(
        &s,
        &mine,
        &mine,
        0,
        4,
        vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)],
        Some(digest(0, 3)),
    )
    .await?;
    assert_eq!(s.archived_run(&mine, &catalog::id(1)).await?, None);
    Ok(())
}

/// Finding 1 (review 2026-11-12): retention rechecks the file inside its own
/// deletion. The job lists X from a snapshot; an amendment to X then commits
/// and marks the file stale; the deletion must refuse, or the rewrite would
/// copy X's pre-amendment folder from the old file and lose the amendment.
/// A run no file lists, or listed at an older revision, is refused too.
pub async fn retention_deletion_rechecks_its_file<F: QueueFactory>(f: &F) -> Result<(), StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let refused = Err(StoreError::InvalidBatch("archive.not_current"));
    // Not archived at all.
    assert_eq!(
        s.delete_execution(&catalog::id(1), RETENTION_REASON)
            .await
            .map(|_| ()),
        refused
    );
    rec(
        &s,
        &mine,
        &mine,
        0,
        1,
        vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)],
        None,
    )
    .await?;
    assert_eq!(
        s.archived_live_runs(&mine, 11, 10).await?,
        [catalog::id(1), catalog::id(2)]
    );
    // The amendment lands between the listing and the deletion.
    s.commit(catalog::batch(1, 2, 3, ExecutionStatus::Succeeded))
        .await?;
    assert_eq!(
        s.delete_execution(&catalog::id(1), RETENTION_REASON)
            .await
            .map(|_| ()),
        refused
    );
    assert!(s.load(&catalog::id(1)).await.is_ok(), "nothing was deleted");
    assert_eq!(s.tombstone(&catalog::id(1)).await?, None);
    // While the file is stale even an unchanged sibling waits.
    assert_eq!(
        s.delete_execution(&catalog::id(2), RETENTION_REASON)
            .await
            .map(|_| ()),
        refused
    );
    // A rewrite recorded from a build that predates the amendment is
    // stale; still refused. The current rewrite unlocks both.
    rec(
        &s,
        &mine,
        &mine,
        0,
        2,
        vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)],
        Some(digest(0, 1)),
    )
    .await?;
    assert_eq!(
        s.delete_execution(&catalog::id(1), RETENTION_REASON)
            .await
            .map(|_| ()),
        refused
    );
    let mut current = listed(catalog::id(1), 0, 10);
    current.revision = 3;
    rec(
        &s,
        &mine,
        &mine,
        0,
        3,
        vec![current, listed(catalog::id(2), 0, 10)],
        Some(digest(0, 2)),
    )
    .await?;
    s.delete_execution(&catalog::id(1), RETENTION_REASON)
        .await?;
    s.delete_execution(&catalog::id(2), RETENTION_REASON)
        .await?;
    Ok(())
}

async fn upload<S: BlobStore + UploadStore>(
    s: &S,
    n: u8,
    finalize: bool,
) -> Result<BlobKey, StoreError> {
    let mine = scope(1);
    let now = s.upload_time_ms().await?;
    let mut record = crate::uploads::slot(&mine, n, now);
    record.execution_id = catalog::id(n);
    s.reserve_evidence(&mine, record, &UploadQuota::default())
        .await?;
    let key = BlobKey::Upload {
        execution_id: catalog::id(n),
        evd_id: crate::uploads::evd(n),
    };
    s.put(key.clone(), &[n; 3]).await?;
    if finalize {
        s.finalize_evidence(
            &mine,
            &crate::uploads::evd(n),
            UploadOutcome::Verified(VerifiedUpload {
                size: 3,
                content_type: "image/png".into(),
                width: None,
                height: None,
                thumbnail: false,
                thumbnail_bytes: 0,
            }),
            &UploadQuota::default(),
        )
        .await?;
    }
    Ok(key)
}

/// PRD §14 (decided 2026-11-12): when the live window ends only the live
/// copy goes; uploaded evidence follows the archive. Retention's deletion
/// keeps the objects (readable, never writable), a single-run deletion of a
/// run retention already deleted (finding 3) drops them with its summary row
/// and marks the file stale, and another tenant never sees them.
pub async fn evidence_follows_the_archive_and_retained_runs_can_be_purged<F>(
    f: &F,
) -> Result<(), StoreError>
where
    F: QueueFactory,
    F::Store: ArchiveIndex + UploadStore,
{
    let s = retention::seed(f).await?;
    // Retained uploads belong to the run's runbook once the run is gone.
    own_runbook(&s).await?;
    let mine = scope(1);
    let key = upload(&s, 1, true).await?;
    let mut run1 = listed(catalog::id(1), 0, 10);
    run1.revision = 2;
    // The file lists the run's finalized evidence; otherwise it is stale.
    run1.evidence = vec![crate::uploads::evd(1)];
    rec(
        &s,
        &mine,
        &mine,
        0,
        1,
        vec![run1, listed(catalog::id(2), 0, 10)],
        None,
    )
    .await?;
    // Finalizing evidence on an archived run is a change the file must
    // carry (finding 10); this file was recorded after it, so it is current.
    assert!(s.stale_archives(&mine, NOW, 10).await?.is_empty());
    s.delete_execution(&catalog::id(1), RETENTION_REASON)
        .await?;
    assert_eq!(s.get(&key).await?, vec![1; 3], "retention keeps uploads");
    assert_eq!(s.head(&key).await?.size, 3);
    let foreign = f.scoped_client(&s, scope(2)).await?;
    assert_eq!(foreign.get(&key).await, Err(StoreError::NotFound));
    assert!(
        s.put(key.clone(), &[1; 3]).await.is_err(),
        "never written again"
    );
    // A privacy request after retention: reason changes, row, objects go.
    assert_eq!(
        foreign
            .purge_retained_run(&scope(2), &catalog::id(1), "privacy request")
            .await?,
        None
    );
    assert_eq!(
        s.purge_retained_run(&mine, &catalog::id(2), "privacy request")
            .await?,
        None,
        "a live run is not retained"
    );
    let tombstone = s
        .purge_retained_run(&mine, &catalog::id(1), "privacy request")
        .await?
        .unwrap();
    assert_eq!(tombstone.reason, "privacy request");
    assert_eq!(s.tombstone(&catalog::id(1)).await?, Some(tombstone));
    assert_eq!(s.archived_run(&mine, &catalog::id(1)).await?, None);
    assert_eq!(s.stale_archives(&mine, NOW, 10).await?.len(), 1);
    assert_eq!(s.get(&key).await, Err(StoreError::NotFound));
    // The rewrite may no longer list it (not retained any more).
    assert_eq!(
        rec(
            &s,
            &mine,
            &mine,
            0,
            2,
            vec![listed(catalog::id(1), 0, 10)],
            Some(digest(0, 1))
        )
        .await,
        Err(StoreError::InvalidBatch("archive.run_gone"))
    );
    Ok(())
}

/// Finding 10: evidence finalized on a run its file already lists makes the
/// file stale (the manifest lists finalized evidence).
pub async fn finalized_evidence_marks_its_file_stale<F>(f: &F) -> Result<(), StoreError>
where
    F: QueueFactory,
    F::Store: ArchiveIndex + UploadStore,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let mut run1 = listed(catalog::id(1), 0, 10);
    run1.revision = 2;
    rec(&s, &mine, &mine, 0, 1, vec![run1], None).await?;
    upload(&s, 1, false).await?;
    assert!(
        s.stale_archives(&mine, NOW, 10).await?.is_empty(),
        "a pending slot is not listed"
    );
    s.finalize_evidence(
        &mine,
        &crate::uploads::evd(1),
        UploadOutcome::Verified(VerifiedUpload {
            size: 3,
            content_type: "image/png".into(),
            width: None,
            height: None,
            thumbnail: false,
            thumbnail_bytes: 0,
        }),
        &UploadQuota::default(),
    )
    .await?;
    assert_eq!(s.stale_archives(&mine, NOW, 10).await?.len(), 1);
    Ok(())
}

/// Finding 7: a failing day backs off (one minute doubling, at most a day)
/// so later days still progress; a successful record clears it.
pub async fn failing_days_back_off<F: QueueFactory>(f: &F) -> Result<(), StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let (mine, theirs) = (scope(1), scope(2));
    let rb = projection().runbook_id;
    assert_eq!(s.archive_failed(&mine, &rb, 0, NOW).await?, 1);
    assert!(s.unarchived_days(&mine, 1, NOW, 10).await?.is_empty());
    assert!(
        s.unarchived_days(&mine, 1, NOW + backoff_ms(1) - 1, 10)
            .await?
            .is_empty()
    );
    assert_eq!(
        s.unarchived_days(&mine, 1, NOW + backoff_ms(1), 10).await?,
        [(rb.clone(), 0)]
    );
    assert_eq!(s.archive_failed(&mine, &rb, 0, NOW).await?, 2);
    assert!(
        s.unarchived_days(&mine, 1, NOW + backoff_ms(1), 10)
            .await?
            .is_empty()
    );
    assert_eq!(backoff_ms(2), 2 * backoff_ms(1));
    assert_eq!(backoff_ms(40), 86_400_000);
    // Another tenant's backoff is its own.
    assert_eq!(s.archive_failed(&theirs, &rb, 0, NOW).await?, 1);
    // A stale file backs off the same way.
    rec(
        &s,
        &mine,
        &mine,
        0,
        1,
        vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)],
        None,
    )
    .await?;
    assert!(s.unarchived_days(&mine, 1, NOW, 10).await?.is_empty());
    assert!(s.mark_archive_stale(&mine, &rb, 0).await?);
    assert_eq!(s.stale_archives(&mine, NOW, 10).await?.len(), 1);
    assert_eq!(
        s.archive_failed(&mine, &rb, 0, NOW).await?,
        1,
        "cleared by the record"
    );
    assert!(s.stale_archives(&mine, NOW, 10).await?.is_empty());
    assert_eq!(
        s.stale_archives(&mine, NOW + backoff_ms(1), 10)
            .await?
            .len(),
        1
    );
    Ok(())
}

/// Re-check finding 1: stored versions are deleted only by the store, under
/// the index row's lock, and never one younger than the version the index
/// names (another writer's in-flight file). The index never names a missing
/// file, and retention never relies on a file that is gone.
pub async fn versions_are_swept_only_behind_the_index<F: QueueFactory>(
    f: &F,
) -> Result<(), StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let rb = projection().runbook_id;
    let both = || vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)];
    let keys: Vec<BlobKey> = (0..=4).map(|n| day_key(0, digest(0, n))).collect();
    let stored = |n: usize| s.head(&keys[n]);
    rec(&s, &mine, &mine, 0, 1, both(), None).await?;
    // Another writer puts version 2 and has not recorded it yet: a sweep by
    // anyone (a lagging writer's, an expiry pass) keeps it.
    s.put(day_key(0, digest(0, 2)), &file_bytes(0, 2)).await?;
    s.sweep_archive_versions(&mine, &rb, 0).await?;
    assert!(stored(1).await.is_ok() && stored(2).await.is_ok());
    // Recording it removes the older named version, under the same lock.
    let mut record = day_record(&mine, 0, 2);
    record.stale_marks = s.get_archive(&mine, &rb, 0).await?.unwrap().stale_marks;
    s.record_archive(&mine, record, both(), Some(digest(0, 1)))
        .await?;
    assert_eq!(stored(1).await, Err(StoreError::NotFound));
    assert!(stored(2).await.is_ok());
    // The index never names bytes that are not stored.
    assert_eq!(
        s.record_archive(&mine, day_record(&mine, 0, 3), both(), Some(digest(0, 2)))
            .await,
        Err(StoreError::InvalidBatch("archive.blob_missing"))
    );
    assert_eq!(
        s.get_archive(&mine, &rb, 0).await?.unwrap().digest,
        digest(0, 2)
    );
    // Retention refuses when the named file is gone, however it went.
    s.delete(&day_key(0, digest(0, 2))).await?;
    assert_eq!(
        s.delete_execution(&catalog::id(1), RETENTION_REASON)
            .await
            .map(|_| ()),
        Err(StoreError::InvalidBatch("archive.not_current"))
    );
    assert!(s.load(&catalog::id(1)).await.is_ok());
    Ok(())
}

/// Re-check findings 2 and 7: a single-run deletion finds its file through
/// membership even after the summary row was purged, and `delete_execution`
/// with another reason on a run retention deleted purges it instead of
/// returning the old tombstone.
pub async fn privacy_deletions_find_their_file_after_the_summary_purge<F: QueueFactory>(
    f: &F,
) -> Result<(), StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let both = || vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)];
    rec(&s, &mine, &mine, 0, 1, both(), None).await?;
    s.delete_execution(&catalog::id(1), RETENTION_REASON)
        .await?;
    s.delete_execution(&catalog::id(2), RETENTION_REASON)
        .await?;
    assert_eq!(s.purge_summaries(&mine, 11, 10).await?, 2);
    assert!(s.stale_archives(&mine, NOW, 10).await?.is_empty());
    let purged = s
        .purge_retained_run(&mine, &catalog::id(1), "privacy request")
        .await?
        .expect("retained");
    assert_eq!(purged.reason, "privacy request");
    assert_eq!(
        s.stale_archives(&mine, NOW, 10).await?.len(),
        1,
        "the file is found through its membership"
    );
    // The same through delete_execution: never a silent no-op.
    let legal = s
        .delete_execution(&catalog::id(2), "legal requirement")
        .await?;
    assert_eq!(legal.reason, "legal requirement");
    assert_eq!(
        s.tombstone(&catalog::id(2)).await?.unwrap().reason,
        "legal requirement"
    );
    // A rewrite may list neither any more.
    assert_eq!(
        rec(&s, &mine, &mine, 0, 2, both(), Some(digest(0, 1))).await,
        Err(StoreError::InvalidBatch("archive.run_gone"))
    );
    Ok(())
}

/// Re-check finding 3: a record lists exactly the run's finalized evidence
/// or is stale, and never clears a mark that landed after the writer read
/// the row.
pub async fn a_record_never_clears_a_later_mark_or_misses_evidence<F>(
    f: &F,
) -> Result<(), StoreError>
where
    F: QueueFactory,
    F::Store: ArchiveIndex + UploadStore,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let rb = projection().runbook_id;
    upload(&s, 1, true).await?;
    let mut run1 = listed(catalog::id(1), 0, 10);
    run1.revision = 2;
    // Built before the finalize: the evidence is missing, so it is stale.
    rec(&s, &mine, &mine, 0, 1, vec![run1.clone()], None).await?;
    assert!(s.get_archive(&mine, &rb, 0).await?.unwrap().stale);
    run1.evidence = vec![crate::uploads::evd(1)];
    rec(
        &s,
        &mine,
        &mine,
        0,
        2,
        vec![run1.clone()],
        Some(digest(0, 1)),
    )
    .await?;
    let read = s.get_archive(&mine, &rb, 0).await?.unwrap();
    assert!(!read.stale);
    // A mark after the writer read the row (count `read.stale_marks`).
    assert!(s.mark_archive_stale(&mine, &rb, 0).await?);
    s.put(day_key(0, digest(0, 3)), &file_bytes(0, 3)).await?;
    let mut record = day_record(&mine, 0, 3);
    record.stale_marks = read.stale_marks;
    s.record_archive(&mine, record, vec![run1], Some(digest(0, 2)))
        .await?;
    let after = s.get_archive(&mine, &rb, 0).await?.unwrap();
    assert_eq!(after.digest, digest(0, 3));
    assert!(after.stale, "the later mark survives the record");
    Ok(())
}

/// Re-check finding 4: retained evidence keeps counting toward the
/// organization's storage until its file expires.
pub async fn retained_evidence_counts_until_its_file_expires<F>(f: &F) -> Result<(), StoreError>
where
    F: QueueFactory,
    F::Store: ArchiveIndex + UploadStore,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let rb = projection().runbook_id;
    upload(&s, 1, true).await?; // counts 3 bytes once finalized
    let mut run1 = listed(catalog::id(1), 0, 10);
    run1.revision = 2;
    run1.evidence = vec![crate::uploads::evd(1)];
    rec(&s, &mine, &mine, 0, 1, vec![run1], None).await?;
    let now = s.upload_time_ms().await?;
    let mut next = crate::uploads::slot(&mine, 2, now);
    next.execution_id = catalog::id(2);
    // Room for the new 1024-byte slot only if the 3 retained bytes are free.
    let quota = UploadQuota {
        storage_bytes: Some(1024 + 2),
        ..Default::default()
    };
    let full = Err(StoreError::InvalidBatch("upload.limit_reached"));
    assert_eq!(s.reserve_evidence(&mine, next.clone(), &quota).await, full);
    s.delete_execution(&catalog::id(1), RETENTION_REASON)
        .await?;
    assert_eq!(
        s.reserve_evidence(&mine, next.clone(), &quota).await,
        full,
        "retention keeps the bytes counted"
    );
    assert!(s.delete_archive(&mine, &rb, 0, digest(0, 1)).await?);
    assert!(
        s.reserve_evidence(&mine, next, &quota).await?,
        "file expiry releases them"
    );
    // The summary row outlives its file (summary_days may exceed
    // archive_days); only purge_summaries or a single-run deletion drop it.
    assert!(s.archived_run(&mine, &catalog::id(1)).await?.is_some());
    assert!(s.file_members(&mine, &rb, 0).await?.is_empty());
    Ok(())
}

/// Re-check finding 6: deleting a file deletes every upload under its
/// retained runs, found by key (the upload, UP2's processed image and
/// thumbnail, and anything the manifest does not list).
pub async fn expiry_deletes_every_upload_under_its_retained_runs<F>(f: &F) -> Result<(), StoreError>
where
    F: QueueFactory,
    F::Store: ArchiveIndex + UploadStore,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    let mine = scope(1);
    let rb = projection().runbook_id;
    let listed_key = upload(&s, 1, true).await?;
    let evd = crate::uploads::evd(1);
    let mut keys = vec![listed_key];
    for evd_id in [
        runbook_core::uploads::processed_id(&evd),
        runbook_core::uploads::thumbnail_id(&evd),
        crate::uploads::evd(9),
    ] {
        let key = BlobKey::Upload {
            execution_id: catalog::id(1),
            evd_id,
        };
        s.put(key.clone(), b"derived").await?;
        keys.push(key);
    }
    let mut run1 = listed(catalog::id(1), 0, 10);
    run1.revision = 2;
    run1.evidence = vec![evd];
    rec(
        &s,
        &mine,
        &mine,
        0,
        1,
        vec![run1, listed(catalog::id(2), 0, 10)],
        None,
    )
    .await?;
    s.delete_execution(&catalog::id(1), RETENTION_REASON)
        .await?;
    for key in &keys {
        assert!(s.head(key).await.is_ok(), "retention keeps {key:?}");
    }
    // Not while run 2 is live.
    assert_eq!(
        s.delete_archive(&mine, &rb, 0, digest(0, 1)).await,
        Err(StoreError::InvalidBatch("archive.member_live"))
    );
    s.delete_execution(&catalog::id(2), RETENTION_REASON)
        .await?;
    // A leftover version goes with the file too.
    s.put(day_key(0, digest(0, 4)), &file_bytes(0, 4)).await?;
    assert!(s.delete_archive(&mine, &rb, 0, digest(0, 1)).await?);
    for key in &keys {
        assert_eq!(s.head(key).await, Err(StoreError::NotFound), "{key:?}");
    }
    assert!(
        s.list_prefix(&BlobPrefix::Archive {
            runbook_id: rb.clone(),
            day: 0
        })
        .await?
        .is_empty()
    );
    Ok(())
}

/// For adapter-specific tests: the seeded runs 1 and 2 (`retention::seed`),
/// their runbook, and file 1 of day 0 listing both.
pub async fn seeded_day<F: QueueFactory>(f: &F) -> Result<F::Store, StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    rec(&s, &scope(1), &scope(1), 0, 1, both_runs(), None).await?;
    Ok(s)
}
/// For adapter-specific tests: the seeded runs 1 and 2 and their runbook,
/// with no file for day 0 yet.
pub async fn seeded_runs<F: QueueFactory>(f: &F) -> Result<F::Store, StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = retention::seed(f).await?;
    own_runbook(&s).await?;
    Ok(s)
}
/// Store file `n` of day 0 and record it as the day's first file.
pub async fn record_first<S: ArchiveIndex + BlobStore>(s: &S, n: u8) -> Result<(), StoreError> {
    s.put(day_key(0, digest(0, n)), &file_bytes(0, n)).await?;
    s.record_archive(&scope(1), day_record(&scope(1), 0, n), both_runs(), None)
        .await
}
/// Store file `n` of day 0 without recording it (a losing write's bytes).
pub async fn put_version<S: BlobStore>(s: &S, n: u8) -> Result<(), StoreError> {
    s.put(day_key(0, digest(0, n)), &file_bytes(0, n)).await
}
/// Whether file `n` of day 0 is stored.
pub async fn version_stored<S: BlobStore>(s: &S, n: u8) -> Result<bool, StoreError> {
    match s.head(&day_key(0, digest(0, n))).await {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound) => Ok(false),
        Err(error) => Err(error),
    }
}
/// Reserve an upload slot on run `n` (pending, not finalized).
pub async fn pending_upload<S: BlobStore + UploadStore>(s: &S, n: u8) -> Result<(), StoreError> {
    upload(s, n, false).await.map(|_| ())
}
/// Finalize the slot `pending_upload(n)` reserved.
pub async fn finalize_upload<S: UploadStore>(s: &S, n: u8) -> Result<(), StoreError> {
    s.finalize_evidence(
        &scope(1),
        &crate::uploads::evd(n),
        UploadOutcome::Verified(VerifiedUpload {
            size: 3,
            content_type: "image/png".into(),
            width: None,
            height: None,
            thumbnail: false,
            thumbnail_bytes: 0,
        }),
        &UploadQuota::default(),
    )
    .await
    .map(|_| ())
}
/// Runs 1 and 2 as day 0's file lists them.
pub fn both_runs() -> Vec<ArchivedRun> {
    vec![listed(catalog::id(1), 0, 10), listed(catalog::id(2), 0, 10)]
}
/// Store file `n` of day 0 and record it, expecting file `previous`, with
/// the stale-mark count read before.
pub async fn record_file<S: ArchiveIndex + BlobStore>(
    s: &S,
    n: u8,
    previous: u8,
    marks: u64,
) -> Result<(), StoreError> {
    s.put(day_key(0, digest(0, n)), &file_bytes(0, n)).await?;
    let mut record = day_record(&scope(1), 0, n);
    record.stale_marks = marks;
    s.record_archive(&scope(1), record, both_runs(), Some(digest(0, previous)))
        .await
}
/// The digest of file `n` of day 0.
pub fn day0_digest(n: u8) -> [u8; 32] {
    digest(0, n)
}

/// Re-check 3, low finding: a folder the writer built from the live run is
/// refused once retention has deleted the run (its report would be
/// rendered from a run that is gone); the writer copies the stored folder.
pub async fn a_live_built_folder_is_refused_for_a_retained_run<F: QueueFactory>(
    f: &F,
) -> Result<(), StoreError>
where
    F::Store: ArchiveIndex,
{
    let s = seeded_day(f).await?;
    let mine = scope(1);
    s.delete_execution(&catalog::id(1), RETENTION_REASON)
        .await?;
    let lagging = || {
        let mut runs = both_runs();
        runs[0].gathered_live = true;
        runs
    };
    s.put(day_key(0, digest(0, 2)), &file_bytes(0, 2)).await?;
    assert_eq!(
        s.record_archive(
            &mine,
            day_record(&mine, 0, 2),
            lagging(),
            Some(digest(0, 1))
        )
        .await,
        Err(StoreError::InvalidBatch("archive.conflict"))
    );
    s.record_archive(
        &mine,
        day_record(&mine, 0, 2),
        both_runs(),
        Some(digest(0, 1)),
    )
    .await?;
    Ok(())
}
