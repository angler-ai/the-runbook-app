use super::*;
use runbook_engine::{Event, EventEnvelope, ExecutionPhase, NotificationIntent};
use runbook_model::{ActorRef, ExecutionStatus};
use serde_json::json;
pub fn projection() -> ExecutionProjection {
    ExecutionProjection {
        execution_id: "exe_00000000000000000000000001".parse().unwrap(),
        runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
        version_id: "rbv_00000000000000000000000001".parse().unwrap(),
        compiled_hash: [7; 32],
        engine_version: runbook_engine::ENGINE_VERSION,
        sequence: 0,
        status: ExecutionStatus::Queued,
        phase: ExecutionPhase::Ordinary,
        completed_at_ms: None,
        result: json!(null),
        outputs: Default::default(),
        steps: vec![],
    }
}
pub fn batch() -> ExecutionCommit {
    let mut p = projection();
    p.sequence = 1;
    p.status = ExecutionStatus::Running;
    ExecutionCommit {
        execution_id: p.execution_id.clone(),
        expected_sequence: 0,
        events: vec![EventEnvelope {
            execution_id: p.execution_id.clone(),
            seq: 1,
            at_ms: 10,
            actor: ActorRef::System,
            event: Event::ManualPauseChanged { paused: false },
        }],
        projection: p.clone(),
        timers: vec![TimerChange::Upsert(TimerRecord {
            execution_id: p.execution_id.clone(),
            key: "deadline".into(),
            generation: 1,
            due_at_ms: 100,
        })],
        outbox: vec![NotificationIntent {
            execution_id: p.execution_id.clone(),
            event_id: "evt_test".into(),
            kind: "execution.started".into(),
            sequence: 1,
            at_ms: 10,
            step_run_ids: vec![],
            recipients: vec![],
            reason: None,
            link: "https://example.test/run".into(),
            ready_steps: vec![],
            params: None,
            detail: None,
        }],
        idempotency: vec![IdempotencyRecord {
            execution_id: p.execution_id.clone(),
            key: "request".into(),
            request_hash: [3; 32],
            response: json!({"ok":true}),
        }],
        derived_outbox: vec![],
        claims: vec![ClaimWrite {
            execution_id: p.execution_id.clone(),
            step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
            expected_generation: 0,
            claim_id: "clm_00000000000000000000000001".parse().unwrap(),
            generation: 1,
            action: runbook_core::ClaimAction::Claim,
            owner: ActorRef::System,
            session: runbook_core::ClaimSession {
                principal: "system".into(),
                actor: ActorRef::System,
                credential_ref: "conformance".into(),
            },
            lease_deadline_at_ms: None,
            released: false,
        }],
        credentials: vec![],
    }
}
pub async fn atomic_batch_or_nothing<F: StoreFactory>(f: &F) -> Result<(), StoreError> {
    for stage in [
        WriteStage::Events,
        WriteStage::Projection,
        WriteStage::Timers,
        WriteStage::Outbox,
        WriteStage::Idempotency,
        WriteStage::Claims,
    ] {
        let s = f.create(projection()).await?;
        let before = s.load(&projection().execution_id).await?;
        f.fail_after(&s, stage).await?;
        assert!(s.commit(batch()).await.is_err());
        assert_eq!(s.load(&projection().execution_id).await?, before);
    }
    Ok(())
}
pub async fn stale_commit_has_no_side_effects<F: StoreFactory>(f: &F) -> Result<(), StoreError> {
    let s = f.create(projection()).await?;
    let other = s.clone();
    assert_eq!(s.commit(batch()).await?, CommitResult::Committed(1));
    let after = s.load(&projection().execution_id).await?;
    let expected = batch();
    assert_eq!(after.events, expected.events);
    assert_eq!(after.projection, expected.projection);
    assert_eq!(after.revision, 1);
    let TimerChange::Upsert(timer) = &expected.timers[0] else {
        unreachable!()
    };
    assert_eq!(after.timers, [(timer.key.clone(), timer.clone())].into());
    assert_eq!(
        after.outbox,
        [(
            expected.outbox[0].event_id.clone(),
            expected.outbox[0].clone()
        )]
        .into()
    );
    assert_eq!(
        after.idempotency,
        [(
            expected.idempotency[0].key.clone(),
            expected.idempotency[0].clone()
        )]
        .into()
    );
    assert_eq!(
        after.claims,
        [(
            expected.claims[0].step_run_id.clone(),
            expected.claims[0].clone()
        )]
        .into()
    );
    assert_eq!(other.commit(batch()).await?, CommitResult::Conflict);
    assert_eq!(s.load(&projection().execution_id).await?, after);
    Ok(())
}
pub async fn one_claim_generation_winner<F: StoreFactory>(f: &F) -> Result<(), StoreError> {
    let s = f.create(projection()).await?;
    let second = s.clone();
    let (first, loser) = together(s.commit(batch()), second.commit(batch())).await;
    let (first, loser) = (first?, loser?);
    assert_eq!(
        [first, loser]
            .iter()
            .filter(|x| matches!(x, CommitResult::Committed(_)))
            .count(),
        1
    );
    let mut bad = batch();
    bad.expected_sequence = 1;
    bad.projection.sequence = 2;
    bad.events[0].seq = 2;
    bad.outbox.clear();
    bad.idempotency.clear();
    assert_eq!(s.commit(bad).await?, CommitResult::Conflict);
    assert_eq!(s.load(&projection().execution_id).await?.claims.len(), 1);
    Ok(())
}
pub async fn idempotent_response_is_payload_bound<F: StoreFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(projection()).await?;
    s.commit(batch()).await?;
    let run = s.load(&projection().execution_id).await?;
    assert_eq!(
        idempotent_response(&run, "request", &[3; 32])?,
        Some(json!({"ok":true}))
    );
    assert!(idempotent_response(&run, "request", &[4; 32]).is_err());
    for hash in [[3; 32], [4; 32]] {
        let mut overwrite = batch();
        overwrite.expected_sequence = 1;
        overwrite.projection.sequence = 2;
        overwrite.events[0].seq = 2;
        overwrite.outbox.clear();
        overwrite.claims.clear();
        overwrite.idempotency[0].request_hash = hash;
        overwrite.idempotency[0].response = json!({"corrupt":true});
        assert!(s.commit(overwrite).await.is_err());
        assert_eq!(s.load(&projection().execution_id).await?, run);
    }

    assert_eq!(s.load(&projection().execution_id).await?, run);
    Ok(())
}
/// Real engine events and a restored compiler snapshot, not a hand-authored projection.
pub async fn replay_equals_projection<F: StoreFactory>(f: &F) -> Result<(), StoreError> {
    use runbook_engine::{Command, ExecutionState, StartContext, apply, decide, prepare_start};
    use std::rc::Rc;
    let source=runbook_spec::parse("apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: replay, title: Replay}\nspec: {steps: [], outputs: {result: healthy}}\n").unwrap();
    let compiled = runbook_spec::compile(&source, &Default::default()).unwrap();
    let bytes = compiled.snapshot_bytes().to_vec();
    let mut p = projection();
    p.compiled_hash = *compiled.hash();
    let initial = ExecutionState::new(p.execution_id.clone(), Rc::new(compiled));
    let first = EventEnvelope {
        execution_id: p.execution_id.clone(),
        seq: 1,
        at_ms: 10,
        actor: ActorRef::System,
        event: prepare_start(&initial, &StartContext::default(), 10).unwrap(),
    };
    let running = apply(&initial, &first).unwrap();
    let Command::CompleteExecution { status, outputs } = decide(&running).unwrap().remove(0) else {
        panic!("empty run completes")
    };
    let last = EventEnvelope {
        execution_id: p.execution_id.clone(),
        seq: 2,
        at_ms: 11,
        actor: ActorRef::System,
        event: Event::ExecutionCompleted { status, outputs },
    };
    let ended = apply(&running, &last).unwrap();
    let store = f.create(p.clone()).await?;
    p.sequence = ended.sequence;
    p.status = ended.status;
    p.phase = ended.phase;
    p.outputs = ended.outputs.clone();
    p.result = ended.result.clone();
    p.completed_at_ms = Some(ended.at_ms);
    store
        .commit(ExecutionCommit {
            execution_id: p.execution_id.clone(),
            expected_sequence: 0,
            events: vec![first, last],
            projection: p.clone(),
            timers: vec![],
            outbox: vec![],
            idempotency: vec![],
            claims: vec![],
            credentials: vec![],
            derived_outbox: vec![],
        })
        .await?;
    let saved = store.load(&p.execution_id).await?;
    let definition = runbook_spec::restore(&bytes).unwrap();
    assert_eq!(*definition.hash(), saved.projection.compiled_hash);
    let mut replay = ExecutionState::new(p.execution_id.clone(), Rc::new(definition));
    for event in &saved.events {
        replay = apply(&replay, event).unwrap();
    }
    assert_eq!(replay, ended);
    assert_eq!(saved.projection, p);
    assert_eq!(saved.projection.sequence, replay.sequence);
    assert_eq!(saved.projection.status, replay.status);
    assert_eq!(saved.projection.result, replay.result);
    assert_eq!(saved.projection.outputs, replay.outputs);
    Ok(())
}

pub(crate) async fn together<A: std::future::Future, B: std::future::Future>(
    a: A,
    b: B,
) -> (A::Output, B::Output) {
    use std::{pin::pin, task::Poll};
    let (mut a, mut b) = (pin!(a), pin!(b));
    let (mut ar, mut br) = (None, None);
    std::future::poll_fn(|cx| {
        if ar.is_none()
            && let Poll::Ready(r) = a.as_mut().poll(cx)
        {
            ar = Some(r);
        }
        if br.is_none()
            && let Poll::Ready(r) = b.as_mut().poll(cx)
        {
            br = Some(r);
        }
        if ar.is_some() && br.is_some() {
            Poll::Ready((ar.take().unwrap(), br.take().unwrap()))
        } else {
            Poll::Pending
        }
    })
    .await
}
