//! UP1 (later-plans rule 1): `evidence.required`/`accepted` with `file` and
//! `screenshot`. A report attaches finalized uploads by `evd_` ID; the core
//! resolves each to its verified type and the event records both, so replay
//! re-checks the same rules without the store. Pinned as a conformance trace
//! (`conformance/execution/evidence-files.json`).
mod support;
use runbook_engine::{Event, validate_report, validate_report_with_files};
use runbook_model::{
    ActorRef, Data, EvidenceFileRef, ExecutionStatus, ReportRequest, ReportStatus, StepStatus,
};
use support::*;

const SPEC: &str = "spec:\n  steps:\n    - id: shot\n      title: Capture the dashboard\n      evidence: {required: [screenshot], accepted: [screenshot, link]}\n    - id: record\n      title: Attach the change record\n      evidence: {required: [file]}\n";

fn file(n: u8, content_type: &str) -> EvidenceFileRef {
    EvidenceFileRef {
        id: format!("evd_{n:026}").parse().unwrap(),
        content_type: content_type.into(),
    }
}
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
fn code(trace: &Trace, step: &str, evidence: &[&str], files: &[EvidenceFileRef]) -> &'static str {
    validate_report_with_files(
        node(trace, step),
        &report(trace, step, evidence),
        trace.step(step),
        files.to_vec(),
    )
    .unwrap_err()
    .code()
}

/// The full flow, pinned as a checked-in trace.
fn run() -> Trace {
    let mut trace = run_case(SPEC, &[]);
    let png = file(1, "image/png");
    let pdf = file(2, "application/pdf");
    let (one, two) = (png.id.to_string(), pdf.id.to_string());
    // `screenshot` needs an image; a link alone does not satisfy it.
    assert_eq!(
        code(&trace, "shot", &["https://example.test/dash"], &[]),
        "step.evidence_required"
    );
    // A PDF is a file, not a screenshot, and `file` is not accepted here.
    assert_eq!(
        code(&trace, "shot", &[&two], std::slice::from_ref(&pdf)),
        "step.evidence_not_accepted"
    );
    // An evd_ entry the core did not resolve, or a resolved file the report
    // does not name, is refused.
    assert_eq!(code(&trace, "shot", &[&one], &[]), "step.evidence_unknown");
    assert_eq!(
        code(&trace, "shot", &[], std::slice::from_ref(&png)),
        "step.evidence_unknown"
    );
    // Without the files the plain validator refuses evd_ entries too.
    assert_eq!(
        validate_report(
            node(&trace, "shot"),
            &report(&trace, "shot", &[&one]),
            trace.step("shot")
        )
        .unwrap_err()
        .code(),
        "step.evidence_unknown"
    );
    let accepted = validate_report_with_files(
        node(&trace, "shot"),
        &report(&trace, "shot", &["https://example.test/dash", &one]),
        trace.step("shot"),
        vec![png.clone()],
    )
    .unwrap();
    trace.append(accepted.into_event(), ActorRef::System, 10);
    trace.settle();
    assert_eq!(trace.step("shot").status, StepStatus::Succeeded);
    assert_eq!(trace.step("shot").files, std::slice::from_ref(&png));
    // Any upload satisfies `file`, an image included.
    assert_eq!(code(&trace, "record", &[], &[]), "step.evidence_required");
    let three = file(3, "image/jpeg");
    let accepted = validate_report_with_files(
        node(&trace, "record"),
        &report(&trace, "record", &[&three.id.to_string()]),
        trace.step("record"),
        vec![three],
    )
    .unwrap();
    trace.append(accepted.into_event(), ActorRef::System, 20);
    trace.settle();
    trace
}

#[test]
fn file_and_screenshot_evidence_is_required_and_accepted_by_kind() {
    let trace = run();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}

/// Replay re-validates recorded reports: an event whose only file is a PDF
/// does not apply to a step that accepts screenshots and links only.
#[test]
fn replay_rechecks_recorded_file_types() {
    let mut trace = run_case(SPEC, &[]);
    let pdf = file(2, "application/pdf");
    let forged = runbook_engine::WorkReport {
        appended_logs: false,
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::new(),
        summary: None,
        evidence: vec![pdf.id.to_string()],
        logs: vec![],
        files: vec![pdf],
    };
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("shot").id.clone().unwrap(),
            report: forged,
        },
        ActorRef::System,
        5,
    );
    assert_eq!(trace.error_codes, ["step.evidence_not_accepted"]);
    assert!(
        trace
            .events
            .iter()
            .all(|e| e.event.name() != "StepReported")
    );
}

#[test]
fn evidence_trace_matches_checked_in_contract() {
    use serde_json::json;
    let trace = run();
    let fixture = json!({
        "format": "pure-engine-trace-v1",
        "integration_seams": {"uploads_and_finalize": "UP1", "magic_bytes": "UP3"},
        "runbook": SPEC,
        "events": trace.events,
        "commands": trace.commands,
        "status": trace.state.status,
        "files": trace.state.steps.iter().map(|s| json!({"step": s.step_id, "files": s.files})).collect::<Vec<_>>(),
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/execution/evidence-files.json");
    if std::env::var_os("RUNBOOK_UPDATE_FIXTURES").is_some() {
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
        )
        .unwrap();
    }
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, fixture, "{}", path.display());
}

/// `screenshot` is not satisfied by a non-image file even where files are
/// accepted; `file` and `link` requirements are independent.
#[test]
fn only_images_satisfy_screenshot_requirements() {
    let trace = run_case(
        "spec: {steps: [{id: a, title: A, evidence: {required: [screenshot, link], accepted: [file, link]}}]}",
        &[],
    );
    let pdf = file(2, "application/pdf");
    let png = file(1, "image/png");
    let link = "https://example.test/ticket";
    assert_eq!(
        code(
            &trace,
            "a",
            &[&pdf.id.to_string(), link],
            std::slice::from_ref(&pdf)
        ),
        "step.evidence_required"
    );
    assert_eq!(
        code(
            &trace,
            "a",
            &[&png.id.to_string()],
            std::slice::from_ref(&png)
        ),
        "step.evidence_required"
    );
    assert!(
        validate_report_with_files(
            node(&trace, "a"),
            &report(&trace, "a", &[&png.id.to_string(), link]),
            trace.step("a"),
            vec![png],
        )
        .is_ok()
    );
}
