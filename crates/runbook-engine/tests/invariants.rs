mod support;
use runbook_engine::{Event, EventEnvelope, WorkReport, apply};
use runbook_model::{ActorRef, ReportStatus};
use support::*;
#[test]
fn event_naming_nonexistent_step_returns_invariant() {
    let trace = run_case("spec: {steps: [{id: a, title: A}]}", &[]);
    let envelope = EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 0,
        actor: ActorRef::System,
        event: Event::StepReported {
            step_run_id: "sr_00000000000000000000000999".parse().unwrap(),
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
    };
    assert_eq!(
        apply(&trace.state, &envelope).unwrap_err().code(),
        "engine.invariant"
    );
}
#[test]
fn waiting_timer_without_run_id_returns_invariant() {
    let mut trace = run_case(
        "spec: {steps: [{id: repeat, loop: {max_iterations: 2, delay: 1m, until: '${{ false }}', steps: []}}]}",
        &[],
    );
    trace.state.steps[0].id = None;
    assert!(matches!(
        runbook_engine::due_timers(&trace.state, 60_000),
        Err(runbook_engine::EngineError::Invariant {
            context: "waiting timer has no step run ID"
        })
    ));
}
#[test]
fn missing_repeated_child_returns_error_without_panicking() {
    let mut trace = run_case(
        "spec: {steps: [{id: repeat, loop: {max_iterations: 2, until: '${{ false }}', steps: [{id: a, title: A}]}}]}",
        &[],
    );
    trace.state.steps.retain(|s| s.node_id != "a");
    assert!(matches!(
        runbook_engine::decide(&trace.state),
        Err(runbook_engine::EngineError::Invariant {
            context: "scoped step record is absent"
        })
    ));
}
