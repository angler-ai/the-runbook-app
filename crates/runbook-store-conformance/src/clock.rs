use crate::*;
use runbook_core::session::{ExecutionSession, SessionStore};
use runbook_engine::{
    EventEnvelope, ExecutionState, StartContext, apply, authorize_at_deadline, prepare_start,
};
use runbook_model::ActorRef;
use std::rc::Rc;
pub async fn store_clock_controls_deadline_equality<F: SessionFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let store = f.create(execution::projection()).await?;
    let first = store.begin(&execution::projection().execution_id).await?;
    let deadline = first.transaction_time_ms();
    let source=runbook_spec::parse("apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: clock, title: Clock}\nspec: {execution: {timeout: 1s}, steps: [{id: work, title: Work}]}\n").unwrap();
    let initial = ExecutionState::new(
        execution::projection().execution_id.clone(),
        Rc::new(runbook_spec::compile(&source, &Default::default()).unwrap()),
    );
    let e = EventEnvelope {
        execution_id: initial.id.clone(),
        seq: 1,
        at_ms: deadline - 1000,
        actor: ActorRef::System,
        event: prepare_start(&initial, &StartContext::default(), deadline - 1000).unwrap(),
    };
    let state = apply(&initial, &e).unwrap();
    assert!(authorize_at_deadline(&state, deadline).is_err());
    drop(first);
    for process_offset in [-3_600_000i64, 3_600_000] {
        let before = f.backend_time_ms(&store).await?;
        let session = store.begin(&state.id).await?;
        assert!(session.transaction_time_ms() >= deadline);
        assert_eq!(
            authorize_at_deadline(&state, session.transaction_time_ms())
                .unwrap_err()
                .code(),
            "execution.deadline_exceeded"
        );
        let transaction_time = session.transaction_time_ms();
        let process_time = before + process_offset;
        let mut wrong = execution::batch();
        wrong.events[0].at_ms = process_time;
        wrong.outbox[0].at_ms = transaction_time;
        assert!(session.commit(wrong).await.is_err());
        drop(session);
        let after = f.backend_time_ms(&store).await?;
        assert!(
            (before..=after).contains(&transaction_time),
            "session must use independently measured backend time"
        );
    }
    assert_eq!(store.load(&state.id).await?.revision, 0);
    Ok(())
}
pub async fn session_time_and_snapshot_are_fenced<F: SessionFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let store = f.create(execution::projection()).await?;
    let id = execution::projection().execution_id;
    let session = store.begin(&id).await?;
    let initial = session.load(&id).await?;
    assert_eq!(initial.revision, 0);
    let mut batch = execution::batch();
    let time = session.transaction_time_ms();
    batch.events[0].at_ms = time;
    batch.outbox[0].at_ms = time;
    assert_eq!(
        session.commit(batch.clone()).await?,
        CommitResult::Committed(1)
    );
    assert!(session.commit(batch).await.is_err());
    assert!(session.load(&id).await.is_err());
    let run = store.load(&id).await?;
    assert_eq!(run.events[0].at_ms, time);
    assert_eq!(run.outbox.values().next().unwrap().at_ms, time);
    Ok(())
}

/// A session commits at its own transaction time even after the store
/// clock moved on (actor logic awaits the directory and the snapshot
/// between begin and commit): a replay row stamped at begin is accepted,
/// as Postgres commits at its transaction's start.
pub async fn session_commits_at_its_begin_time<F: SessionFactory>(f: &F) -> Result<(), StoreError> {
    let store = f.create(execution::projection()).await?;
    let id = execution::projection().execution_id;
    let session = store.begin(&id).await?;
    let at = session.transaction_time_ms();
    let replay = runbook_core::actor::Replay {
        key: "retry-1".into(),
        principal: "system".into(),
        request_hash: [5; 32],
    };
    let mut batch = execution::batch();
    batch.events[0].at_ms = at;
    batch.outbox[0].at_ms = at;
    batch
        .idempotency
        .push(replay.record("claim_step:x", &id, at, 1)?);
    f.advance_clock(&store, 5).await?;
    assert_eq!(session.commit(batch).await?, CommitResult::Committed(1));
    let later = store.begin(&id).await?;
    let saved = later
        .actor_replay(&replay.prefix("claim_step:x")?)
        .await?
        .expect("the replay row");
    assert_eq!(saved.response["at_ms"], serde_json::json!(at));
    Ok(())
}
