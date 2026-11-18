//! CF12b review round 2: log evidence counts the lines appended through the
//! logs route, not only the lines a report carries. The core asks its store
//! whether the step run has a non-blank appended line and passes the answer
//! in; the event records it (`appended_logs`), so replay re-checks the
//! same rule without the store.
mod support;
use runbook_engine::{Event, WorkReport, validate_report_with_files, validate_report_with_logs};
use runbook_model::{ActorRef, Data, ReportRequest, ReportStatus, StepStatus};
use support::*;

const SPEC: &str = "spec:\n  steps:\n    - id: build\n      title: Build\n      evidence: {required: [log]}\n    - id: ship\n      title: Ship\n      evidence: {required: [link], accepted: [link]}\n";

fn report(trace: &Trace, step: &str, evidence: &[&str]) -> ReportRequest {
    ReportRequest {
        step_run_id: trace.step(step).id.clone().unwrap(),
        claim: runbook_model::ClaimRef {
            id: "clm_00000000000000000000000001".parse().unwrap(),
            generation: 1.try_into().unwrap(),
        },
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::new(),
        summary: None,
        evidence: evidence.iter().map(|e| e.to_string()).collect(),
        logs: vec![],
    }
}
fn node<'a>(trace: &'a Trace, step: &str) -> &'a runbook_spec::ir::Node {
    trace.state.node(&trace.step(step).step_id).unwrap()
}

#[test]
fn appended_lines_satisfy_required_log_evidence() {
    let mut trace = run_case(SPEC, &[]);
    // Without appended lines a report with no logs is refused, as before.
    assert_eq!(
        validate_report_with_files(
            node(&trace, "build"),
            &report(&trace, "build", &[]),
            trace.step("build"),
            vec![]
        )
        .unwrap_err()
        .code(),
        "step.evidence_required"
    );
    assert_eq!(
        validate_report_with_logs(
            node(&trace, "build"),
            &report(&trace, "build", &[]),
            trace.step("build"),
            vec![],
            false,
        )
        .unwrap_err()
        .code(),
        "step.evidence_required"
    );
    // With them it is accepted, and the event says so.
    let accepted = validate_report_with_logs(
        node(&trace, "build"),
        &report(&trace, "build", &[]),
        trace.step("build"),
        vec![],
        true,
    )
    .unwrap();
    assert!(accepted.work().appended_logs);
    let event = serde_json::to_value(accepted.clone().into_event()).unwrap();
    assert_eq!(event["report"]["appended_logs"], true, "{event}");
    trace.append(accepted.into_event(), ActorRef::System, 10);
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("build").status, StepStatus::Succeeded);
    // Appended lines are not report evidence of a kind the step refuses:
    // `ship` accepts links only, and its agent appended lines.
    let accepted = validate_report_with_logs(
        node(&trace, "ship"),
        &report(&trace, "ship", &["https://example.test/release"]),
        trace.step("ship"),
        vec![],
        true,
    )
    .unwrap();
    trace.append(accepted.into_event(), ActorRef::System, 20);
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("ship").status, StepStatus::Succeeded);
    // Replay re-checks the recorded reports and agrees.
    assert_eq!(trace.state, trace.replay());
}

/// A report that carried no logs and recorded no appended lines does not
/// apply on replay; a report without the flag keeps its exact bytes.
#[test]
fn replay_rechecks_the_recorded_flag() {
    let mut trace = run_case(SPEC, &[]);
    let forged = WorkReport {
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::new(),
        summary: None,
        evidence: vec![],
        logs: vec![],
        files: vec![],
        appended_logs: false,
    };
    let text = serde_json::to_string(&forged).unwrap();
    assert!(!text.contains("appended_logs"), "{text}");
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("build").id.clone().unwrap(),
            report: forged,
        },
        ActorRef::System,
        5,
    );
    assert_eq!(trace.error_codes, ["step.evidence_required"]);
}
