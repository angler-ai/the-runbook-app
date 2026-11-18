//! Negative paths and invariants of overrides (PRD section 6, "Overrides,
//! amendments and reruns"), beyond `amendments.rs`.
mod support;
use runbook_engine::*;
use runbook_model::{
    ActorRef, Data, ExecutionStatus, ReportStatus, StepRecord, StepStatus, Verdict,
};
use serde_json::json;
use support::*;

const ADMIN: ActorRef = ActorRef::System;

fn report(step: &str, n: i64) -> Input {
    Input::Report {
        step: step.into(),
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::from([("n".to_string(), json!(n))]),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        at_ms: None,
    }
}
fn record(n: i64) -> StepRecord {
    StepRecord {
        status: StepStatus::Succeeded,
        verdict: None,
        outputs: Data::from([("n".to_string(), json!(n))]),
        summary: None,
    }
}
fn amend(trace: &mut Trace, step: &str, to: StepRecord) {
    let id = trace.step(step).id.clone().unwrap();
    let amendment = validate_override(&trace.state, &id, &to, "corrected", ADMIN).unwrap();
    let before = trace.error_codes.len();
    trace.append(
        Event::StepOverridden {
            step_run_id: id,
            amendment: Box::new(amendment),
        },
        ADMIN,
        trace.state.at_ms + 1,
    );
    assert_eq!(trace.error_codes.len(), before, "{:?}", trace.error_codes);
    trace.settle();
}

const CHAIN: &str = "spec:
  steps:
    - {id: a, title: A, outputs: {n: {type: integer}}}
    - {id: b, title: B}
    - id: later
      if: \"${{ steps.a.outputs.n == 2 }}\"
      title: Later
  outputs: {result: \"${{ steps.a.outputs.n }}\"}";

/// "Overrides do not themselves fire the scheduler or reopen terminal
/// work", while "dependent expressions not yet evaluated read the corrected
/// current record when evaluated".
#[test]
fn running_override_does_not_drive_but_later_guards_read_it() {
    let mut trace = run_case(CHAIN, &[report("a", 1)]);
    let pending = decide(&trace.state).unwrap();
    let steps_before: Vec<_> = trace
        .state
        .steps
        .iter()
        .map(|s| (s.step_id.clone(), s.status))
        .collect();
    let events_before = trace.events.len();
    amend(&mut trace, "a", record(2));
    assert_eq!(trace.events.len(), events_before + 1, "only the override");
    assert_eq!(decide(&trace.state).unwrap(), pending);
    assert_eq!(
        trace
            .state
            .steps
            .iter()
            .map(|s| (s.step_id.clone(), s.status))
            .collect::<Vec<_>>(),
        steps_before
    );
    // `later` was never evaluated, so it reads the corrected n == 2.
    let b = trace.step("b").id.clone().unwrap();
    trace.append(
        Event::StepReported {
            step_run_id: b,
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::new(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        ADMIN,
        trace.state.at_ms + 1,
    );
    trace.settle();
    assert_eq!(trace.step("later").status, StepStatus::Ready);
    assert!(!trace.step("later").stale);
    assert_eq!(trace.state, trace.replay());
}

/// "Completed executions gain an amended marker ... Their recorded terminal
/// status and result stay as they were when the run completed."
#[test]
fn completed_status_and_result_survive_overrides() {
    let mut trace = run_case(CHAIN, &[report("a", 1), succeeded("b")]);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state.result, json!(1));
    assert_eq!(trace.step("later").status, StepStatus::Skipped);
    amend(&mut trace, "a", record(2));
    amend(&mut trace, "a", record(3));
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state.result, json!(1));
    assert_eq!(trace.state.outputs["result"], json!(1));
    // The skipped guard is not re-evaluated, but it consumed the value.
    assert_eq!(trace.step("later").status, StepStatus::Skipped);
    assert!(trace.step("later").stale);
    // History keeps the first record as original and both amendments.
    let a = trace.step("a");
    assert_eq!(a.original.as_ref().unwrap().outputs["n"], json!(1));
    assert_eq!(a.amendments.len(), 2);
    assert_eq!(a.amendments[0].from.outputs["n"], json!(1));
    assert_eq!(a.amendments[1].from.outputs["n"], json!(2));
    assert_eq!(a.outputs["n"], json!(3));
    assert_eq!(trace.count("ExecutionCompleted"), 1);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn overrides_cannot_change_status_or_break_the_task_contract() {
    let trace = run_case(
        "spec: {steps: [{id: probe, title: Probe, check: true, outputs: {n: {type: integer, required: true}}}, {id: b, title: B}]}",
        &[Input::Report {
            step: "probe".into(),
            status: ReportStatus::Succeeded,
            verdict: Some(Verdict::Fail),
            outputs: Data::from([("n".to_string(), json!(1))]),
            actor: ADMIN,
            at_ms: None,
        }],
    );
    let id = trace.step("probe").id.clone().unwrap();
    let base = StepRecord {
        status: StepStatus::Succeeded,
        verdict: Some(Verdict::Pass),
        outputs: Data::from([("n".to_string(), json!(1))]),
        summary: None,
    };
    let code = |to: StepRecord| {
        validate_override(&trace.state, &id, &to, "fix", ADMIN)
            .map(|_| ())
            .map_err(|e| e.code())
    };
    assert_eq!(code(base.clone()), Ok(()));
    assert_eq!(
        code(StepRecord {
            status: StepStatus::Failed,
            ..base.clone()
        }),
        Err("amendment.status_change")
    );
    assert_eq!(
        code(StepRecord {
            verdict: None,
            ..base.clone()
        }),
        Err("step.verdict_required")
    );
    assert_eq!(
        code(StepRecord {
            outputs: Data::new(),
            ..base.clone()
        }),
        Err("step.invalid_outputs")
    );
    assert_eq!(
        code(StepRecord {
            outputs: Data::from([("n".to_string(), json!("one"))]),
            ..base.clone()
        }),
        Err("step.invalid_outputs")
    );
    assert_eq!(
        code(StepRecord {
            outputs: Data::from([("n".to_string(), json!(1)), ("extra".into(), json!(1))]),
            ..base
        }),
        Err("step.invalid_outputs")
    );
}

/// The reducer re-validates the stored fact: a forged or stale amendment
/// (wrong `from`, changed status, empty reason) is refused.
#[test]
fn forged_override_facts_are_rejected() {
    let trace = run_case(CHAIN, &[report("a", 1)]);
    let id = trace.step("a").id.clone().unwrap();
    let genuine = validate_override(&trace.state, &id, &record(2), "fix", ADMIN).unwrap();
    let try_apply = |amendment: runbook_model::Amendment, actor: ActorRef| {
        let envelope = EventEnvelope {
            execution_id: trace.state.id.clone(),
            seq: trace.state.sequence + 1,
            at_ms: trace.state.at_ms,
            actor,
            event: Event::StepOverridden {
                step_run_id: id.clone(),
                amendment: Box::new(amendment),
            },
        };
        apply(&trace.state, &envelope)
            .map(|_| ())
            .map_err(|e| e.code())
    };
    assert_eq!(try_apply(genuine.clone(), ADMIN), Ok(()));
    let mut stale_from = genuine.clone();
    stale_from.from.outputs.insert("n".into(), json!(7));
    assert_eq!(try_apply(stale_from, ADMIN), Err("amendment.from_mismatch"));
    let mut status = genuine.clone();
    status.to.status = StepStatus::Failed;
    assert_eq!(try_apply(status, ADMIN), Err("amendment.status_change"));
    let mut empty_reason = genuine;
    empty_reason.reason = "  ".into();
    assert_eq!(
        try_apply(empty_reason, ADMIN),
        Err("amendment.reason_required")
    );
}

/// A gate's decision record cannot be amended even after it decided; a new
/// execution is the only way to supersede it.
#[test]
fn decided_approval_cannot_be_amended() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Go?, options: [go, stop], approvers: {teams: [oncall]}}}, {id: next, title: Next}]}",
        &[],
        directory,
    );
    let vote = ResolvedVote {
        at_ms: 0,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "tok".into(),
        option: "go".into(),
        comment: None,
        eligibility: VoteEligibility {
            eligible: true,
            directory_revision: 1,
            matched_recipients: vec![runbook_model::RecipientRef::Team(
                "team_00000000000000000000000001".into(),
            )],
        },
    };
    for event in apply_vote(&trace.state, &vote).unwrap() {
        trace.append(event, vote.actor.clone(), 0);
    }
    trace.settle();
    let gate = trace.step("gate");
    assert_eq!(gate.status, StepStatus::Succeeded);
    let to = StepRecord {
        status: StepStatus::Succeeded,
        verdict: None,
        outputs: Data::from([("decision".to_string(), json!("stop"))]),
        summary: None,
    };
    assert_eq!(
        validate_override(&trace.state, gate.id.as_ref().unwrap(), &to, "fix", ADMIN)
            .unwrap_err()
            .code(),
        "amendment.approval_forbidden"
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
}
