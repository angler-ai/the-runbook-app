mod support;
use runbook_model::{ExecutionStatus, StepStatus};
use support::*;

#[test]
fn end_pins_each_status_and_leaves_following_work_unmaterialized() {
    for (status, expected) in [
        ("succeeded", ExecutionStatus::Succeeded),
        ("failed", ExecutionStatus::Failed),
        ("cancelled", ExecutionStatus::Cancelled),
    ] {
        let source = format!(
            "spec: {{steps: [{{id: stop, end: {{status: {status}, message: Done}}}}, {{id: later, title: Later}}]}}"
        );
        let trace = run_case(&source, &[]);
        assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
        assert_eq!(trace.state.status, expected);
        assert_eq!(trace.step("stop").status, StepStatus::Succeeded);
        assert_eq!(trace.step("stop").summary.as_deref(), Some("Done"));
        assert_eq!(trace.step("later").id, None);
        assert_eq!(trace.state, trace.replay());
    }
}

#[test]
fn end_evaluates_checked_text_and_persists_provenance() {
    let trace = run_case(
        "spec: {inputs: {reason: {type: string, default: all done}}, steps: [{id: stop, end: {status: succeeded, message: 'Finished: ${{ inputs.reason }}'}}]}",
        &[],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(
        trace.step("stop").summary.as_deref(),
        Some("Finished: all done")
    );
    assert!(
        trace
            .step("stop")
            .reads
            .iter()
            .any(|r| r.origin.path == "inputs.reason")
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn end_unwinds_group_then_top_finally_and_preserves_failed_status() {
    let source = "spec:\n  steps:\n    - id: group\n      steps:\n        - {id: stop, end: {status: failed, message: Stopped}}\n        - {id: inner_later, title: Inner later}\n      finally:\n        - {id: group_cleanup, title: Group cleanup}\n    - {id: outer_later, title: Outer later}\n  finally:\n    - {id: top_cleanup, title: Top cleanup}\n";
    let group = run_case(source, &[]);
    assert!(group.error_codes.is_empty(), "{:?}", group.error_codes);
    assert_eq!(group.ready, ["group_cleanup"]);
    let top = run_case(source, &[succeeded("group_cleanup")]);
    assert_eq!(top.ready, ["top_cleanup"]);
    let done = run_case(
        source,
        &[succeeded("group_cleanup"), succeeded("top_cleanup")],
    );
    assert!(done.error_codes.is_empty(), "{:?}", done.error_codes);
    assert_eq!(done.state.status, ExecutionStatus::Failed);
    assert_eq!(done.step("inner_later").id, None);
    assert_eq!(done.step("outer_later").id, None);
    assert_eq!(done.state, done.replay());
}

#[test]
fn end_replay_uses_persisted_message_and_rejects_forged_status() {
    use runbook_engine::{Event, apply};
    use runbook_spec::ir::EndStatus;
    let trace = run_case(
        "spec: {inputs: {reason: {type: string, default: original}}, steps: [{id: stop, end: {status: succeeded, message: '${{ inputs.reason }}'}}]}",
        &[],
    );
    let position = trace
        .events
        .iter()
        .position(|e| matches!(e.event, Event::EndSelected { .. }))
        .unwrap();
    let mut before = trace.events[..position]
        .iter()
        .try_fold(trace.initial.clone(), |state, event| apply(&state, event))
        .unwrap();
    before
        .inputs
        .insert("reason".into(), serde_json::json!("changed"));
    let envelope = &trace.events[position];
    let replayed = apply(&before, envelope).unwrap();
    assert_eq!(
        replayed
            .steps
            .iter()
            .find(|s| s.step_id == "stop")
            .unwrap()
            .summary
            .as_deref(),
        Some("original")
    );
    let mut forged = envelope.clone();
    let Event::EndSelected { selection } = &mut forged.event else {
        unreachable!()
    };
    selection.status = EndStatus::Failed;
    assert_eq!(
        apply(&before, &forged).unwrap_err().code(),
        "end.invalid_selection"
    );
    let Event::EndSelected { selection } = &mut forged.event else {
        unreachable!()
    };
    selection.status = EndStatus::Succeeded;
    selection.base_sequence += 1;
    assert_eq!(
        apply(&before, &forged).unwrap_err().code(),
        "event.sequence_conflict"
    );
}
