//! Review F95: a checked-in event log recorded by engine version 2. Every
//! later release must replay it to the same state, or bump the engine
//! version and keep version-2 rules for runs that recorded it. Never
//! regenerate these files; add a new golden for a new version instead.
use runbook_engine::{EventEnvelope, ExecutionState, apply};
use serde_json::{Value, json};
use std::rc::Rc;

fn summary(state: &ExecutionState) -> Value {
    json!({
        "engine_version": state.engine_version,
        "sequence": state.sequence,
        "status": state.status,
        "phase": state.phase,
        "terminal_reason": state.terminal_reason,
        "result": state.result,
        "outputs": state.outputs,
        "vars": state.vars,
        "steps": state.steps.iter().map(|s| json!({
            "step_id": s.step_id,
            "id": s.id,
            "status": s.status,
            "verdict": s.verdict,
            "outputs": s.outputs,
            "summary": s.summary,
            "attempt": s.attempt,
            "attempts": s.attempt_history.iter().map(|a| json!({
                "attempt": a.attempt,
                "timed_out": a.timed_out,
                "status": a.report.status,
            })).collect::<Vec<_>>(),
            "actor": s.actor,
            "finished_at_ms": s.finished_at_ms,
            "deadline_at_ms": s.deadline_at_ms,
            "stale": s.stale,
            "amended": s.amended,
        })).collect::<Vec<_>>(),
    })
}

#[test]
fn version_2_golden_log_replays_to_the_recorded_state() {
    let source = include_str!("fixtures/golden-v2/runbook.yaml");
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let compiled =
        runbook_spec::compile(&runbook_spec::parse(source).unwrap(), &directory).unwrap();
    let events: Vec<EventEnvelope> =
        serde_json::from_str(include_str!("fixtures/golden-v2/events.json")).unwrap();
    let state = events
        .iter()
        .try_fold(
            ExecutionState::new(events[0].execution_id.clone(), Rc::new(compiled)),
            |state, event| apply(&state, event),
        )
        .unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/golden-v2/state.json")).unwrap();
    assert_eq!(summary(&state), expected);
    assert_eq!(state.engine_version, 2);
}
