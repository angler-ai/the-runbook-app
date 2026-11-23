//! LANG4 (later-plans rule 1): `call`. A ready call step evaluates `with`
//! and records `CallRequested` with its child's ID; the step waits. The
//! host records `CallStarted` when the child's start commits and
//! `CallCompleted` with the child's outcome (or the code that refused its
//! start). The step takes the child's status, verdict and declared outputs.
//! Pinned as a conformance trace (`conformance/execution/call.json`).
mod support;
use runbook_engine::{CallOutcome, Event};
use runbook_model::{ActorRef, Data, ExecutionStatus, StepStatus, Verdict};
use runbook_spec::{CompileContext, ir::CallResolution};
use serde_json::json;
use support::*;

const SOURCE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: deploy, title: Deploy}\nspec:\n  inputs:\n    env: {type: string, default: staging}\n  steps:\n    - id: migrate\n      call:\n        runbook: db-migrate\n        with:\n          target: \"${{ inputs.env }}\"\n        outputs:\n          rows: {type: integer}\n    - id: verify\n      title: \"Verify ${{ steps.migrate.outputs.rows }} rows\"\n";

fn context() -> CompileContext {
    let mut c = CompileContext::default();
    c.runbooks.insert(
        "db-migrate".into(),
        CallResolution {
            runbook_id: "rb_00000000000000000000000002".into(),
            version_id: "rbv_00000000000000000000000002".into(),
            version: 1,
            depth: 0,
            reaches: vec![],
            destructive: false,
            agent_decidable: Some(false),
            inputs: [("target".to_string(), true)].into_iter().collect(),
            input_schemas: Default::default(),
            outputs: [("rows".to_string(), runbook_spec::ir::OutputKind::Integer)]
                .into_iter()
                .collect(),
        },
    );
    c
}

fn started() -> Trace {
    run_document(SOURCE, &[], context())
}

fn child() -> runbook_model::ExecutionId {
    CHILD.parse().unwrap()
}

fn outcome(trace: &Trace, status: ExecutionStatus, outputs: Data, error: Option<&str>) -> Event {
    Event::CallCompleted {
        outcome: Box::new(CallOutcome {
            step_run_id: trace.step("migrate").id.clone().unwrap(),
            child_execution_id: child(),
            status,
            verdict: Some(Verdict::Warn),
            outputs,
            error: error.map(str::to_owned),
        }),
    }
}

fn record_start(trace: &mut Trace, at_ms: i64) {
    let step_run_id = trace.step("migrate").id.clone().unwrap();
    trace.append(
        Event::CallStarted {
            step_run_id,
            child_execution_id: child(),
        },
        ActorRef::System,
        at_ms,
    );
}

#[test]
fn a_ready_call_requests_its_child_and_waits() {
    let trace = started();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("CallRequested"), 1);
    let migrate = trace.step("migrate");
    assert_eq!(migrate.status, StepStatus::Waiting);
    let call = migrate.call.as_ref().unwrap();
    assert_eq!(call.child_execution_id, child());
    assert_eq!(call.inputs.get("target"), Some(&json!("staging")));
    assert!(!call.started && !call.completed);
    // Nobody can claim it: it is not an actor's task. The system moves the
    // run (the child, then its result), so it reports `running` (LANG4-24).
    assert!(trace.ready.is_empty());
    assert_eq!(
        runbook_engine::reported_status(&trace.state),
        ExecutionStatus::Running
    );
    assert_eq!(trace.state.status, ExecutionStatus::Running);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn the_childs_success_and_outputs_become_the_steps() {
    let mut trace = started();
    record_start(&mut trace, 5);
    assert!(trace.step("migrate").call.as_ref().unwrap().started);
    let mut outputs = Data::new();
    outputs.insert("rows".into(), json!(42));
    outputs.insert("extra".into(), json!("not declared, not exposed"));
    let event = outcome(&trace, ExecutionStatus::Succeeded, outputs, None);
    trace.append(event, ActorRef::System, 10);
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let migrate = trace.step("migrate");
    assert_eq!(migrate.status, StepStatus::Succeeded);
    assert_eq!(migrate.verdict, Some(Verdict::Warn));
    assert_eq!(migrate.outputs.get("rows"), Some(&json!(42)));
    assert!(!migrate.outputs.contains_key("extra"));
    assert_eq!(trace.ready, ["verify"]);
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("verify").id.clone().unwrap(),
            report: runbook_engine::WorkReport {
                appended_logs: false,
                files: vec![],
                status: runbook_model::ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::new(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        20,
    );
    trace.settle();
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_refused_start_fails_the_step_with_its_code() {
    let mut trace = started();
    let event = outcome(
        &trace,
        ExecutionStatus::Failed,
        Data::new(),
        Some("limit.executions_monthly"),
    );
    trace.append(event, ActorRef::System, 10);
    trace.settle();
    let migrate = trace.step("migrate");
    assert_eq!(migrate.status, StepStatus::Failed);
    assert_eq!(
        migrate.call.as_ref().unwrap().error.as_deref(),
        Some("limit.executions_monthly")
    );
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_failed_or_cancelled_child_or_bad_outputs_fail_the_step() {
    for (status, outputs, code) in [
        (ExecutionStatus::Failed, Data::new(), "call.child_failed"),
        (
            ExecutionStatus::Cancelled,
            Data::new(),
            "call.child_cancelled",
        ),
        (
            ExecutionStatus::Succeeded,
            [("rows".to_string(), json!("many"))].into_iter().collect(),
            "call.outputs_invalid",
        ),
    ] {
        let mut trace = started();
        record_start(&mut trace, 5);
        let event = outcome(&trace, status, outputs, None);
        trace.append(event, ActorRef::System, 10);
        trace.settle();
        let migrate = trace.step("migrate");
        assert_eq!(migrate.status, StepStatus::Failed, "{code}");
        assert_eq!(migrate.call.as_ref().unwrap().error.as_deref(), Some(code));
        assert_eq!(trace.state.status, ExecutionStatus::Failed, "{code}");
        assert_eq!(trace.state, trace.replay());
    }
}

#[test]
fn only_the_waiting_child_and_the_system_can_answer() {
    let mut trace = started();
    // Another child.
    let mut wrong = outcome(&trace, ExecutionStatus::Succeeded, Data::new(), None);
    if let Event::CallCompleted { outcome } = &mut wrong {
        outcome.child_execution_id = "exe_00000000000000000000000009".parse().unwrap();
    }
    trace.append(wrong, ActorRef::System, 10);
    // A person.
    let human = outcome(&trace, ExecutionStatus::Succeeded, Data::new(), None);
    trace.append(
        human,
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        10,
    );
    // The outcome may arrive before the start is recorded (a crash between
    // the two); a later start record is refused, not applied twice.
    let mut outputs = Data::new();
    outputs.insert("rows".into(), json!(1));
    let done = outcome(&trace, ExecutionStatus::Succeeded, outputs, None);
    trace.append(done.clone(), ActorRef::System, 11);
    record_start(&mut trace, 12);
    trace.append(done, ActorRef::System, 13);
    assert_eq!(
        trace.error_codes,
        [
            "call.wrong_child",
            "event.system_actor_required",
            "call.not_waiting",
            "call.not_waiting"
        ]
    );
    assert_eq!(trace.step("migrate").status, StepStatus::Succeeded);
}

#[test]
fn cancelling_the_run_cancels_the_waiting_call_step() {
    let mut trace = started();
    record_start(&mut trace, 5);
    trace.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        10,
    );
    trace.settle();
    assert_eq!(trace.step("migrate").status, StepStatus::Cancelled);
    assert!(!trace.step("migrate").call.as_ref().unwrap().completed);
    assert_eq!(trace.state.status, ExecutionStatus::Cancelled);
}

/// A run whose outputs read a step that never reported can still close as
/// failed or cancelled (the output is null); a succeeded run still needs
/// every value. This lets cancelling a parent cancel such a child.
#[test]
fn a_cancelled_or_failed_run_closes_even_when_its_outputs_are_missing() {
    let spec = "spec:\n  defaults: {actor: agent}\n  steps:\n    - {id: work, title: Work, outputs: {rows: {type: integer, required: true}}}\n  outputs:\n    rows: \"${{ steps.work.outputs.rows }}\"\n";
    let mut trace = run_case(spec, &[]);
    trace.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        ActorRef::System,
        10,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Cancelled);
    assert_eq!(
        trace.state.outputs.get("rows"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(trace.state, trace.replay());
    let failed = run_case(
        spec,
        &[Input::Report {
            step: "work".into(),
            status: runbook_model::ReportStatus::Failed,
            verdict: None,
            outputs: Data::new(),
            actor: ActorRef::Agent {
                principal_id: "runner".into(),
                session_id: "s1".into(),
                name: "bot".into(),
                model: None,
                service_attribution: None,
            },
            at_ms: Some(5),
        }],
    );
    assert!(failed.error_codes.is_empty(), "{:?}", failed.error_codes);
    assert_eq!(failed.state.status, ExecutionStatus::Failed);
}

/// Any evaluation error, not only a missing value, leaves a failed or
/// cancelled run's output null instead of blocking the close.
#[test]
fn a_cancelled_run_closes_even_when_an_output_fails_to_evaluate() {
    let spec = "spec:\n  inputs:\n    items: {type: list, items: {type: integer}, default: []}\n  steps:\n    - {id: work, title: Work}\n  outputs:\n    n: \"${{ inputs.items[3] }}\"\n";
    let mut trace = run_case(spec, &[]);
    trace.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        ActorRef::System,
        10,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Cancelled);
    assert_eq!(trace.state.outputs.get("n"), Some(&serde_json::Value::Null));
    assert_eq!(trace.state, trace.replay());
}
