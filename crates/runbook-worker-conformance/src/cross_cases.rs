//! CF5: the composed Cloudflare store's own cases (beyond the shared
//! suite), on any cross host: natively over the in-process protocol, in
//! workerd over real Durable Objects, D1 and R2.
use crate::cross::{CrossFactory, CrossHost};
use runbook_core::{
    ExecutionStore, StoreError,
    blob::{BlobKey, BlobPrefix, BlobStore},
    claims::ClaimStore,
};
use runbook_model::{ActorRef, ClaimRef};
use runbook_store_conformance::{StoreFactory, execution};
use runbook_worker::{compose::Objects, object::ObjectHandle};

fn check(ok: bool, what: &'static str) -> Result<(), StoreError> {
    if ok {
        Ok(())
    } else {
        Err(StoreError::Backend(what))
    }
}

/// CF4-8: a step's log lines move to R2 in 64 KiB chunks after the
/// object's transaction; reads stitch the chunks and the inline tail with
/// unchanged sequence numbers; the 5 MiB cap counts moved lines; recording
/// a chunk twice is harmless and a stale chunk is refused.
pub async fn step_logs_move_to_r2_in_chunks<H: CrossHost>(
    f: &CrossFactory<H>,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    let id = execution::projection().execution_id;
    let t0 = f.host().start_ms();
    let mut b = crate::cases::step(t0, 1);
    b.claims = vec![crate::cases::claim(Some(t0 + 600_000))];
    s.commit(b).await?;
    let c = crate::cases::claim(None);
    let sr = c.step_run_id.clone();
    let reference = ClaimRef {
        id: c.claim_id.clone(),
        generation: std::num::NonZeroU64::new(1).ok_or(StoreError::Backend("test"))?,
    };
    // 200 lines of 1,000 bytes: three full chunks (65 lines each) move,
    // five lines stay inline.
    let lines: Vec<String> = (0..200)
        .map(|n| format!("{n:04}{}", "x".repeat(996)))
        .collect();
    let last = s
        .append_logs(&id, &sr, &reference, &ActorRef::System, lines.clone(), None)
        .await?;
    check(last == 200, "test.last_seq")?;
    let object = s.objects().object(&id, s.scope())?;
    let counts: std::collections::BTreeMap<String, u64> =
        object.table_counts().await?.into_iter().collect();
    check(counts.get("log_chunk") == Some(&3), "test.three_chunks")?;
    check(counts.get("claim_log") == Some(&5), "test.inline_tail")?;
    let keys = s.list_prefix(&BlobPrefix::Execution(id.clone())).await?;
    for chunk in 0..3 {
        let key = BlobKey::Execution {
            execution_id: id.clone(),
            name: runbook_store_do::logs::chunk_name(&sr, chunk),
        };
        check(keys.contains(&key), "test.chunk_in_r2")?;
    }
    // Every line, in order, with its sequence number.
    let all = s.read_logs(&id, &sr, 0, 1_000).await?;
    let expected: Vec<(u64, String)> = (1..=200).zip(lines.iter().cloned()).collect();
    check(all == expected, "test.stitched_read")?;
    // A page across a chunk boundary (chunk 0 ends at 65).
    let page = s.read_logs(&id, &sr, 60, 10).await?;
    check(
        page.iter().map(|(seq, _)| *seq).collect::<Vec<_>>() == (61..=70).collect::<Vec<_>>(),
        "test.page_across_chunks",
    )?;
    // A page that starts in the tail.
    let tail = s.read_logs(&id, &sr, 198, 10).await?;
    check(
        tail.iter().map(|(seq, _)| *seq).collect::<Vec<_>>() == vec![199, 200],
        "test.tail_page",
    )?;
    // Numbering continues past moved lines.
    let next = s
        .append_logs(
            &id,
            &sr,
            &reference,
            &ActorRef::System,
            vec!["more".into()],
            None,
        )
        .await?;
    check(next == 201, "test.numbering_continues")?;
    // The cap counts moved lines: this line fits beside the inline tail
    // alone but not beside the 195,000 bytes in R2.
    let huge = "x".repeat(5 * 1024 * 1024 - 199_000);
    check(
        s.append_logs(&id, &sr, &reference, &ActorRef::System, vec![huge], None)
            .await
            == Err(StoreError::InvalidBatch("step.logs_too_large")),
        "test.cap_counts_chunks",
    )?;
    // Recording a chunk again is a no-op; a chunk whose lines are gone is
    // refused.
    let chunks = object.log_chunks(&id, &sr, 0).await?;
    check(chunks.len() == 3, "test.chunk_rows")?;
    check(
        !object.log_flushed(&id, &sr, &chunks[0]).await?,
        "test.repeat_flush",
    )?;
    let mut stale = chunks[0].clone();
    stale.chunk = 9;
    check(
        object.log_flushed(&id, &sr, &stale).await
            == Err(StoreError::InvalidBatch("log.flush_stale")),
        "test.stale_flush",
    )?;
    Ok(())
}

/// CLI3 re-check N9: a chunk put in R2 before a deploy that changed how
/// chunk sizes count (blank lines went from zero bytes to one), whose
/// `log_flushed` never landed (refused as stale by the new object, or the
/// host stopped), sits under the key the new code computes a different
/// chunk for. R2 keys are immutable, so the new chunk's put is refused;
/// the chunk already there is recorded instead (its lines are still the
/// oldest inline ones), and the flushes continue. Nothing is lost or read
/// twice.
pub async fn a_log_chunk_put_before_a_deploy_is_recorded<H: CrossHost>(
    f: &CrossFactory<H>,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    let id = execution::projection().execution_id;
    let t0 = f.host().start_ms();
    let mut b = crate::cases::step(t0, 1);
    b.claims = vec![crate::cases::claim(Some(t0 + 600_000))];
    s.commit(b).await?;
    let c = crate::cases::claim(None);
    let sr = c.step_run_id.clone();
    let reference = ClaimRef {
        id: c.claim_id.clone(),
        generation: std::num::NonZeroU64::new(1).ok_or(StoreError::Backend("test"))?,
    };
    let big = |n: usize| format!("{n:04}{}", "x".repeat(996));
    // 60 lines of 1,000 bytes and 2,000 blank lines: 62,000 bytes now,
    // below a chunk, so they stay inline.
    let mut lines: Vec<String> = (0..60).map(big).collect();
    lines.extend(std::iter::repeat_n(String::new(), 2000));
    // Then 140 more lines of 1,000 bytes.
    let more: Vec<String> = (60..200).map(big).collect();
    for part in lines.chunks(500) {
        s.append_logs(&id, &sr, &reference, &ActorRef::System, part.to_vec(), None)
            .await?;
    }
    let object = s.objects().object(&id, s.scope())?;
    check(
        object.log_chunks(&id, &sr, 0).await?.is_empty(),
        "test.no_chunk_yet",
    )?;
    // The chunk the earlier code computed (blank lines counted as zero:
    // 65 lines of 1,000 bytes and the blanks), put before the deploy.
    let mut old_chunk = lines.clone();
    old_chunk.extend(more[..5].iter().cloned());
    BlobStore::put(
        &s,
        BlobKey::Execution {
            execution_id: id.clone(),
            name: runbook_store_do::logs::chunk_name(&sr, 0),
        },
        &serde_json::to_vec(&old_chunk).map_err(|_| StoreError::Backend("test"))?,
    )
    .await?;
    // Appends after the deploy flush with today's boundaries (63 lines of
    // 1,000 bytes and the blanks): the put of chunk 0 is refused, and the
    // chunk already there is recorded.
    s.append_logs(&id, &sr, &reference, &ActorRef::System, more.clone(), None)
        .await?;
    let chunks = object.log_chunks(&id, &sr, 0).await?;
    check(chunks.len() == 3, "test.three_chunks")?;
    check(
        (chunks[0].first_seq, chunks[0].last_seq) == (1, 2065),
        "test.the_old_chunk_is_recorded",
    )?;
    check(
        chunks[0].bytes == 65 * 1000 + 2000,
        "test.recorded_with_todays_measure",
    )?;
    check(
        (chunks[1].first_seq, chunks[2].last_seq) == (2066, 2195),
        "test.later_chunks_follow",
    )?;
    let all = s.read_logs(&id, &sr, 0, 10_000).await?;
    let expected: Vec<(u64, String)> = (1..)
        .zip(lines.iter().chain(more.iter()).cloned())
        .collect();
    check(all == expected, "test.every_line_once_in_order")?;
    Ok(())
}

/// CF6: the OrgDO reservation is idempotent and compensated. A start whose
/// D1 batch never committed (and whose refund was lost) is refunded by the
/// sweep; admitted runs' reservations are forgotten (counted for good); a
/// queued run keeps its reservation until its cancellation refunds it; a
/// start refused in D1 is refunded at once.
pub async fn quota_reservations_are_compensated<H: CrossHost>(
    f: &CrossFactory<H>,
) -> Result<(), StoreError> {
    use runbook_core::{
        catalog::CatalogStore,
        quotas::{QuotaLimits, QuotaStore},
        start::StartStore,
    };
    use runbook_store_conformance::{SessionFactory, catalog, quotas::start, scope};
    use runbook_worker::quota::{QuotaHandle, sweep};
    let s = f
        .create(execution::projection())
        .await?
        .with_quota(QuotaLimits {
            executions_monthly: Some(10),
        });
    let org = scope(1).org_id;
    let db = f.db()?;
    let now = runbook_store_d1::D1::now_ms(&db);
    let month = runbook_core::quotas::month_of(now);
    // A start that reserved and then lost its D1 batch and its refund.
    let lost = catalog::id(50);
    s.org_quota()
        .reserve(&lost, &month, now, QuotaLimits::default())
        .await?;
    s.commit_start(start(51, None, None)).await?;
    s.commit_start(start(52, None, Some("p"))).await?;
    s.commit_start(start(53, None, Some("p"))).await?;
    check(
        s.org_usage(&org).await?.executions_started == 4,
        "test.four_reserved",
    )?;
    // A start D1 refuses (its run ID is taken by another tenant's run) is
    // refunded by the start itself.
    let taken = f.scoped_client(&s, scope(2)).await?;
    taken.commit_start(start(54, None, None)).await?;
    check(
        s.commit_start(start(54, None, None)).await.is_err(),
        "test.foreign_id_refused",
    )?;
    check(
        s.org_usage(&org).await?.executions_started == 4,
        "test.refused_refunded",
    )?;
    // Nothing is judged inside the grace period.
    let quiet = sweep(
        &db,
        &s.org_quota(),
        now - runbook_worker::quota::SWEEP_GRACE_MS,
        100,
    )
    .await?;
    check(quiet == Default::default(), "test.grace")?;
    // An hour on: the lost start is refunded, admitted runs settled, the
    // waiter kept.
    f.advance_clock(&s, 2 * 3_600_000).await?;
    let later = runbook_store_d1::D1::now_ms(&db);
    let swept = sweep(
        &db,
        &s.org_quota(),
        later - runbook_worker::quota::SWEEP_GRACE_MS,
        100,
    )
    .await?;
    check(swept.refunded == 1, "test.lost_refunded")?;
    check(swept.settled == 2, "test.admitted_settled")?;
    check(
        s.org_usage(&org).await?.executions_started == 3,
        "test.count_after_sweep",
    )?;
    check(
        s.org_quota().reservations_before(later, 100).await? == vec![catalog::id(53)],
        "test.waiter_kept",
    )?;
    // The waiter's cancellation still refunds it.
    check(
        s.cancel_queued(&catalog::id(53), None).await?,
        "test.waiter_cancelled",
    )?;
    check(
        s.org_usage(&org).await?.executions_started == 2,
        "test.waiter_refunded",
    )?;
    check(
        s.admission(&catalog::id(52)).await?.is_some(),
        "test.holder",
    )?;
    Ok(())
}

/// Gate fix (low): the sweep also refunds runs that never ran but whose
/// own refund was lost: a queued start released before admission (its
/// admission row released with no admission time), and a start the
/// reconciler abandoned (its tombstone's reason `start.abandoned`). Both
/// were counted for good before.
pub async fn quota_sweep_refunds_runs_that_never_ran<H: CrossHost>(
    f: &CrossFactory<H>,
) -> Result<(), StoreError> {
    use runbook_core::{
        quotas::{QuotaLimits, QuotaStore},
        start::StartStore,
    };
    use runbook_store_conformance::{SessionFactory, catalog, quotas::start, scope};
    use runbook_worker::quota::{QuotaHandle, sweep};
    let s = f
        .create(execution::projection())
        .await?
        .with_quota(QuotaLimits {
            executions_monthly: Some(10),
        });
    let org = scope(1).org_id;
    let db = f.db()?;
    let now = runbook_store_d1::D1::now_ms(&db);
    let month = runbook_core::quotas::month_of(now);
    // The pool's holder, then a waiter released before admission whose
    // refund was lost (the store's release without the refund).
    s.commit_start(start(61, None, Some("p"))).await?;
    s.commit_start(start(62, None, Some("p"))).await?;
    check(
        s.catalog().release_queued(&catalog::id(62)).await?,
        "test.waiter_released",
    )?;
    // A start abandoned by the reconciler, its refund lost.
    let abandoned = catalog::id(63);
    s.org_quota()
        .reserve(&abandoned, &month, now, QuotaLimits::default())
        .await?;
    runbook_store_d1::D1::run(
        &db,
        runbook_store_d1::prepare(
            "INSERT INTO tombstone (execution_id, org_id, project_id, body) \
             VALUES (?, ?, ?, '{\"reason\": \"start.abandoned\"}')",
        )
        .bind(abandoned.as_str())
        .bind(scope(1).org_id.as_str())
        .bind(scope(1).project_id.as_str()),
    )
    .await
    .map_err(|_| StoreError::Backend("test.tombstone"))?;
    check(
        s.org_usage(&org).await?.executions_started == 3,
        "test.three_reserved",
    )?;
    f.advance_clock(&s, 2 * 3_600_000).await?;
    let later = runbook_store_d1::D1::now_ms(&db);
    let swept = sweep(
        &db,
        &s.org_quota(),
        later - runbook_worker::quota::SWEEP_GRACE_MS,
        100,
    )
    .await?;
    check(swept.refunded == 2, "test.never_ran_refunded")?;
    check(swept.settled == 1, "test.holder_settled")?;
    check(
        s.org_usage(&org).await?.executions_started == 1,
        "test.count_after_sweep",
    )?;
    Ok(())
}
