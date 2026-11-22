//! UploadStore and upload-blob cases (PRD §14, D49). Registered red at T0;
//! UP1 makes them pass on both adapters.
use crate::{BlobFactory, QueueFactory, StoreFactory, execution::projection, scope};
use runbook_core::{
    StoreError,
    blob::{BlobHead, BlobKey, BlobPrefix, BlobStore},
    catalog::TenantScope,
    uploads::{
        EvidenceRecord, EvidenceState, UploadOutcome, UploadQuota, UploadStore, VerifiedUpload,
    },
};
use runbook_model::EvidenceId;

pub(crate) fn evd(n: u8) -> EvidenceId {
    format!("evd_{n:026}").parse().unwrap()
}
pub(crate) fn slot(scope: &TenantScope, n: u8, at: i64) -> EvidenceRecord {
    EvidenceRecord {
        id: evd(n),
        scope: scope.clone(),
        execution_id: projection().execution_id,
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        name: format!("screenshot-{n}.png"),
        declared_type: "image/png".into(),
        declared_size: 1024,
        state: EvidenceState::Pending,
        verified: None,
        created_by: "human:usr_00000000000000000000000001".into(),
        created_at_ms: at,
        url_expires_at_ms: at + runbook_core::uploads::UPLOAD_URL_TTL_MS,
        finalized_at_ms: None,
    }
}

/// Limits are checked with the insert, a retry is idempotent, and a slot
/// finalizes once; another tenant sees nothing.
pub async fn slots_count_limits_and_finalize_once<F: StoreFactory<Store: UploadStore>>(
    f: &F,
) -> Result<(), StoreError> {
    let u = f.create(projection()).await?;
    let (mine, theirs) = (scope(1), scope(2));
    let now = u.upload_time_ms().await?;
    let quota = UploadQuota {
        images_per_step_run: Some(1),
        ..Default::default()
    };
    assert!(
        u.reserve_evidence(&mine, slot(&mine, 1, now), &quota)
            .await?
    );
    assert!(
        !u.reserve_evidence(&mine, slot(&mine, 1, now), &quota)
            .await?
    );
    assert_eq!(
        u.reserve_evidence(&mine, slot(&mine, 2, now), &quota).await,
        Err(StoreError::InvalidBatch("upload.limit_reached"))
    );
    assert_eq!(
        u.reserve_evidence(&theirs, slot(&theirs, 3, now), &UploadQuota::default())
            .await,
        Err(StoreError::NotFound)
    );
    assert_eq!(u.get_evidence(&theirs, &evd(1)).await?, None);
    let verified = VerifiedUpload {
        size: 1024,
        content_type: "image/png".into(),
        width: Some(10),
        height: Some(10),
        thumbnail: true,
        thumbnail_bytes: 0,
    };
    let done = u
        .finalize_evidence(
            &mine,
            &evd(1),
            UploadOutcome::Verified(verified.clone()),
            &UploadQuota::default(),
        )
        .await?;
    assert_eq!(done.state, EvidenceState::Finalized);
    assert_eq!(done.verified, Some(verified));
    assert_eq!(
        u.finalize_evidence(
            &mine,
            &evd(1),
            UploadOutcome::Rejected {
                code: "upload.type_mismatch".into()
            },
            &UploadQuota::default()
        )
        .await,
        Err(StoreError::InvalidBatch("upload.not_pending"))
    );
    let listed = u.list_evidence(&mine, &projection().execution_id).await?;
    assert_eq!(listed.iter().map(|e| &e.id).collect::<Vec<_>>(), [&evd(1)]);
    Ok(())
}

/// Upload bytes live under their run's prefix, answer `head` and ranges,
/// and are invisible to another tenant.
pub async fn upload_blobs_support_head_and_ranges<F: BlobFactory>(f: &F) -> Result<(), StoreError> {
    let s = f.blobs(projection()).await?;
    let foreign = f.foreign_blobs(&s, scope(2)).await?;
    let key = BlobKey::Upload {
        execution_id: projection().execution_id,
        evd_id: evd(1),
    };
    let bytes: Vec<u8> = (0..=255).collect();
    s.put(key.clone(), &bytes).await?;
    assert_eq!(s.head(&key).await?, BlobHead { size: 256 });
    assert_eq!(s.get_range(&key, 2..5).await?, [2, 3, 4]);
    assert_eq!(s.get_range(&key, 250..256).await?, bytes[250..]);
    assert_eq!(
        s.get_range(&key, 200..257).await,
        Err(StoreError::InvalidBatch("blob.invalid_range"))
    );
    assert_eq!(foreign.head(&key).await, Err(StoreError::NotFound));
    assert_eq!(foreign.get(&key).await, Err(StoreError::NotFound));
    assert!(
        s.list_prefix(&BlobPrefix::Execution(projection().execution_id))
            .await?
            .contains(&key)
    );
    Ok(())
}

/// UP1: a range may span chunk boundaries; only its bytes come back, and an
/// empty range at the end is allowed.
pub async fn upload_ranges_cross_chunk_boundaries<F: BlobFactory>(f: &F) -> Result<(), StoreError> {
    use runbook_core::blob::BLOB_CHUNK_BYTES;
    let s = f.blobs(projection()).await?;
    let key = BlobKey::Upload {
        execution_id: projection().execution_id,
        evd_id: evd(2),
    };
    let size = BLOB_CHUNK_BYTES * 2 + BLOB_CHUNK_BYTES / 2;
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    s.put(key.clone(), &bytes).await?;
    let size = size as u64;
    assert_eq!(s.head(&key).await?, BlobHead { size });
    let chunk = BLOB_CHUNK_BYTES as u64;
    for range in [
        0..4,
        chunk - 3..chunk + 5,
        chunk..2 * chunk,
        chunk - 1..2 * chunk + 1,
        size - 7..size,
        size..size,
        0..size,
    ] {
        assert_eq!(
            s.get_range(&key, range.clone()).await?,
            bytes[range.start as usize..range.end as usize],
            "{range:?}"
        );
    }
    #[allow(clippy::reversed_empty_ranges)]
    let backwards = 5..4;
    assert_eq!(
        s.get_range(&key, backwards).await,
        Err(StoreError::InvalidBatch("blob.invalid_range"))
    );
    let absent = BlobKey::Upload {
        execution_id: projection().execution_id,
        evd_id: evd(3),
    };
    assert_eq!(s.head(&absent).await, Err(StoreError::NotFound));
    assert_eq!(s.get_range(&absent, 0..0).await, Err(StoreError::NotFound));
    Ok(())
}

/// UP1: the sweeper's query returns only this tenant's pending slots issued
/// before the cutoff, oldest first, up to the limit; deletion is per tenant.
pub async fn stale_slots_are_listed_and_deleted<F: StoreFactory<Store: UploadStore>>(
    f: &F,
) -> Result<(), StoreError> {
    let u = f.create(projection()).await?;
    let (mine, theirs) = (scope(1), scope(2));
    let now = u.upload_time_ms().await?;
    let day = runbook_core::uploads::UPLOAD_SLOT_SWEEP_MS;
    let none = UploadQuota::default();
    // 4 and 5 are old; 6 is fresh; 7 is old but finalized.
    for (n, at) in [
        (5, now - day - 1),
        (4, now - day - 2),
        (6, now),
        (7, now - day - 3),
    ] {
        assert!(u.reserve_evidence(&mine, slot(&mine, n, at), &none).await?);
    }
    u.finalize_evidence(
        &mine,
        &evd(7),
        UploadOutcome::Rejected {
            code: "upload.type_mismatch".into(),
        },
        &UploadQuota::default(),
    )
    .await?;
    let ids = |v: Vec<EvidenceRecord>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(
        ids(u.stale_evidence(&mine, now - day, 10).await?),
        [evd(4), evd(5)]
    );
    assert_eq!(ids(u.stale_evidence(&mine, now - day, 1).await?), [evd(4)]);
    assert!(u.stale_evidence(&theirs, now - day, 10).await?.is_empty());
    assert!(!u.delete_evidence(&theirs, &evd(4)).await?);
    assert!(u.delete_evidence(&mine, &evd(4)).await?);
    assert!(!u.delete_evidence(&mine, &evd(4)).await?);
    assert_eq!(u.get_evidence(&mine, &evd(4)).await?, None);
    assert_eq!(
        ids(u.list_evidence(&mine, &projection().execution_id).await?),
        [evd(7), evd(5), evd(6)]
    );
    Ok(())
}

/// UP1: limits count pending and finalized slots, never rejected ones;
/// files are per execution and storage is per organization.
pub async fn quotas_count_live_slots<F: StoreFactory<Store: UploadStore>>(
    f: &F,
) -> Result<(), StoreError> {
    let u = f.create(projection()).await?;
    let mine = scope(1);
    let now = u.upload_time_ms().await?;
    let files = UploadQuota {
        files_per_execution: Some(2),
        ..Default::default()
    };
    assert!(
        u.reserve_evidence(&mine, slot(&mine, 1, now), &files)
            .await?
    );
    u.finalize_evidence(
        &mine,
        &evd(1),
        UploadOutcome::Rejected {
            code: "upload.size_mismatch".into(),
        },
        &UploadQuota::default(),
    )
    .await?;
    assert!(
        u.reserve_evidence(&mine, slot(&mine, 2, now), &files)
            .await?
    );
    assert!(
        u.reserve_evidence(&mine, slot(&mine, 3, now), &files)
            .await?
    );
    assert_eq!(
        u.reserve_evidence(&mine, slot(&mine, 4, now), &files).await,
        Err(StoreError::InvalidBatch("upload.limit_reached"))
    );
    // Two live slots of 1024 bytes are stored; 2048 more would exceed 4096.
    let storage = UploadQuota {
        storage_bytes: Some(4096),
        ..Default::default()
    };
    let mut big = slot(&mine, 5, now);
    big.declared_size = 2049;
    assert_eq!(
        u.reserve_evidence(&mine, big.clone(), &storage).await,
        Err(StoreError::InvalidBatch("upload.limit_reached"))
    );
    big.declared_size = 2048;
    assert!(u.reserve_evidence(&mine, big, &storage).await?);
    // Images per step run ignore non-image slots.
    let images = UploadQuota {
        images_per_step_run: Some(3),
        ..Default::default()
    };
    let mut log = slot(&mine, 6, now);
    log.declared_type = "text/plain".into();
    assert!(u.reserve_evidence(&mine, log, &images).await?);
    assert_eq!(
        u.reserve_evidence(&mine, slot(&mine, 8, now), &images)
            .await,
        Err(StoreError::InvalidBatch("upload.limit_reached"))
    );
    // A different record under a taken ID is a duplicate, not a retry.
    let mut other = slot(&mine, 2, now);
    other.name = "other.png".into();
    assert_eq!(
        u.reserve_evidence(&mine, other, &UploadQuota::default())
            .await,
        Err(StoreError::InvalidBatch("upload.duplicate"))
    );
    Ok(())
}

/// UP2 fixes: storage counts what is stored once a slot is finalized (the
/// verified file and its thumbnail, not the declared size), and finalizing
/// re-checks it; rejected slots count nothing.
pub async fn storage_counts_stored_bytes<F: StoreFactory<Store: UploadStore>>(
    f: &F,
) -> Result<(), StoreError> {
    let u = f.create(projection()).await?;
    let mine = scope(1);
    let now = u.upload_time_ms().await?;
    let storage = UploadQuota {
        storage_bytes: Some(3000),
        ..Default::default()
    };
    for n in [1, 2] {
        assert!(
            u.reserve_evidence(&mine, slot(&mine, n, now), &storage)
                .await?
        );
    }
    let verified = |size, thumbnail_bytes| {
        UploadOutcome::Verified(VerifiedUpload {
            size,
            content_type: "image/png".into(),
            width: Some(10),
            height: Some(10),
            thumbnail: true,
            thumbnail_bytes,
        })
    };
    // 1024 declared + 1900 + 100 stored = 3024 > 3000: refused, unchanged.
    assert_eq!(
        u.finalize_evidence(&mine, &evd(1), verified(1900, 100), &storage)
            .await,
        Err(StoreError::InvalidBatch("upload.limit_reached"))
    );
    assert_eq!(
        u.get_evidence(&mine, &evd(1)).await?.map(|r| r.state),
        Some(EvidenceState::Pending)
    );
    // 1024 + 1800 + 100 = 2924 fits; the slot now counts 1900.
    let done = u
        .finalize_evidence(&mine, &evd(1), verified(1800, 100), &storage)
        .await?;
    assert_eq!(done.counted_bytes(), 1900);
    // 1900 + 1024 + 1024 > 3000.
    assert_eq!(
        u.reserve_evidence(&mine, slot(&mine, 3, now), &storage)
            .await,
        Err(StoreError::InvalidBatch("upload.limit_reached"))
    );
    // A rejected slot frees its share: 1900 + 1024 = 2924.
    u.finalize_evidence(
        &mine,
        &evd(2),
        UploadOutcome::Rejected {
            code: "upload.type_mismatch".into(),
        },
        &storage,
    )
    .await?;
    assert!(
        u.reserve_evidence(&mine, slot(&mine, 3, now), &storage)
            .await?
    );
    Ok(())
}

/// UP1: deleting a finished run removes its evidence slots and objects; a
/// sibling run keeps its own.
pub async fn run_deletion_removes_its_uploads<F>(f: &F) -> Result<(), StoreError>
where
    F: QueueFactory,
    F::Store: UploadStore,
{
    use runbook_core::retention::RunStore;
    let s = crate::retention::seed(f).await?;
    let mine = scope(1);
    let now = s.upload_time_ms().await?;
    let mut keys = Vec::new();
    for n in [1u8, 2] {
        let mut record = slot(&mine, n, now);
        record.execution_id = crate::catalog::id(n);
        assert!(
            s.reserve_evidence(&mine, record, &UploadQuota::default())
                .await?
        );
        let key = BlobKey::Upload {
            execution_id: crate::catalog::id(n),
            evd_id: evd(n),
        };
        s.put(key.clone(), &[n; 3]).await?;
        keys.push(key);
    }
    s.delete_execution(&crate::catalog::id(1), "retention")
        .await?;
    assert_eq!(s.get_evidence(&mine, &evd(1)).await?, None);
    assert_eq!(s.get(&keys[0]).await, Err(StoreError::NotFound));
    assert!(s.get_evidence(&mine, &evd(2)).await?.is_some());
    assert_eq!(s.get(&keys[1]).await?, vec![2; 3]);
    Ok(())
}

/// Test-only stand-in for UP3's magic-byte table: PNG and PDF.
pub fn stub_sniff(bytes: &[u8]) -> Option<runbook_core::uploads::verify::SniffedType> {
    use runbook_core::uploads::verify::SniffedType;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(SniffedType::Png)
    } else if bytes.starts_with(b"%PDF-") {
        Some(SniffedType::Pdf)
    } else {
        None
    }
}

const SHOT: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: shot, title: Shot}\n\
    spec: {steps: [{id: only, title: Only, evidence: {required: [screenshot]}}]}\n";

/// UP1 store test (later-plans rule 1): a step requiring a screenshot is
/// refused without one, refused with a PDF or a pending slot, and succeeds
/// with a finalized image; the committed log replays to the same files.
pub async fn reports_attach_finalized_uploads<S, D>(store: &S, dir: &D)
where
    S: runbook_core::start::StartStore
        + runbook_core::catalog::CatalogStore
        + runbook_core::work_queue::WorkQueue
        + runbook_core::session::SessionStore
        + runbook_core::ExecutionStore
        + runbook_core::claims::ClaimStore
        + BlobStore
        + UploadStore,
    D: runbook_core::directory::DirectoryStore
        + runbook_core::directory_operations::DirectoryOperations,
{
    use crate::portable::{agent, directory, exe, p, start};
    use runbook_core::{
        actor::begin_actor,
        claims::claim_step,
        uploads::{NewSlot, finalize, report_with_uploads, reserve},
    };
    use runbook_model::{
        ClaimRequest, CreateEvidenceRequest, ReportRequest, ReportStatus, StepStatus, UploadLimits,
    };
    directory(dir).await;
    crate::parity::publish(store, 3, "shot", SHOT, 3).await;
    start(store, dir, &p(), 1, 3).await;
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let only = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "only")
        .and_then(|s| s.id.clone())
        .unwrap();
    let claim = claim_step(
        work,
        &agent(),
        &only,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    let limits = UploadLimits {
        image_types: vec!["image/png".into()],
        image_bytes: None,
        file_bytes: None,
        image_max_dimension: None,
        image_max_pixels: None,
        thumbnail_max_dimension: None,
        images_per_step_run: None,
        files_per_execution: None,
        storage_bytes: None,
        client_resize_long_edge: 2048,
    };
    let mut ids = Vec::new();
    for (n, ty, bytes) in [
        (11u8, "image/png", b"\x89PNG\r\n\x1a\n-image".to_vec()),
        (12, "application/pdf", b"%PDF-1.7 document".to_vec()),
        (13, "image/png", b"\x89PNG\r\n\x1a\n-never".to_vec()),
    ] {
        let slot = reserve(
            store,
            NewSlot {
                scope: &p(),
                execution_id: &exe(1),
                step_run_id: &only,
                id: evd(n),
                request: &CreateEvidenceRequest {
                    name: format!("file-{n}"),
                    content_type: ty.into(),
                    size: bytes.len() as u64,
                },
                limits: &limits,
                created_by: "service:runner",
            },
        )
        .await
        .unwrap();
        if n != 13 {
            store
                .put(runbook_core::uploads::object_key(&slot), &bytes)
                .await
                .unwrap();
            finalize(store, &p(), &only, &slot.id, stub_sniff)
                .await
                .unwrap();
        }
        ids.push(slot.id.to_string());
    }
    let report = |evidence: Vec<String>| ReportRequest {
        step_run_id: only.clone(),
        claim: claim.clone(),
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Default::default(),
        summary: None,
        evidence,
        logs: vec![],
        cost: None,
    };
    for (evidence, code) in [
        (vec![], "step.evidence_required"),
        (vec![ids[1].clone()], "step.evidence_required"),
        (vec![ids[2].clone()], "evidence.not_finalized"),
        (vec![evd(14).to_string()], "evidence.not_found"),
    ] {
        let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
        let refused = report_with_uploads(store, work, &agent(), &report(evidence), None).await;
        assert_eq!(refused.unwrap_err().code(), code);
    }
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    report_with_uploads(
        store,
        work,
        &agent(),
        &report(vec![ids[0].clone(), ids[1].clone()]),
        None,
    )
    .await
    .unwrap();
    // A fresh replay of the stored log carries the files and their types.
    let state = begin_actor(store, dir, p(), &exe(1))
        .await
        .unwrap()
        .runtime
        .state;
    let step = state.steps.iter().find(|s| s.step_id == "only").unwrap();
    assert_eq!(step.status, StepStatus::Succeeded);
    assert_eq!(step.evidence, [ids[0].clone(), ids[1].clone()]);
    assert_eq!(
        step.files
            .iter()
            .map(|f| (f.id.to_string(), f.content_type.as_str()))
            .collect::<Vec<_>>(),
        [
            (ids[0].clone(), "image/png"),
            (ids[1].clone(), "application/pdf")
        ]
    );
}

const BUILD: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: build, title: Build}\n\
    spec: {steps: [{id: only, title: Only, evidence: {required: [log]}}]}\n";

/// CF12b review round 2 (cross-store): required `log` evidence is met by a
/// non-blank line appended through the logs route, not only by lines the
/// report carries. Every store counts alike: blank and whitespace-only
/// lines (Unicode whitespace too) do not count, a line that moved to R2 on
/// Cloudflare does, and the event records it so replay agrees.
pub async fn appended_logs_satisfy_log_evidence<S, D>(store: &S, dir: &D)
where
    S: runbook_core::start::StartStore
        + runbook_core::catalog::CatalogStore
        + runbook_core::work_queue::WorkQueue
        + runbook_core::session::SessionStore
        + runbook_core::ExecutionStore
        + runbook_core::claims::ClaimStore
        + BlobStore
        + UploadStore,
    D: runbook_core::directory::DirectoryStore
        + runbook_core::directory_operations::DirectoryOperations,
{
    use crate::portable::{agent, directory, exe, p, start};
    use runbook_core::{
        actor::begin_actor,
        claims::{append_logs, claim_step},
        uploads::report_with_uploads,
    };
    use runbook_model::{ClaimRequest, LogsRequest, ReportRequest, ReportStatus, StepStatus};
    directory(dir).await;
    crate::parity::publish(store, 3, "build", BUILD, 3).await;
    start(store, dir, &p(), 1, 3).await;
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let only = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "only")
        .and_then(|s| s.id.clone())
        .unwrap();
    let claim = claim_step(
        work,
        &agent(),
        &only,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    let append = async |logs: Vec<String>| {
        append_logs(
            store,
            &agent(),
            &exe(1),
            &only,
            &LogsRequest {
                claim: claim.clone(),
                logs,
            },
            None,
        )
        .await
        .unwrap()
    };
    let report = ReportRequest {
        step_run_id: only.clone(),
        claim: claim.clone(),
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Default::default(),
        summary: None,
        evidence: vec![],
        logs: vec![],
        cost: None,
    };
    let attempt = async || {
        let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
        report_with_uploads(store, work, &agent(), &report, None).await
    };
    assert_eq!(
        attempt().await.unwrap_err().code(),
        "step.evidence_required"
    );
    append(vec![String::new(), "   ".into(), "\u{3000}\t\u{a0}".into()]).await;
    assert_eq!(
        attempt().await.unwrap_err().code(),
        "step.evidence_required"
    );
    // One non-blank line, then over 64 KiB of blank text: on Cloudflare the
    // line moves to R2 in a chunk before the report reads it.
    append(vec!["built in 3s".into(), " ".repeat(70_000)]).await;
    attempt().await.unwrap();
    let state = begin_actor(store, dir, p(), &exe(1))
        .await
        .unwrap()
        .runtime
        .state;
    let step = state.steps.iter().find(|s| s.step_id == "only").unwrap();
    assert_eq!(step.status, StepStatus::Succeeded);
    assert!(step.logs.is_empty());
}
