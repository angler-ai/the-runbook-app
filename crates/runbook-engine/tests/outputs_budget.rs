//! CF13 review round 1, item 1: a run's outputs together are held to
//! [`MAX_OUTPUTS_BYTES`] (engine version 5), so the rows that carry them
//! (the run's head, its `ExecutionCompleted`, a parent's `CallCompleted`
//! and call step) stay within one Durable Object row on Workers, and both
//! editions end such a run alike:
//! - a run that would succeed with more fails with
//!   `execution.outputs_too_large`, its outputs past the budget null;
//! - a failed or cancelled run closes with its outputs past the budget null;
//! - runs recorded before version 5 replay unchanged.
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, Data, ExecutionStatus, ReportStatus};
use serde_json::{Value, json};
use support::*;

const BLOB: usize = 60_000;

/// 40 outputs, each a copy of `work`'s 60,000-byte output: about 2.4 MB
/// together. `then` follows `work`.
fn wide(then: &str, finally: &str) -> String {
    let outputs: Vec<String> = (1..=40)
        .map(|n| format!("o{n:02}: '${{{{ steps.work.outputs.blob }}}}'"))
        .collect();
    format!(
        "spec:\n  steps:\n    - {{id: work, title: Work, outputs: {{blob: {{type: string}}}}}}\n    - {then}\n{finally}  outputs: {{{}}}",
        outputs.join(", ")
    )
}

fn report(step: &str, status: ReportStatus, outputs: Data) -> Input {
    Input::Report {
        step: step.into(),
        status,
        verdict: None,
        outputs,
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        at_ms: None,
    }
}

fn blob() -> Input {
    report(
        "work",
        ReportStatus::Succeeded,
        Data::from([("blob".into(), json!("b".repeat(BLOB)))]),
    )
}

/// The outputs the run recorded on its `ExecutionCompleted`.
fn completed(trace: &Trace) -> ExecutionOutputs {
    trace
        .events
        .iter()
        .find_map(|e| match &e.event {
            Event::ExecutionCompleted { outputs, .. } => Some(outputs.clone()),
            _ => None,
        })
        .expect("the run completed")
}

/// The run closed within the budget: every output is present, the ones
/// that fit keep the value and the rest are null.
fn assert_within_budget(trace: &Trace) {
    let outputs = completed(trace);
    assert!(
        outputs_bytes(&outputs) <= MAX_OUTPUTS_BYTES,
        "{} bytes recorded",
        outputs_bytes(&outputs)
    );
    assert_eq!(outputs.values.len(), 40, "every output is present");
    let kept = outputs.values.values().filter(|v| !v.is_null()).count();
    assert!(kept > 0 && kept < 40, "{kept} kept");
    // The first outputs (in name order) keep their values; the rest are null.
    let first_null = outputs.values.values().position(Value::is_null).unwrap();
    assert!(outputs.values.values().skip(first_null).all(Value::is_null));
    assert_eq!(trace.state.outputs, outputs.values);
    assert_eq!(trace.state, trace.replay());
}

/// The budget holds the rows that carry the outputs under one Durable
/// Object row by construction: the head keeps the outputs and `result`
/// (a copy of one of them) in the stored (tagged) form the budget
/// measures, which is never shorter than plain JSON.
#[test]
fn the_budget_keeps_every_row_that_carries_outputs_under_a_row() {
    const ROW: usize = 2_000_000;
    const SLACK: usize = 4_096;
    const REST: usize = 65_536;
    const { assert!(2 * MAX_OUTPUTS_BYTES + REST + SLACK < ROW) };
    let value = json!({"a": [1, "x", null, true, {"b": 2.5}], "c": "q\"\u{1}"});
    let outputs = ExecutionOutputs {
        values: Data::from([("result".into(), value.clone())]),
        reads: vec![],
    };
    assert!(outputs_bytes(&outputs) >= serde_json::to_vec(&value).unwrap().len());
    assert!(outputs_bytes(&outputs) >= serde_json::to_vec(&outputs).unwrap().len());
}

#[test]
fn a_run_whose_outputs_pass_the_budget_fails_instead_of_succeeding() {
    let trace = run_case(
        &wide("{id: last, title: Last}", ""),
        &[blob(), succeeded("last")],
    );
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(
        trace.state.terminal_reason.as_deref(),
        Some(OUTPUTS_TOO_LARGE)
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_within_budget(&trace);
}

#[test]
fn outputs_within_the_budget_still_succeed() {
    let spec = "spec:\n  steps:\n    - {id: work, title: Work, outputs: {blob: {type: string}}}\n  outputs: {a: '${{ steps.work.outputs.blob }}', b: '${{ steps.work.outputs.blob }}'}";
    let trace = run_case(spec, &[blob()]);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state.outputs["b"].as_str().map(str::len), Some(BLOB));
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_failing_run_past_the_budget_closes_with_its_late_outputs_null() {
    let trace = run_case(
        &wide("{id: last, title: Last}", ""),
        &[blob(), report("last", ReportStatus::Failed, Data::new())],
    );
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_within_budget(&trace);
}

#[test]
fn a_run_past_the_budget_can_be_cancelled() {
    let mut trace = run_case(&wide("{id: last, title: Last}", ""), &[blob()]);
    trace.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        trace.state.at_ms,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Cancelled);
    assert_within_budget(&trace);
}

/// A run whose close is driven by a timer (here its last step is a wait)
/// completes; it does not refuse the timer's commit.
#[test]
fn a_timer_driven_close_past_the_budget_completes() {
    let trace = run_case(
        &wide("{id: pause, wait: {for: 1m}}", ""),
        &[blob(), Input::Tick { at_ms: 60_000 }],
    );
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(
        trace.state.terminal_reason.as_deref(),
        Some(OUTPUTS_TOO_LARGE)
    );
    assert_within_budget(&trace);
}

/// A run with cleanup evaluates its outputs after cleanup: past the budget
/// it fails there.
#[test]
fn a_run_with_cleanup_past_the_budget_fails_after_cleanup() {
    let trace = run_case(
        &wide(
            "{id: last, title: Last}",
            "  finally: [{id: tidy, title: Tidy}]\n",
        ),
        &[blob(), succeeded("last"), succeeded("tidy")],
    );
    assert_eq!(
        trace.step("tidy").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(
        trace.state.terminal_reason.as_deref(),
        Some(OUTPUTS_TOO_LARGE)
    );
    assert_within_budget(&trace);
}

/// The reducer holds a version 5 run's recorded outputs to the budget too.
#[test]
fn a_forged_completion_past_the_budget_is_rejected() {
    let mut trace = run_case(&wide("{id: last, title: Last}", ""), &[blob()]);
    let big = json!("b".repeat(BLOB));
    let values: Data = (1..=40)
        .map(|n| (format!("o{n:02}"), big.clone()))
        .collect();
    let id = trace.step("last").id.clone().unwrap();
    trace.append(
        Event::StepReported {
            step_run_id: id,
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
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        0,
    );
    trace.append(
        Event::ExecutionCompleted {
            status: ExecutionStatus::Succeeded,
            outputs: ExecutionOutputs {
                values,
                reads: vec![],
            },
        },
        ActorRef::System,
        0,
    );
    assert_eq!(trace.error_codes, [OUTPUTS_TOO_LARGE]);
}

/// A run recorded before version 5 keeps its whole outputs: its log,
/// which completed with them, replays unchanged, and it still completes
/// that way.
#[test]
fn runs_before_engine_version_5_keep_outputs_past_the_budget() {
    let new = run_case(&wide("{id: last, title: Last}", ""), &[blob()]);
    let mut trace = new;
    let Event::ExecutionStarted { engine_version, .. } = &mut trace.events[0].event else {
        panic!("first event starts the run")
    };
    *engine_version = 4;
    trace.state = trace
        .events
        .iter()
        .try_fold(trace.initial.clone(), |state, event| apply(&state, event))
        .unwrap();
    let id = trace.step("last").id.clone().unwrap();
    trace.append(
        Event::StepReported {
            step_run_id: id,
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
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        0,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    let outputs = completed(&trace);
    assert!(
        outputs
            .values
            .values()
            .all(|v| v.as_str().map(str::len) == Some(BLOB))
    );
    assert!(outputs_bytes(&outputs) > MAX_OUTPUTS_BYTES);
    assert_eq!(trace.state, trace.replay());
}
