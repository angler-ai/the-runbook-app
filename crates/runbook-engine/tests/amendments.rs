//! Amendment history, staleness propagation and replay (B9a).
mod support;
use runbook_engine::{Event, validate_override};
use runbook_model::{ActorRef, Data, ReportStatus, StepRecord, StepStatus};
use support::Input;

fn report(step: &str, n: i64) -> Input {
    Input::Report {
        step: step.into(),
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::from([("n".to_string(), serde_json::json!(n))]),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        at_ms: None,
    }
}
const ADMIN: ActorRef = ActorRef::System;

/// a feeds b (set var) feeds c (reads var); a switch already took its
/// branch from a's output. Amending a marks b and c stale, preserves the
/// original record, and never moves the taken branch.
#[test]
fn amendment_preserves_history_and_marks_consumers() {
    let spec = r#"spec:
  vars: {v: 0, w: 0}
  steps:
    - id: a
      title: A
      outputs: {n: {type: integer}}
    - id: assign
      set: {vars: {v: "${{ steps.a.outputs.n }}"}}
    - id: choose
      switch:
        cases:
          - when: "${{ steps.a.outputs.n == 1 }}"
            steps: [{id: chosen, title: Chosen, with: {x: "${{ vars.v }}"}}]
        default:
          steps: [{id: other, title: Other}]
    - id: copy
      set: {vars: {w: "${{ vars.v }}"}}"#;
    let mut trace = support::run_case(spec, &[report("a", 1), support::succeeded("chosen")]);
    assert_eq!(trace.step("chosen").status, StepStatus::Succeeded);
    let a = trace.step("a").id.clone().unwrap();
    let to = StepRecord {
        status: StepStatus::Succeeded,
        verdict: None,
        outputs: Data::from([("n".to_string(), serde_json::json!(2))]),
        summary: Some("corrected".into()),
    };
    let amendment = validate_override(&trace.state, &a, &to, "wrong count", ADMIN).unwrap();
    assert_eq!(amendment.from.outputs["n"], serde_json::json!(1));
    trace.append(
        Event::StepOverridden {
            step_run_id: a.clone(),
            amendment: Box::new(amendment),
        },
        ADMIN,
        trace.state.at_ms + 1000,
    );
    trace.settle();
    let amended = trace.step("a");
    assert!(amended.amended);
    assert_eq!(amended.outputs["n"], serde_json::json!(2));
    assert_eq!(
        amended.original.as_ref().unwrap().outputs["n"],
        serde_json::json!(1)
    );
    assert_eq!(amended.amendments.len(), 1);
    assert!(trace.step("assign").stale, "direct consumer is stale");
    assert!(trace.step("copy").stale, "transitive var consumer is stale");
    // The taken branch stays taken; nothing reruns or recomputes.
    assert_eq!(trace.step("chosen").status, StepStatus::Succeeded);
    assert!(
        !trace
            .state
            .steps
            .iter()
            .any(|s| s.step_id == "other" && s.status == StepStatus::Succeeded),
        "the untaken branch stays untaken"
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn amendment_rejects_unfinished_and_approval_steps() {
    let spec = "spec: {steps: [{id: work, title: Work}]}";
    let trace = support::run_case(spec, &[]);
    let work = trace.step("work").id.clone().unwrap();
    let to = StepRecord {
        status: StepStatus::Ready,
        verdict: None,
        outputs: Data::new(),
        summary: None,
    };
    assert_eq!(
        validate_override(&trace.state, &work, &to, "fix", ADMIN)
            .unwrap_err()
            .code(),
        "amendment.step_not_finished"
    );
    assert_eq!(
        validate_override(&trace.state, &work, &to, " ", ADMIN)
            .unwrap_err()
            .code(),
        "amendment.reason_required"
    );
    // Approvals can never be amended, finished or not.
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let gate = support::run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [keep], approvers: {teams: [oncall]}}}]}",
        &[],
        directory,
    );
    let gate_run = gate.step("gate").id.clone().unwrap();
    assert_eq!(
        validate_override(&gate.state, &gate_run, &to, "fix", ADMIN)
            .unwrap_err()
            .code(),
        "amendment.approval_forbidden"
    );
}
