//! DO-specific cases beyond the shared conformance suite: the outbox drain
//! contract, the alarm mapping, execution credentials, claim heartbeats and
//! logs, the execution-local start, migrations and tenancy. Each runs on any
//! [`Host`]: natively over SQLite and inside workerd over `SqlStorage`.
use crate::host::Host;
use runbook_core::{
    ClaimAction, ClaimSession, ClaimWrite, CommitResult, ExecutionCommit, ExecutionStore,
    StoreError, TimerChange, TimerRecord, WriteStage,
    catalog::{Admission, AdmissionState, DerivedUpdate, RunSummary},
    claims::ClaimStore,
    credentials::{TokenKind, secret_hash},
    execution_credentials::{
        CredentialFamily, CredentialWrite, ExecutionCredentialRecord, ExecutionCredentialStore,
    },
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{ActorRef, ExecutionStatus};
use runbook_store_conformance::{execution, scope};
use runbook_store_do::{
    DoSql, DoStore, Fault, NotificationCursor, OUTBOX_FIRST_RETRY_MS, OutboxStream, StartInit,
    next_alarm_at,
};

/// Acknowledge every pending notification through the last one's cursor.
fn ack_all_notifications<B: DoSql>(s: &DoStore<B>) -> Result<u64, StoreError> {
    match s.pending_notifications(None, 10_000)?.last() {
        Some(last) => s.ack_notifications(&NotificationCursor::of(last)),
        None => Ok(0),
    }
}

/// A fresh object holding the shared projection's head, clock at start.
async fn store<H: Host>(h: &H) -> (DoStore<H::Sql>, H::Sql) {
    let sql = h.fresh().expect("fresh object");
    h.set_time(&sql, h.start_ms());
    let store =
        DoStore::open(sql.clone(), &execution::projection().execution_id, scope(1)).expect("open");
    store
        .create_execution(execution::projection())
        .expect("head");
    (store, sql)
}

fn summary(sequence: u64, status: ExecutionStatus, completed: Option<i64>) -> RunSummary {
    let p = execution::projection();
    RunSummary {
        execution_id: p.execution_id,
        runbook_id: p.runbook_id,
        sequence,
        status,
        completed_at_ms: completed,
        title: "t".into(),
        tags: vec![],
        fields: Default::default(),
        created_by: "system".into(),
        created_at_ms: 0,
    }
}

/// The shared batch at `sequence`, with a derived row and one intent.
pub(crate) fn step(t0: i64, sequence: u64) -> ExecutionCommit {
    let mut b = execution::batch();
    b.expected_sequence = sequence - 1;
    b.projection.sequence = sequence;
    b.events[0].seq = sequence;
    b.events[0].at_ms = t0;
    b.outbox[0].sequence = sequence;
    b.outbox[0].at_ms = t0;
    b.outbox[0].event_id = format!("evt_{sequence}");
    b.claims.clear();
    b.idempotency.clear();
    b.timers.clear();
    b.derived_outbox = vec![DerivedUpdate {
        summary: summary(sequence, ExecutionStatus::Running, None),
        assignments: vec![],
        release_concurrency: false,
        memory: vec![],
    }];
    b
}

pub async fn derived_and_notification_streams_drain_in_order_and_ack_through<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    for n in 1..=3 {
        assert_eq!(
            (s.commit(step(t0, n))).await.unwrap(),
            CommitResult::Committed(n)
        );
    }
    let derived: Vec<u64> = s
        .pending_derived(None, 10)
        .unwrap()
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(derived, [1, 2, 3]);
    assert_eq!(
        s.pending_derived(Some(1), 10)
            .unwrap()
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(s.pending_derived(None, 1).unwrap().len(), 1);
    assert_eq!(s.ack_derived(2).unwrap(), 2);
    // Idempotent and cumulative: acking again or lower changes nothing.
    assert_eq!(s.ack_derived(2).unwrap(), 0);
    assert_eq!(s.ack_derived(1).unwrap(), 0);
    assert_eq!(
        s.pending_derived(None, 10)
            .unwrap()
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>(),
        [3]
    );
    assert_eq!(
        s.ack_derived(4),
        Err(StoreError::InvalidBatch("outbox.ack_beyond_head"))
    );
    // Acked rows remain stored with the run; only the pending view (and
    // the load, which keeps undelivered rows plus the latest) shrinks.
    let stored = s.table_counts()?;
    assert_eq!(
        stored
            .iter()
            .find(|(t, _)| *t == "derived_outbox")
            .map(|(_, n)| *n),
        Some(3)
    );
    let ids: Vec<String> = s
        .pending_notifications(None, 10)
        .unwrap()
        .into_iter()
        .map(|i| i.event_id)
        .collect();
    assert_eq!(ids, ["evt_1", "evt_2", "evt_3"]);
    assert_eq!(ack_all_notifications(&s).unwrap(), 3);
    assert!(s.pending_notifications(None, 10).unwrap().is_empty());
    Ok(())
}

pub async fn a_notification_acked_through_the_queue_leaves_the_drain<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    (s.commit(step(t0, 1))).await.unwrap();
    let lease = (s.claim_due(10, 60_000)).await.unwrap().remove(0);
    assert!(matches!(lease.id.kind, WorkKind::Notification { .. }));
    (s.ack(&lease.id, lease.generation)).await.unwrap();
    assert!(s.pending_notifications(None, 10).unwrap().is_empty());
    // And one drained by the cursor is no longer offered as work.
    (s.commit(step(t0, 2))).await.unwrap();
    ack_all_notifications(&s).unwrap();
    assert!((s.claim_due(10, 60_000)).await.unwrap().is_empty());
    Ok(())
}

pub async fn the_alarm_is_the_earliest_pending_work_and_follows_every_write<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    assert_eq!(
        h.alarm(&sql).await,
        None,
        "a fresh head has nothing to wake for"
    );
    let mut b = step(t0, 1);
    b.timers = vec![
        TimerChange::Upsert(TimerRecord {
            execution_id: b.execution_id.clone(),
            key: "a".into(),
            generation: 1,
            due_at_ms: t0 + 5_000,
        }),
        TimerChange::Upsert(TimerRecord {
            execution_id: b.execution_id.clone(),
            key: "b".into(),
            generation: 1,
            due_at_ms: t0 + 2_000,
        }),
    ];
    (s.commit(b)).await.unwrap();
    ack_all_notifications(&s).unwrap();
    assert_eq!(h.alarm(&sql).await, Some(t0 + 2_000), "earliest timer");
    assert_eq!(next_alarm_at(&s).unwrap(), h.alarm(&sql).await);
    // Leasing the due timer pushes it to the lease deadline; acking drops it.
    h.set_time(&sql, t0 + 2_000);
    let lease = (s.claim_due(1, 1_000)).await.unwrap().remove(0);
    assert!(matches!(&lease.id.kind, WorkKind::Timer { key, .. } if key == "b"));
    assert_eq!(h.alarm(&sql).await, Some(t0 + 3_000), "the lease deadline");
    (s.ack(&lease.id, lease.generation)).await.unwrap();
    assert_eq!(h.alarm(&sql).await, Some(t0 + 5_000), "the next timer");
    // Undelivered outbox entries wake the object for a retry after 10 s.
    h.set_time(&sql, t0 + 5_000);
    let lease = (s.claim_due(1, 60_000)).await.unwrap().remove(0);
    (s.ack(&lease.id, lease.generation)).await.unwrap();
    assert_eq!(
        h.alarm(&sql).await,
        Some(t0 + OUTBOX_FIRST_RETRY_MS),
        "the derived row's retry"
    );
    s.ack_derived(1).unwrap();
    assert_eq!(h.alarm(&sql).await, None, "nothing left to do");
    Ok(())
}

pub async fn a_failed_delivery_backs_its_stream_off_and_an_ack_resets_it<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    (s.commit(step(t0, 1))).await.unwrap();
    ack_all_notifications(&s).unwrap();
    h.set_time(&sql, t0 + OUTBOX_FIRST_RETRY_MS);
    assert_eq!(s.defer_outbox(OutboxStream::Derived).unwrap(), t0 + 20_000);
    assert_eq!(s.defer_outbox(OutboxStream::Derived).unwrap(), t0 + 30_000);
    assert_eq!(s.defer_outbox(OutboxStream::Derived).unwrap(), t0 + 50_000);
    assert_eq!(h.alarm(&sql).await, Some(t0 + 50_000));
    for _ in 0..20 {
        s.defer_outbox(OutboxStream::Derived).unwrap();
    }
    assert_eq!(
        h.alarm(&sql).await,
        Some(t0 + OUTBOX_FIRST_RETRY_MS + 600_000),
        "capped"
    );
    s.ack_derived(1).unwrap();
    assert_eq!(h.alarm(&sql).await, None);
    Ok(())
}

pub async fn a_failed_commit_rolls_back_rows_and_the_alarm<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let mut b = step(t0, 1);
    b.timers = vec![TimerChange::Upsert(TimerRecord {
        execution_id: b.execution_id.clone(),
        key: "t".into(),
        generation: 1,
        due_at_ms: t0 + 1_000,
    })];
    let before = s.table_counts().unwrap();
    for fault in [
        Fault::After(WriteStage::Events),
        Fault::After(WriteStage::DerivedRows),
        Fault::SqlConstraint,
        Fault::AfterAlarm,
    ] {
        s.inject(fault);
        assert!((s.commit(b.clone())).await.is_err(), "{fault:?}");
        assert_eq!(s.table_counts().unwrap(), before, "{fault:?}");
        assert_eq!(h.alarm(&sql).await, None, "{fault:?}");
    }
    assert_eq!((s.commit(b)).await.unwrap(), CommitResult::Committed(1));
    assert_eq!(h.alarm(&sql).await, Some(t0 + 1_000));
    Ok(())
}

fn token(
    t0: i64,
    kind: TokenKind,
    id: &str,
    secret: &str,
    generation: u64,
    family: &str,
) -> ExecutionCredentialRecord {
    let s = scope(1);
    ExecutionCredentialRecord {
        token_id: id.parse().unwrap(),
        kind,
        hash: secret_hash(secret),
        family_id: family.into(),
        execution_id: execution::projection().execution_id,
        org_id: s.org_id,
        project_id: s.project_id,
        generation,
        issued_at_ms: t0,
        expires_at_ms: t0 + 3_600_000,
        consumed_at_ms: None,
        revoked_at_ms: None,
    }
}
fn family(t0: i64, key: &str, hash: u8) -> CredentialFamily {
    let s = scope(1);
    CredentialFamily {
        family_id: "fam_1".into(),
        execution_id: execution::projection().execution_id,
        org_id: s.org_id,
        project_id: s.project_id,
        issuer_token_id: "tok_00000000000000000000000009".parse().unwrap(),
        issuer: runbook_core::credentials::CredentialPrincipal::Service {
            project_id: scope(1).project_id,
            role: runbook_model::ProjectRole::Operator,
        },
        session_id: "ses_1".into(),
        idempotency_key: key.into(),
        request_hash: [hash; 32],
        created_at_ms: t0,
        revoked_at_ms: None,
    }
}
fn issue(t0: i64) -> CredentialWrite {
    CredentialWrite::IssueFamily {
        family: Box::new(family(t0, "mint-1", 1)),
        access: Box::new(token(
            t0,
            TokenKind::Execution,
            "tok_00000000000000000000000001",
            "access-1",
            1,
            "fam_1",
        )),
        refresh: Box::new(token(
            t0,
            TokenKind::Refresh,
            "tok_00000000000000000000000002",
            "refresh-1",
            1,
            "fam_1",
        )),
    }
}

pub async fn credentials_are_hash_only_rotate_once_and_revoke_on_close<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    let mut b = step(t0, 1);
    b.credentials = vec![issue(t0)];
    (s.commit(b)).await.unwrap();
    let access = (s.credential_by_hash(&secret_hash("access-1")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(access.generation, 1);
    assert!(
        (s.credential_by_hash(&secret_hash("other")))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        (s.family_by_idempotency_key(&access.execution_id, "mint-1"))
            .await
            .unwrap()
            .is_some()
    );
    // Replaying the mint key with the same request, or another request.
    let mut again = step(t0, 2);
    again.credentials = vec![issue(t0)];
    assert_eq!(
        (s.commit(again.clone())).await,
        Err(StoreError::InvalidBatch("credentials.already_issued"))
    );
    if let CredentialWrite::IssueFamily { family, .. } = &mut again.credentials[0] {
        family.request_hash = [9; 32];
    }
    assert_eq!(
        (s.commit(again)).await,
        Err(StoreError::InvalidBatch("request.idempotency_key_reused"))
    );
    let rotate = |access: &str, refresh: &str, a_id: &str, r_id: &str| CredentialWrite::Rotate {
        consume_token_id: "tok_00000000000000000000000002".parse().unwrap(),
        consumed_at_ms: t0,
        access: Box::new(token(t0, TokenKind::Execution, a_id, access, 2, "fam_1")),
        refresh: Box::new(token(t0, TokenKind::Refresh, r_id, refresh, 2, "fam_1")),
    };
    let mut r = step(t0, 2);
    r.credentials = vec![rotate(
        "access-2",
        "refresh-2",
        "tok_00000000000000000000000003",
        "tok_00000000000000000000000004",
    )];
    (s.commit(r)).await.unwrap();
    let consumed = (s.credential_by_id(&"tok_00000000000000000000000002".parse().unwrap()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(consumed.consumed_at_ms, Some(t0));
    let mut reuse = step(t0, 3);
    reuse.credentials = vec![rotate(
        "access-3",
        "refresh-3",
        "tok_00000000000000000000000005",
        "tok_00000000000000000000000006",
    )];
    assert_eq!(
        (s.commit(reuse)).await,
        Err(StoreError::InvalidBatch("credentials.refresh_reused"))
    );
    // Closing the run revokes every token of every family, atomically.
    let mut close = step(t0, 3);
    close.projection.status = ExecutionStatus::Succeeded;
    close.projection.completed_at_ms = Some(t0);
    close.derived_outbox[0].summary = summary(3, ExecutionStatus::Succeeded, Some(t0));
    close.derived_outbox[0].release_concurrency = true;
    (s.commit(close)).await.unwrap();
    for n in 1..=4 {
        let id = format!("tok_0000000000000000000000000{n}").parse().unwrap();
        let record = (s.credential_by_id(&id)).await.unwrap().unwrap();
        assert_eq!(record.revoked_at_ms, Some(t0), "token {n}");
    }
    assert_eq!(
        (s.credential_family("fam_1"))
            .await
            .unwrap()
            .unwrap()
            .revoked_at_ms,
        Some(t0)
    );
    // Another tenant sees none of it.
    let other = s.scoped(scope(2));
    assert!(
        (other.credential_by_hash(&secret_hash("access-2")))
            .await
            .unwrap()
            .is_none()
    );
    assert!((other.credential_family("fam_1")).await.unwrap().is_none());
    Ok(())
}

pub(crate) fn claim(lease: Option<i64>) -> ClaimWrite {
    let mut c = execution::batch().claims.remove(0);
    c.owner = ActorRef::System;
    c.session = ClaimSession {
        principal: "system".into(),
        actor: ActorRef::System,
        credential_ref: "test".into(),
    };
    c.action = ClaimAction::Claim;
    c.lease_deadline_at_ms = lease;
    c
}

pub async fn heartbeats_and_logs_are_fenced_and_lease_expiry_is_work<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let mut b = step(t0, 1);
    b.claims = vec![claim(Some(t0 + 600_000))];
    (s.commit(b)).await.unwrap();
    s.ack_derived(1).unwrap();
    ack_all_notifications(&s).unwrap();
    assert_eq!(
        h.alarm(&sql).await,
        Some(t0 + 600_000),
        "the claim deadline"
    );
    let c = claim(None);
    let reference = runbook_model::ClaimRef {
        id: c.claim_id.clone(),
        generation: std::num::NonZeroU64::new(1).unwrap(),
    };
    let id = execution::projection().execution_id;
    h.set_time(&sql, t0 + 1_000);
    let deadline = (s.heartbeat(&id, &c.step_run_id, &reference, &ActorRef::System, None))
        .await
        .unwrap();
    assert_eq!(deadline, Some(t0 + 1_000 + 600_000));
    assert_eq!(
        h.alarm(&sql).await,
        deadline,
        "the renewed lease moves the alarm"
    );
    let lines = vec!["one".to_owned(), "two".to_owned()];
    assert_eq!(
        (s.append_logs(
            &id,
            &c.step_run_id,
            &reference,
            &ActorRef::System,
            lines,
            None
        ))
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        (s.read_logs(&id, &c.step_run_id, 1, 10)).await.unwrap(),
        [(2, "two".to_owned())]
    );
    let stale = runbook_model::ClaimRef {
        id: c.claim_id.clone(),
        generation: std::num::NonZeroU64::new(2).unwrap(),
    };
    assert_eq!(
        (s.heartbeat(&id, &c.step_run_id, &stale, &ActorRef::System, None)).await,
        Err(StoreError::InvalidBatch("queue.stale_lease"))
    );
    let huge = vec!["x".repeat(5 * 1024 * 1024)];
    assert_eq!(
        (s.append_logs(
            &id,
            &c.step_run_id,
            &reference,
            &ActorRef::System,
            huge,
            None
        ))
        .await,
        Err(StoreError::InvalidBatch("step.logs_too_large"))
    );
    // Past the deadline the lease is due work for the drain.
    h.set_time(&sql, t0 + 1_000 + 600_000);
    let lease = (s.claim_due(10, 60_000)).await.unwrap().remove(0);
    assert!(matches!(
        lease.id.kind,
        WorkKind::ClaimLease { generation: 1, .. }
    ));
    let other = s.scoped(scope(2));
    assert!(
        (other.claim_record(&id, &c.step_run_id))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        (other.read_logs(&id, &c.step_run_id, 0, 10)).await,
        Err(StoreError::NotFound)
    );
    Ok(())
}

fn start_init(admission: Option<Admission>) -> StartInit {
    let p = execution::projection();
    StartInit {
        projection: p.clone(),
        record: runbook_core::start::StartRecord {
            runbook_id: p.runbook_id.clone(),
            version_id: p.version_id.clone(),
            version: std::num::NonZeroU32::new(1),
            semantic_hash: [1; 32],
            snapshot_digest: [2; 32],
            context: Default::default(),
            concurrency_key: admission.as_ref().map(|_| "deploy".into()),
            rerun_of: None,
            request_tags: vec![],
        },
        admission,
        derived: DerivedUpdate {
            summary: summary(0, ExecutionStatus::Queued, None),
            assignments: vec![],
            release_concurrency: false,
            memory: vec![],
        },
    }
}

pub async fn a_start_records_its_facts_and_activation_waits_for_admission<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let id = execution::projection().execution_id;
    // Unqueued: activation is due at the start time.
    let sql = h.fresh()?;
    h.set_time(&sql, t0);
    let s = DoStore::open(sql.clone(), &execution::projection().execution_id, scope(1))?;
    s.initialize_start(start_init(None)).unwrap();
    assert_eq!(s.start_record(&id).unwrap().unwrap().semantic_hash, [1; 32]);
    let derived = s.pending_derived(None, 10).unwrap();
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].0, 0);
    assert_eq!(derived[0].1.summary.created_at_ms, t0, "store clock");
    assert_eq!(h.alarm(&sql).await, Some(t0), "activation is due now");
    let lease = (s.claim_due(10, 60_000)).await.unwrap().remove(0);
    assert_eq!(lease.id.kind, WorkKind::Activation);
    assert_eq!(
        s.initialize_start(start_init(None)),
        Err(StoreError::InvalidBatch("store.execution_exists"))
    );
    assert!(s.scoped(scope(2)).start_record(&id).unwrap().is_none());

    // Queued in D1: an execution.queued intent at sequence 0, no activation
    // until D1 admits it, then activation at the admission time (D24).
    let sql = h.fresh()?;
    h.set_time(&sql, t0);
    let s = DoStore::open(sql.clone(), &execution::projection().execution_id, scope(1))?;
    let queued = Admission {
        execution_id: id.clone(),
        state: AdmissionState::Queued,
        admitted_at_ms: None,
    };
    s.initialize_start(start_init(Some(queued))).unwrap();
    let intents = s.pending_notifications(None, 10).unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(
        (intents[0].kind.as_str(), intents[0].sequence),
        ("execution.queued", 0)
    );
    ack_all_notifications(&s).unwrap();
    s.ack_derived(0).unwrap();
    assert_eq!(h.alarm(&sql).await, None, "queued: nothing to do");
    assert!((s.claim_due(10, 60_000)).await.unwrap().is_empty());
    s.admit(&id, t0 + 50).unwrap();
    s.admit(&id, t0 + 50).unwrap();
    assert_eq!(
        s.admit(&id, t0 + 60),
        Err(StoreError::InvalidBatch("start.already_admitted"))
    );
    assert_eq!(h.alarm(&sql).await, Some(t0 + 50));
    Ok(())
}

pub async fn another_tenant_cannot_read_or_write_this_object<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    (s.commit(step(t0, 1))).await.unwrap();
    let other = s.scoped(scope(2));
    let id = execution::projection().execution_id;
    assert_eq!((other.load(&id)).await, Err(StoreError::NotFound));
    assert_eq!((other.commit(step(t0, 2))).await, Err(StoreError::NotFound));
    assert_eq!(other.pending_derived(None, 10), Err(StoreError::NotFound));
    assert_eq!(other.ack_derived(1), Err(StoreError::NotFound));
    assert!((other.claim_due(10, 60_000)).await.unwrap().is_empty());
    // Nor can a commit for another execution land in this object.
    let mut foreign = step(t0, 2);
    foreign.execution_id = "exe_00000000000000000000000002".parse().unwrap();
    assert_eq!(
        (s.commit(foreign)).await,
        Err(StoreError::InvalidBatch("do.object_mismatch"))
    );
    Ok(())
}

pub async fn every_run_owned_row_carries_its_tenant_and_execution<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let mut b = execution::batch();
    b.credentials = vec![issue(t0)];
    b.events[0].at_ms = t0;
    b.outbox[0].at_ms = t0;
    (s.commit(b)).await.unwrap();
    for (table, count) in s.table_counts().unwrap() {
        if count == 0 || table == "alarm_log" {
            continue;
        }
        let rows = sql
            .query(
                &format!("SELECT COUNT(*) FROM {table} WHERE org_id = ? AND project_id = ? AND execution_id = ?"),
                &[
                    scope(1).org_id.as_str().into(),
                    scope(1).project_id.as_str().into(),
                    execution::projection().execution_id.as_str().into(),
                ],
            )
            .unwrap();
        assert_eq!(
            rows[0][0],
            runbook_store_do::Value::Integer(count as i64),
            "{table}"
        );
    }
    Ok(())
}

pub async fn migrations_apply_once_and_refuse_a_changed_history<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let (_store, sql) = store(h).await;
    assert!(
        runbook_store_do::migrate::migrate(&sql)?.is_empty(),
        "already applied"
    );
    let history = sql.query(
        "SELECT version, name FROM do_migration ORDER BY version",
        &[],
    )?;
    assert_eq!(history.len(), runbook_store_do::migrate::MIGRATIONS.len());
    sql.exec(
        "UPDATE do_migration SET checksum = 'tampered' WHERE version = 1",
        &[],
    )?;
    assert!(matches!(
        DoStore::open(sql.clone(), &execution::projection().execution_id, scope(1)),
        Err(StoreError::Backend("do.migration_history_mismatch"))
    ));
    let pinned = runbook_store_do::migrate::checksum(runbook_store_do::migrate::MIGRATIONS[0].sql);
    sql.exec(
        "UPDATE do_migration SET checksum = ? WHERE version = 1",
        &[pinned.into()],
    )?;
    // UPD1: a newer release's file (the object was migrated by the next
    // release, then `wrangler rollback` ran this code) is accepted, since
    // expand/contract keeps this code working on it; nothing is re-run.
    let newer = runbook_store_do::migrate::MIGRATIONS.len() as i64 + 1;
    sql.exec(
        "INSERT INTO do_migration (version, name, checksum, applied_at_ms) VALUES (?, ?, ?, ?)",
        &[
            runbook_store_do::Value::Integer(newer),
            "9999_from_a_newer_release.sql".into(),
            "from-a-newer-release".into(),
            runbook_store_do::Value::Integer(1),
        ],
    )?;
    assert!(
        runbook_store_do::migrate::migrate(&sql)?.is_empty(),
        "a newer release's schema is accepted"
    );
    // A gap is not a newer release's history.
    sql.exec(
        "UPDATE do_migration SET version = ? WHERE version = ?",
        &[
            runbook_store_do::Value::Integer(newer + 1),
            runbook_store_do::Value::Integer(newer),
        ],
    )?;
    assert!(matches!(
        runbook_store_do::migrate::migrate(&sql),
        Err(StoreError::Backend("do.migration_history_mismatch"))
    ));
    sql.exec(
        "DELETE FROM do_migration WHERE version = ?",
        &[runbook_store_do::Value::Integer(newer + 1)],
    )?;
    Ok(())
}

pub async fn reset_empties_every_table<H: Host>(h: &H) -> Result<(), StoreError> {
    let (s, _sql) = store(h).await;
    s.reset()?;
    assert!(s.table_counts()?.iter().all(|(_, n)| *n == 0));
    Ok(())
}

/// CF0/CF2: actor logic runs inside the DO and awaits D1 and R2 between
/// `begin` and `commit`, so the object's clock moves on meanwhile. The
/// session commits at its own transaction time (as Postgres commits at the
/// transaction's start time), and the head compare-and-set still decides.
pub async fn a_session_commits_at_its_own_time_after_the_clock_moves<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    use runbook_core::session::{ExecutionSession, SessionStore};
    let (s, sql) = store(h).await;
    let id = execution::projection().execution_id;
    let session = (s.begin(&id)).await.unwrap();
    let at = session.transaction_time_ms();
    let replay = runbook_core::actor::Replay {
        key: "retry-1".into(),
        principal: "system".into(),
        request_hash: [5; 32],
    };
    let mut b = step(t0, 1);
    b.idempotency = vec![replay.record("claim_step:x", &id, at, 1).unwrap()];
    h.set_time(&sql, t0 + 250); // a D1 directory read and an R2 snapshot fetch
    assert_eq!(
        (session.commit(b)).await.unwrap(),
        CommitResult::Committed(1)
    );
    let later = (s.begin(&id)).await.unwrap();
    let saved = (later.actor_replay(&replay.prefix("claim_step:x").unwrap()))
        .await
        .unwrap();
    assert_eq!(saved.unwrap().response["at_ms"], at);
    drop(later);
    // A second session begun before that commit loses the compare-and-set.
    let stale = (s.begin(&id)).await.unwrap();
    (s.commit(step(t0, 2))).await.unwrap();
    assert_eq!(
        (stale.commit(step(t0, 3))).await.unwrap(),
        CommitResult::Conflict
    );
    Ok(())
}

/// Review fix 1: a page that ends inside a sequence acks only what it held.
pub async fn a_notification_page_never_acks_an_undelivered_intent<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    let mut b = step(t0, 1);
    let first = b.outbox[0].clone();
    b.outbox = ["evt_a", "evt_b", "evt_c"]
        .into_iter()
        .map(|id| {
            let mut intent = first.clone();
            intent.event_id = id.into();
            intent
        })
        .collect();
    s.commit(b).await?;
    let page = s.pending_notifications(None, 2)?;
    assert_eq!(page.len(), 2);
    let last = NotificationCursor::of(page.last().expect("a page"));
    assert_eq!(s.ack_notifications(&last)?, 2);
    assert_eq!(s.ack_notifications(&last)?, 0, "idempotent");
    let next = s.pending_notifications(Some(&last), 10)?;
    assert_eq!(next.len(), 1, "the page after the cursor");
    let beyond = NotificationCursor {
        sequence: 2,
        event_id: "evt_a".into(),
    };
    assert_eq!(
        s.ack_notifications(&beyond),
        Err(StoreError::InvalidBatch("outbox.ack_beyond_head"))
    );
    let left: Vec<String> = s
        .pending_notifications(None, 10)?
        .into_iter()
        .map(|i| i.event_id)
        .collect();
    assert_eq!(
        left,
        ["evt_c"],
        "the intent after the page is still pending"
    );
    Ok(())
}

fn other_projection() -> runbook_core::ExecutionProjection {
    let mut p = execution::projection();
    p.execution_id = "exe_00000000000000000000000002".parse().expect("id");
    p
}

/// Review fix 3: an object serves only the execution it is named for.
pub async fn an_object_serves_only_its_own_execution<H: Host>(h: &H) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let mismatch = StoreError::InvalidBatch("do.object_mismatch");
    // Nothing for another execution can land in a fresh object.
    let sql = h.fresh()?;
    h.set_time(&sql, t0);
    let s = DoStore::open(sql.clone(), &execution::projection().execution_id, scope(1))?;
    assert_eq!(
        s.create_execution(other_projection()),
        Err(mismatch.clone())
    );
    let mut foreign = step(t0, 1);
    foreign.execution_id = other_projection().execution_id;
    foreign.projection.execution_id = foreign.execution_id.clone();
    foreign.events[0].execution_id = foreign.execution_id.clone();
    foreign.outbox[0].execution_id = foreign.execution_id.clone();
    foreign.derived_outbox[0].summary.execution_id = foreign.execution_id.clone();
    assert_eq!(s.commit(foreign).await, Err(mismatch.clone()));
    let mut init = start_init(None);
    init.projection = other_projection();
    init.derived.summary.execution_id = init.projection.execution_id.clone();
    assert_eq!(s.initialize_start(init), Err(mismatch));
    assert!(
        s.table_counts()?.iter().all(|(_, n)| *n == 0),
        "nothing written"
    );
    // An object holding one execution cannot be opened as another.
    s.create_execution(execution::projection())?;
    assert!(matches!(
        DoStore::open(sql, &other_projection().execution_id, scope(1)),
        Err(StoreError::Backend("do.object_mismatch"))
    ));
    Ok(())
}

/// Review fix 5: an object instance checks its schema once, not per request.
pub async fn an_object_checks_its_schema_once_per_instance<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let (_store, sql) = store(h).await;
    let name = execution::projection().execution_id;
    let object = runbook_store_do::DoObject::new(sql.clone());
    object.store(&name, scope(1))?;
    sql.exec(
        "UPDATE do_migration SET checksum = 'tampered' WHERE version = 1",
        &[],
    )?;
    // The instance already checked: later requests do not re-read or re-hash.
    object.store(&name, scope(1))?;
    // A new instance (after eviction) checks again and refuses.
    assert!(matches!(
        runbook_store_do::DoObject::new(sql.clone()).store(&name, scope(1)),
        Err(StoreError::Backend("do.migration_history_mismatch"))
    ));
    let pinned = runbook_store_do::migrate::checksum(runbook_store_do::migrate::MIGRATIONS[0].sql);
    sql.exec(
        "UPDATE do_migration SET checksum = ? WHERE version = 1",
        &[pinned.into()],
    )?;
    // UPD1: a newer release's file (the object was migrated by the next
    // release, then `wrangler rollback` ran this code) is accepted, since
    // expand/contract keeps this code working on it; nothing is re-run.
    let newer = runbook_store_do::migrate::MIGRATIONS.len() as i64 + 1;
    sql.exec(
        "INSERT INTO do_migration (version, name, checksum, applied_at_ms) VALUES (?, ?, ?, ?)",
        &[
            runbook_store_do::Value::Integer(newer),
            "9999_from_a_newer_release.sql".into(),
            "from-a-newer-release".into(),
            runbook_store_do::Value::Integer(1),
        ],
    )?;
    assert!(
        runbook_store_do::migrate::migrate(&sql)?.is_empty(),
        "a newer release's schema is accepted"
    );
    // A gap is not a newer release's history.
    sql.exec(
        "UPDATE do_migration SET version = ? WHERE version = ?",
        &[
            runbook_store_do::Value::Integer(newer + 1),
            runbook_store_do::Value::Integer(newer),
        ],
    )?;
    assert!(matches!(
        runbook_store_do::migrate::migrate(&sql),
        Err(StoreError::Backend("do.migration_history_mismatch"))
    ));
    sql.exec(
        "DELETE FROM do_migration WHERE version = ?",
        &[runbook_store_do::Value::Integer(newer + 1)],
    )?;
    Ok(())
}

/// Review fix 2: a write that leaves the revision unchanged (a no-event
/// claim release) still fences a session that began before it.
pub async fn a_write_that_keeps_the_revision_still_fences_a_session<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    use runbook_core::session::{ExecutionSession, SessionStore};
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    let id = execution::projection().execution_id;
    let mut first = step(t0, 1);
    first.claims = vec![claim(None)];
    s.commit(first.clone()).await?;
    let stale = s.begin(&id).await?;
    // A no-event commit releases the claim; the revision stays 1.
    let mut released = claim(None);
    released.expected_generation = 1;
    released.generation = 1;
    released.action = ClaimAction::Release;
    released.released = true;
    let stored = s.load(&id).await?;
    s.commit(ExecutionCommit {
        execution_id: id.clone(),
        expected_sequence: 1,
        events: vec![],
        projection: stored.projection.clone(),
        timers: vec![],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![released],
        credentials: vec![],
        derived_outbox: vec![],
    })
    .await?;
    assert_eq!(s.load(&id).await?.revision, 1, "the revision did not move");
    // The stale session's events would rest on a claim that is gone.
    let mut next = step(t0, 2);
    next.events[0].at_ms = stale.transaction_time_ms();
    next.outbox[0].at_ms = stale.transaction_time_ms();
    assert_eq!(stale.commit(next).await?, CommitResult::Conflict);
    Ok(())
}

/// Review fix 2: two sessions begun in the same millisecond for the same
/// replay key: the second sees the first's reply, as behind Postgres's
/// row lock, instead of colliding on the replay row.
pub async fn sessions_in_one_millisecond_replay_instead_of_colliding<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    use runbook_core::session::{ExecutionSession, SessionStore};
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    let id = execution::projection().execution_id;
    s.commit(step(t0, 1)).await?;
    let replay = runbook_core::actor::Replay {
        key: "retry-1".into(),
        principal: "system".into(),
        request_hash: [5; 32],
    };
    let prefix = replay.prefix("heartbeat:x")?;
    let first = s.begin(&id).await?;
    assert!(first.actor_replay(&prefix).await?.is_none());
    // The second request begins while the first is still open.
    let mut second = pin!(async {
        let session = s.begin(&id).await?;
        let seen = session.actor_replay(&prefix).await?;
        Ok::<_, StoreError>((session, seen))
    });
    let mut context = Context::from_waker(Waker::noop());
    let early = second.as_mut().poll(&mut context);
    // The first commits its reply (replay only: no events).
    let stored = s.load(&id).await?;
    let record = replay.record("heartbeat:x", &id, first.transaction_time_ms(), 7)?;
    first
        .commit(ExecutionCommit {
            execution_id: id.clone(),
            expected_sequence: 1,
            events: vec![],
            projection: stored.projection,
            timers: vec![],
            outbox: vec![],
            idempotency: vec![record],
            claims: vec![],
            credentials: vec![],
            derived_outbox: vec![],
        })
        .await?;
    let (_session, seen) = match early {
        Poll::Ready(result) => result?,
        Poll::Pending => match second.as_mut().poll(&mut context) {
            Poll::Ready(result) => result?,
            Poll::Pending => panic!("the second session still waits after the first ended"),
        },
    };
    let seen = seen.expect("the second request replays the first's reply");
    assert_eq!(seen.response["value"], serde_json::json!(7));
    Ok(())
}

/// Review fix 4: an overdue outbox retry never re-arms the alarm in the
/// past (which would fire it in a loop when a write forgets to defer).
pub async fn an_overdue_outbox_retry_never_arms_a_past_alarm<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let mut b = step(t0, 1);
    b.claims = vec![claim(None)];
    s.commit(b).await?;
    // Long past the retry time, a write that is not a delivery attempt.
    let now = t0 + 60_000;
    h.set_time(&sql, now);
    let c = claim(None);
    let reference = runbook_model::ClaimRef {
        id: c.claim_id.clone(),
        generation: std::num::NonZeroU64::new(1).expect("one"),
    };
    let id = execution::projection().execution_id;
    s.heartbeat(&id, &c.step_run_id, &reference, &ActorRef::System, None)
        .await?;
    let alarm = h.alarm(&sql).await.expect("the outbox still needs a retry");
    assert!(
        alarm >= now + runbook_store_do::OUTBOX_MIN_RETRY_MS,
        "alarm {alarm} is not before now + the minimum backoff"
    );
    Ok(())
}

/// Review fix 4: a host claims only the work it will do; the rest stays
/// unleased for its own consumer.
pub async fn claiming_one_kind_leaves_other_work_unleased<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let mut b = step(t0, 1);
    b.timers = vec![TimerChange::Upsert(TimerRecord {
        execution_id: b.execution_id.clone(),
        key: "t".into(),
        generation: 1,
        due_at_ms: t0,
    })];
    s.commit(b).await?;
    h.set_time(&sql, t0 + 1);
    let timers = s.claim_due_of(10, 60_000, |kind| matches!(kind, WorkKind::Timer { .. }))?;
    assert_eq!(timers.len(), 1);
    assert!(matches!(timers[0].id.kind, WorkKind::Timer { .. }));
    // The notification was not leased: its consumer gets generation 1 now.
    let rest = s.claim_due(10, 60_000).await?;
    assert_eq!(rest.len(), 1);
    assert!(matches!(rest[0].id.kind, WorkKind::Notification { .. }));
    assert_eq!(rest[0].generation, 1);
    Ok(())
}

/// Review fix 4: a time a JS `Date` cannot hold (|t| > 8.64e15 ms) would
/// make `setAlarm` throw on every later write; it is refused at commit.
pub async fn a_time_past_the_date_range_is_refused<H: Host>(h: &H) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let max = runbook_store_do::MAX_ALARM_MS;
    let mut b = step(t0, 1);
    b.timers = vec![TimerChange::Upsert(TimerRecord {
        execution_id: b.execution_id.clone(),
        key: "far".into(),
        generation: 1,
        due_at_ms: max + 1,
    })];
    assert_eq!(
        s.commit(b.clone()).await,
        Err(StoreError::InvalidBatch("store.timer_out_of_range"))
    );
    let mut lease = step(t0, 1);
    let mut c = claim(Some(max + 1));
    c.lease_deadline_at_ms = Some(max + 1);
    lease.claims = vec![c];
    assert_eq!(
        s.commit(lease).await,
        Err(StoreError::InvalidBatch("store.time_out_of_range"))
    );
    // The edge of the range is fine, and later writes still work.
    if let TimerChange::Upsert(timer) = &mut b.timers[0] {
        timer.due_at_ms = max;
    }
    s.commit(b).await?;
    s.ack_derived(1)?;
    assert!(h.alarm(&sql).await.is_some());
    Ok(())
}

/// Review fix 6: a load (and a session's snapshot) carries no delivered
/// notification, and of the delivered derived rows only the latest (the
/// actor diffs inbox assignments against it).
pub async fn a_load_skips_delivered_outbox_rows<H: Host>(h: &H) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, _sql) = store(h).await;
    for n in 1..=3 {
        s.commit(step(t0, n)).await?;
    }
    ack_all_notifications(&s)?;
    s.ack_derived(3)?;
    let id = execution::projection().execution_id;
    let delivered = s.load(&id).await?;
    assert!(delivered.outbox.is_empty(), "no delivered intent is loaded");
    assert_eq!(
        delivered.derived_outbox.keys().copied().collect::<Vec<_>>(),
        [3],
        "the latest row stays loaded once delivered"
    );
    s.commit(step(t0, 4)).await?;
    let run = s.load(&id).await?;
    assert_eq!(
        run.outbox.keys().cloned().collect::<Vec<_>>(),
        ["evt_4"],
        "only the undelivered intent"
    );
    assert_eq!(
        run.derived_outbox.keys().copied().collect::<Vec<_>>(),
        [4],
        "the undelivered row, which is also the latest"
    );
    // The rows themselves stay with the run.
    assert_eq!(
        s.table_counts()?
            .iter()
            .find(|(t, _)| *t == "outbox")
            .map(|(_, n)| *n),
        Some(4)
    );
    Ok(())
}

/// Review fix 7: two stores from two `fresh` objects never share rows.
/// Natively they are two databases; in workerd a case has one object, so
/// the older store fails loudly (`test.store_aliased`) instead.
pub async fn two_fresh_objects_never_share_rows<H: Host>(h: &H) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let id = execution::projection().execution_id;
    let (first, _a) = store(h).await;
    let (second, _b) = store(h).await;
    second.commit(step(t0, 1)).await?;
    match first.load(&id).await {
        Ok(run) => assert_eq!(run.revision, 0, "the first object saw the second's commit"),
        Err(error) => assert_eq!(error, StoreError::Backend("test.store_aliased")),
    }
    Ok(())
}

/// A start commit for `catalog::id(n)` with no admission (CF3 D1 cases).
pub fn start_commit(n: u8) -> runbook_core::start::StartCommit {
    let init = start_init(None);
    let mut projection = init.projection;
    projection.execution_id = runbook_store_conformance::catalog::id(n);
    let mut derived = init.derived;
    derived.summary.execution_id = projection.execution_id.clone();
    runbook_core::start::StartCommit {
        projection,
        record: init.record,
        admission: None,
        on_conflict: None,
        derived,
        replay: None,
    }
}

// ---- CF13: a run's rows stay within one Durable Object row ----

/// Step run `n` of the shared run, reported with `summary`.
fn step_projection(n: u8, summary: &str) -> runbook_core::StepProjection {
    let mut outputs = runbook_model::Data::new();
    outputs.insert("n".into(), serde_json::json!(n));
    runbook_core::StepProjection {
        execution_id: execution::projection().execution_id,
        step_run_id: format!("sr_{:026}", n).parse().unwrap(),
        path: format!("step{n}"),
        status: runbook_model::StepStatus::Succeeded,
        verdict: Some(runbook_model::Verdict::Pass),
        outputs,
        summary: Some(summary.to_owned()),
    }
}

/// An open assignment of step run `n`, titled `title`.
fn assignment(n: u8, title: &str) -> runbook_core::catalog::Assignment {
    runbook_core::catalog::Assignment {
        execution_id: execution::projection().execution_id,
        step_run_id: format!("sr_{:026}", n).parse().unwrap(),
        recipients: vec![runbook_model::RecipientRef::Team("ops".into())],
        kind: Default::default(),
        sequence: 1,
        updated_at_ms: 0,
        voted_by: vec![],
        path: format!("step{n}"),
        title: title.to_owned(),
    }
}

fn text_column<B: DoSql>(sql: &B, query: &str) -> Vec<Option<String>> {
    sql.query(query, &[])
        .unwrap()
        .into_iter()
        .map(|row| match &row[0] {
            runbook_store_do::Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// CF13: the head keeps no steps; each step run is a row of its own, read
/// back in order and identical. A commit rewrites only the step rows that
/// changed, and an older release (which requires `steps`) refuses the new
/// head instead of reading a run with none.
pub async fn step_runs_are_rows_of_their_own_and_load_back_identical<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let id = execution::projection().execution_id;
    let mut b = step(t0, 1);
    b.projection.steps = (1..=3).map(|n| step_projection(n, "first")).collect();
    s.commit(b.clone()).await?;
    assert_eq!(s.load(&id).await?.projection, b.projection);
    let head = text_column(&sql, "SELECT projection FROM meta")[0]
        .clone()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&head).unwrap();
    assert!(json.get("steps").is_none(), "{head}");
    assert!(json.get("status").is_some(), "{head}");
    assert!(
        serde_json::from_str::<runbook_core::ExecutionProjection>(&head).is_err(),
        "an older release cannot read the head as a run with no steps"
    );
    // Mark the stored rows: a row the next commit leaves alone keeps it.
    sql.exec("UPDATE step_run SET summary = 'marked'", &[])?;
    let mut next = step(t0, 2);
    next.projection.steps = b.projection.steps.clone();
    next.projection.steps[1].summary = Some("second".into());
    next.projection.steps.push(step_projection(4, "new"));
    s.commit(next.clone()).await?;
    assert_eq!(s.load(&id).await?.projection, next.projection);
    assert_eq!(
        text_column(&sql, "SELECT summary FROM step_run ORDER BY position"),
        // CF13 review round 1: a written row leaves the unread `summary`
        // column empty, so the marked rows are the ones left alone.
        [Some("marked"), None, Some("marked"), None].map(|s| s.map(str::to_owned)),
        "only the changed and the new step rows were written"
    );
    // A step run gone from the projection is gone from the rows.
    let mut fewer = step(t0, 3);
    fewer.projection.steps = next.projection.steps[1..].to_vec();
    s.commit(fewer.clone()).await?;
    assert_eq!(s.load(&id).await?.projection, fewer.projection);
    assert_eq!(
        text_column(&sql, "SELECT step_run_id FROM step_run").len(),
        3
    );
    Ok(())
}

/// CF13: a head written before CF13 (its steps inside it, its step rows
/// only an index) still loads identically, and the run's next commit
/// splits it.
pub async fn a_head_written_before_cf13_loads_and_the_next_commit_splits_it<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let id = execution::projection().execution_id;
    let mut b = step(t0, 1);
    b.projection.steps = (1..=3).map(|n| step_projection(n, "old")).collect();
    s.commit(b.clone()).await?;
    // The shape the code before CF13 wrote.
    let whole = serde_json::to_string(&b.projection).unwrap();
    sql.exec("UPDATE meta SET projection = ?", &[whole.as_str().into()])?;
    sql.exec("UPDATE step_run SET body = NULL, position = NULL", &[])?;
    let before = s.load(&id).await?;
    assert_eq!(before.projection, b.projection);
    let mut next = step(t0, 2);
    next.projection.steps = b.projection.steps.clone();
    next.projection.steps[0].summary = Some("changed".into());
    s.commit(next.clone()).await?;
    let head = text_column(&sql, "SELECT projection FROM meta")[0]
        .clone()
        .unwrap();
    assert!(!head.contains("\"steps\""), "split: {head}");
    assert!(
        text_column(&sql, "SELECT body FROM step_run")
            .iter()
            .all(Option::is_some),
        "every step row has its body"
    );
    assert_eq!(s.load(&id).await?.projection, next.projection);
    Ok(())
}

/// CF13: a derived row keeps each open assignment as a row of its own and
/// reads back identical; one written before CF13 (assignments inside it)
/// is read as it is.
pub async fn derived_assignments_are_rows_of_their_own<H: Host>(h: &H) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let id = execution::projection().execution_id;
    let mut b = step(t0, 1);
    b.derived_outbox[0].assignments = (1..=3).map(|n| assignment(n, "Check")).collect();
    s.commit(b.clone()).await?;
    let entry = text_column(&sql, "SELECT entry FROM derived_outbox WHERE sequence = 1")[0]
        .clone()
        .unwrap();
    assert!(!entry.contains("\"assignments\""), "{entry}");
    assert!(serde_json::from_str::<DerivedUpdate>(&entry).is_err());
    assert_eq!(
        text_column(&sql, "SELECT body FROM derived_assignment").len(),
        3
    );
    assert_eq!(s.derived_row(1)?.as_ref(), Some(&b.derived_outbox[0]));
    assert_eq!(
        s.pending_derived(None, 10)?,
        [(1, b.derived_outbox[0].clone())]
    );
    assert_eq!(s.load(&id).await?.derived_outbox[&1], b.derived_outbox[0]);
    // A row written before CF13.
    let mut old = b.derived_outbox[0].clone();
    old.summary.sequence = 2;
    old.assignments.truncate(2);
    let row = runbook_store_do::Value::Text(serde_json::to_string(&old).unwrap());
    let tenant = [
        scope(1).org_id.as_str().into(),
        scope(1).project_id.as_str().into(),
        id.as_str().into(),
    ];
    let mut params: Vec<runbook_store_do::Value> = tenant.to_vec();
    params.extend([
        runbook_store_do::Value::Integer(2),
        runbook_store_do::Value::Integer(t0),
        row,
    ]);
    sql.exec(
        "INSERT INTO derived_outbox (org_id, project_id, execution_id, sequence, at_ms, entry) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &params,
    )?;
    assert_eq!(s.derived_row(2)?, Some(old.clone()));
    assert_eq!(s.pending_derived(Some(1), 10)?, [(2, old.clone())]);
    assert_eq!(s.load(&id).await?.derived_outbox[&2], old);
    Ok(())
}

/// CF13 backstop: a commit with a row past the object's limit is refused
/// with `execution.row_too_large` (a 422 at the API, never retried), and
/// nothing is written: no row, no alarm. For the head (the run's outputs),
/// a step row and an assignment row alike.
pub async fn a_row_past_the_limit_is_refused_and_nothing_is_written<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let (s, sql) = store(h).await;
    let mut b = step(t0, 1);
    b.timers = vec![TimerChange::Upsert(TimerRecord {
        execution_id: b.execution_id.clone(),
        key: "t".into(),
        generation: 1,
        due_at_ms: t0 + 1_000,
    })];
    let big = "x".repeat(runbook_core::MAX_ROW_BYTES);
    let mut outputs = b.clone();
    outputs
        .projection
        .outputs
        .insert("result".into(), serde_json::json!(big));
    let mut summary = b.clone();
    summary.projection.steps = vec![step_projection(1, &big)];
    let mut title = b.clone();
    title.derived_outbox[0].assignments = vec![assignment(1, &big)];
    let before = s.table_counts()?;
    for (what, batch) in [("outputs", outputs), ("summary", summary), ("title", title)] {
        assert_eq!(
            s.commit(batch).await,
            Err(StoreError::InvalidBatch(runbook_core::ROW_TOO_LARGE)),
            "{what}"
        );
        assert_eq!(s.table_counts()?, before, "{what}");
        assert_eq!(h.alarm(&sql).await, None, "{what}");
    }
    assert_eq!(s.commit(b).await?, CommitResult::Committed(1));
    assert_eq!(h.alarm(&sql).await, Some(t0 + 1_000));
    Ok(())
}

/// A run as the import installs it: the shared batch's run, one start
/// record, one actor replay row and one claim log line, at `t0`.
fn install_unit(
    t0: i64,
    id: &runbook_model::ExecutionId,
) -> runbook_store_do::portable::RunInstall {
    let b = execution::batch();
    let replay = runbook_core::actor::Replay {
        key: "retry-1".into(),
        principal: "system".into(),
        request_hash: [5; 32],
    };
    runbook_store_do::portable::RunInstall {
        run: runbook_core::StoredExecution {
            revision: 1,
            projection: b.projection.clone(),
            events: b.events.clone(),
            timers: Default::default(),
            timer_history: Default::default(),
            outbox: Default::default(),
            idempotency: b
                .idempotency
                .iter()
                .map(|r| (r.key.clone(), r.clone()))
                .collect(),
            claims: Default::default(),
            derived_outbox: Default::default(),
        },
        actor_replays: vec![replay.record("claim_step:x", id, t0, 1).unwrap()],
        work: vec![],
        start: Some(start_init(None).record),
        claim_logs: vec![runbook_core::portable::ClaimLog {
            step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
            lines: vec!["line".into()],
        }],
        activation_at_ms: None,
        held: false,
        import_id: String::new(),
        replace_held: false,
    }
}

/// CF13 review round 1, item 4: the import wrote events, idempotency and
/// actor replay rows, the start record and claim log lines unmeasured, so
/// one past what an object row holds failed in SQLite (a retryable `503`
/// in workerd) instead of `422 execution.row_too_large`. Every row the
/// import writes is measured: each such row is refused and nothing is
/// written.
pub async fn an_import_measures_every_row<H: Host>(h: &H) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let id = execution::projection().execution_id;
    let big = "x".repeat(runbook_core::MAX_ROW_BYTES);
    let mut cases = vec![];
    let mut unit = install_unit(t0, &id);
    unit.run.events[0].event = serde_json::from_value(serde_json::json!({
        "type": "StepClaimReleased",
        "step_run_id": "sr_00000000000000000000000001",
        "reason": big,
    }))
    .unwrap();
    cases.push(("event", unit));
    let mut unit = install_unit(t0, &id);
    for record in unit.run.idempotency.values_mut() {
        record.response = serde_json::json!(big);
    }
    cases.push(("idempotency", unit));
    let mut unit = install_unit(t0, &id);
    unit.actor_replays[0].response = serde_json::json!({"at_ms": t0, "value": big});
    cases.push(("actor replay", unit));
    let mut unit = install_unit(t0, &id);
    if let Some(start) = &mut unit.start {
        start.context.title = Some(big.clone());
    }
    cases.push(("start record", unit));
    let mut unit = install_unit(t0, &id);
    unit.claim_logs[0].lines = vec![big.clone()];
    cases.push(("claim log", unit));
    for (what, unit) in cases {
        let sql = h.fresh()?;
        h.set_time(&sql, t0);
        let s = DoStore::open(sql.clone(), &id, scope(1))?;
        let before = s.table_counts()?;
        assert_eq!(
            s.install(unit),
            Err(StoreError::InvalidBatch(runbook_core::ROW_TOO_LARGE)),
            "{what}"
        );
        assert_eq!(s.table_counts()?, before, "{what}: nothing is written");
    }
    // The same run within the limit installs.
    let sql = h.fresh()?;
    h.set_time(&sql, t0);
    let s = DoStore::open(sql, &id, scope(1))?;
    s.install(install_unit(t0, &id))?;
    assert_eq!(s.load(&id).await?.revision, 1);
    Ok(())
}

/// CF13 review round 1, item 5: the backstop's threshold on the object's
/// real SQLite. The largest step summary the row check admits is found by
/// bisection; that row (just under `MAX_ROW_BYTES` less the 4 KiB slack) is
/// written and loads back whole, so SQLite holds what the check lets
/// through, and one byte more is refused with `422 execution.row_too_large`
/// (never a storage failure). The summary is kept once in the row (in
/// `body`; the unread `summary` index column is no longer written), so the
/// largest is within a few hundred bytes of the threshold.
pub async fn a_row_at_the_threshold_is_written_and_one_byte_more_is_refused<H: Host>(
    h: &H,
) -> Result<(), StoreError> {
    let t0 = h.start_ms();
    let id = execution::projection().execution_id;
    let attempt = |n: usize| {
        let mut b = step(t0, 1);
        b.projection.steps = vec![step_projection(1, &"s".repeat(n))];
        b
    };
    let (mut fits, mut over) = (1_000_000usize, runbook_core::MAX_ROW_BYTES);
    while over - fits > 1 {
        let mid = fits + (over - fits) / 2;
        let (s, _) = store(h).await;
        match s.commit(attempt(mid)).await {
            Ok(CommitResult::Committed(1)) => fits = mid,
            Err(StoreError::InvalidBatch(runbook_core::ROW_TOO_LARGE)) => over = mid,
            other => panic!("a summary of {mid} bytes: {other:?}"),
        }
    }
    let threshold = runbook_core::MAX_ROW_BYTES - 4_096;
    assert!(
        fits > threshold - 1_024 && fits < threshold,
        "the largest summary admitted is {fits} bytes"
    );
    let (s, _) = store(h).await;
    assert_eq!(s.commit(attempt(fits)).await?, CommitResult::Committed(1));
    let loaded = s.load(&id).await?;
    assert_eq!(
        loaded.projection.steps[0].summary.as_ref().map(String::len),
        Some(fits)
    );
    let (s, _) = store(h).await;
    let before = s.table_counts()?;
    assert_eq!(
        s.commit(attempt(fits + 1)).await,
        Err(StoreError::InvalidBatch(runbook_core::ROW_TOO_LARGE))
    );
    assert_eq!(s.table_counts()?, before);
    Ok(())
}
