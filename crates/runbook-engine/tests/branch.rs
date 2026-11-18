mod support;
use runbook_model::StepStatus;
use support::*;
#[test]
fn false_guard_is_condition_skip() {
    let trace = run_case(
        "spec: {steps: [{id: a, title: A, if: '${{ false }}'}, {id: b, title: B}]}",
        &[],
    );
    assert_eq!(trace.step("a").status, StepStatus::Skipped);
    assert_eq!(trace.ready, ["b"]);
    assert_eq!(trace.count("StepSkipped"), 1);
    assert_eq!(trace.count("StepReady"), 1);
    assert!(trace.step("a").verdict.is_none());
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn switch_uses_first_matching_case() {
    let trace = run_case(
        "spec:\n  steps:\n    - id: choose\n      switch:\n        cases:\n          - {when: '${{ true }}', steps: [{id: a, title: A}]}\n          - {when: '${{ true }}', steps: [{id: b, title: B}]}\n        default: {steps: [{id: c, title: C}]}\n",
        &[],
    );
    assert_eq!(trace.ready, ["a"]);
    assert_eq!(trace.count("BranchSelected"), 1);
    assert!(trace.step("b").id.is_none());
    assert!(trace.step("c").id.is_none());
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn true_guard_is_recorded_once_and_keeps_reads() {
    let mut trace = run_case(
        "spec: {inputs: {allowed: {type: boolean, default: true}}, steps: [{id: a, title: A, if: '${{ inputs.allowed }}'}]}",
        &[],
    );
    assert_eq!(trace.ready, ["a"]);
    assert_eq!(trace.step("a").reads[0].origin.path, "inputs.allowed");
    trace
        .state
        .inputs
        .insert("allowed".into(), serde_json::json!(false));
    assert!(runbook_engine::decide(&trace.state).unwrap().is_empty());
    assert_eq!(trace.count("StepReady"), 1);
}
#[test]
fn selected_branch_finishes_before_following_task() {
    let spec = "spec:\n  steps:\n    - id: choose\n      switch:\n        cases: [{when: '${{ true }}', steps: [{id: a, title: A}]}]\n        default: {steps: [{id: unused, title: Unused}]}\n    - {id: b, title: B}\n";
    let trace = run_case(spec, &[succeeded("a")]);
    assert_eq!(trace.ready, ["b"]);
    assert_eq!(trace.step("choose").status, StepStatus::Succeeded);
    assert_eq!(trace.count("StepCompleted"), 1);
    assert!(trace.step("unused").id.is_none());
    assert_eq!(trace.state, trace.replay());
    let done = run_case(spec, &[succeeded("a"), succeeded("b")]);
    assert_eq!(done.state.status, runbook_model::ExecutionStatus::Succeeded);
    assert_eq!(done.count("BranchSelected"), 1);
}
#[test]
fn switch_default_and_empty_default_are_explicit() {
    for default in ["{steps: [{id: chosen, title: Default}]}", "{steps: []}"] {
        let source = format!(
            "spec:\n  steps:\n    - id: choose\n      switch:\n        cases: [{{when: '${{{{ false }}}}', steps: [{{id: unused, title: Unused}}]}}]\n        default: {default}\n"
        );
        let trace = run_case(&source, &[]);
        assert_eq!(
            trace.step("choose").branch,
            Some(runbook_engine::BranchChoice::Default)
        );
        assert!(trace.step("unused").id.is_none());
        if default.contains("chosen") {
            assert_eq!(trace.ready, ["chosen"]);
        } else {
            assert_eq!(
                trace.state.status,
                runbook_model::ExecutionStatus::Succeeded
            );
            assert_eq!(trace.step("choose").verdict, None);
        }
    }
}
#[test]
fn first_match_avoids_later_errors_and_records_only_evaluated_reads() {
    let trace = run_case(
        "spec:\n  inputs: {first: {type: boolean, default: true}, missing: {type: boolean}}\n  steps:\n    - id: choose\n      switch:\n        cases:\n          - {when: '${{ inputs.first }}', steps: [{id: a, title: A}]}\n          - {when: '${{ inputs.missing }}', steps: [{id: b, title: B}]}\n",
        &[],
    );
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.ready, ["a"]);
    assert_eq!(trace.step("choose").reads.len(), 1);
    assert_eq!(trace.step("choose").reads[0].origin.path, "inputs.first");
    let failed = run_case(
        "spec:\n  inputs: {missing: {type: boolean}}\n  steps:\n    - id: choose\n      switch:\n        cases:\n          - {when: '${{ inputs.missing }}', steps: [{id: a, title: A}]}\n          - {when: '${{ true }}', steps: [{id: b, title: B}]}\n",
        &[],
    );
    assert_eq!(failed.error_codes, ["expression.missing_value"]);
    assert_eq!(failed.count("BranchSelected"), 0);
    assert!(failed.step("a").id.is_none());
    assert!(failed.step("b").id.is_none());
}
#[test]
fn skipped_control_never_materializes_children() {
    let trace = run_case(
        "spec:\n  steps:\n    - id: choose\n      if: '${{ false }}'\n      switch:\n        cases: [{when: '${{ true }}', steps: [{id: never, title: Never}]}]\n    - id: change\n      if: '${{ false }}'\n      set: {vars: {}}\n",
        &[],
    );
    assert_eq!(trace.step("choose").status, StepStatus::Skipped);
    assert_eq!(trace.step("change").status, StepStatus::Skipped);
    assert_eq!(trace.count("BranchSelected"), 0);
    assert_eq!(trace.count("DataChanged"), 0);
    assert!(trace.step("never").id.is_none());
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
}
#[test]
fn nested_switches_preserve_scope_and_roll_up_findings() {
    let mut input = succeeded("a");
    let Input::Report { verdict, .. } = &mut input else {
        unreachable!()
    };
    *verdict = Some(runbook_model::Verdict::Warn);
    let trace = run_case(
        "spec:\n  steps:\n    - id: outer\n      switch:\n        cases:\n          - when: '${{ true }}'\n            steps:\n              - id: inner\n                switch:\n                  cases: [{when: '${{ true }}', steps: [{id: a, title: Check, check: true}]}]\n                  default: {steps: [{id: never, title: Never}]}\n  outputs: {result: '${{ steps.outer.verdict }}'}",
        &[input],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(
        trace.step("inner").verdict,
        Some(runbook_model::Verdict::Warn)
    );
    assert_eq!(
        trace.step("outer").verdict,
        Some(runbook_model::Verdict::Warn)
    );
    assert_eq!(trace.state.result, serde_json::json!("warn"));
    assert_eq!(trace.state, trace.replay());
    assert_eq!(trace.count("BranchSelected"), 2);
}
#[test]
fn branch_facts_are_fenced_and_unchosen_work_cannot_be_readied() {
    use runbook_engine::{Event, EventEnvelope, apply};
    use runbook_model::ActorRef;
    let trace = run_case(
        "spec:\n  steps:\n    - id: choose\n      switch:\n        cases: [{when: '${{ true }}', steps: [{id: a, title: A}]}]\n        default: {steps: [{id: never, title: Never}]}\n",
        &[],
    );
    let base = EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 0,
        actor: ActorRef::System,
        event: Event::StepReady {
            step_id: "never".into(),
            step_run_id: "sr_00000000000000000000000099".parse().unwrap(),
            reads: vec![],
        },
    };
    assert_eq!(
        apply(&trace.state, &base).unwrap_err().code(),
        "step.not_ready"
    );
    let mut completed = base.clone();
    completed.event = Event::StepCompleted {
        status: StepStatus::Succeeded,
        step_run_id: trace.step("choose").id.clone().unwrap(),
        verdict: None,
    };
    assert_eq!(
        apply(&trace.state, &completed).unwrap_err().code(),
        "step.not_complete"
    );
    let mut stale = base;
    stale.event = Event::BranchSelected {
        selection: runbook_engine::BranchSelected {
            step_run_id: trace.step("choose").id.clone().unwrap(),
            base_sequence: trace.state.sequence - 1,
            choice: runbook_engine::BranchChoice::Default,
            reads: vec![],
        },
    };
    assert_eq!(
        apply(&trace.state, &stale).unwrap_err().code(),
        "event.sequence_conflict"
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn guarded_assignments_preserve_guard_and_rhs_reads() {
    let trace = run_case(
        "spec: {inputs: {enabled: {type: boolean, default: true}, value: {type: string, default: changed}}, vars: {value: before}, steps: [{id: assign, if: '${{ inputs.enabled }}', set: {vars: {value: '${{ inputs.value }}'}}}]}",
        &[],
    );
    let paths: Vec<_> = trace
        .step("assign")
        .reads
        .iter()
        .map(|r| r.origin.path.as_str())
        .collect();
    assert_eq!(paths, ["inputs.enabled", "inputs.value"]);
    assert_eq!(trace.state.vars["value"], serde_json::json!("changed"));
    assert_eq!(trace.state, trace.replay());
}
