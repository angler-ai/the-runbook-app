//! UP1: the execution document references uploaded evidence by `evd_` ID;
//! the Markdown export adds the verified type beside it.
#[path = "../../runbook-engine/tests/support/mod.rs"]
mod support;
use runbook_core::execution_document::{build_document, render_markdown};
use runbook_engine::{ExecutionState, validate_report_with_files};
use runbook_model::{ActorRef, Data, EvidenceFileRef, ExecutionView, ReportRequest, ReportStatus};
use std::num::NonZeroU32;

fn header(state: &ExecutionState) -> ExecutionView {
    ExecutionView {
        overdue_steps: vec![],
        url: String::new(),
        web_url: String::new(),
        schema_revision: NonZeroU32::MIN,
        id: state.id.clone(),
        runbook_id: None,
        runbook_slug: None,
        project_id: None,
        created_at_ms: None,
        created_by: None,
        started_at_ms: state.started_at_ms,
        completed_at_ms: None,
        runbook_version_id: "rbv_00000000000000000000000001".parse().unwrap(),
        runbook_version: Some(1),
        title: state.title.clone(),
        status: state.status,
        phase: runbook_model::ExecutionPhase::Ordinary,
        admission: None,
        execution_deadline_at_ms: state.execution_deadline_at_ms,
        result: None,
        pending_terminal_status: None,
        terminal_reason: None,
        inputs: state.inputs.clone(),
        fields: state.fields.clone(),
        tags: state.tags.clone(),
        steps: vec![],
        amended: false,
        amendments: vec![],
        stale_step_ids: vec![],
        rerun_of: None,
        resume_brief: None,
        notifications_not_delivered: vec![],
        superseded_by: None,
        parent: None,
        progress: None,
        cost: None,
    }
}

#[test]
fn documents_list_uploads_by_evd_id_with_their_type() {
    let mut trace = support::run_case(
        "spec: {steps: [{id: shot, title: Shot, evidence: {required: [screenshot]}}]}",
        &[],
    );
    let png = EvidenceFileRef {
        id: "evd_00000000000000000000000001".parse().unwrap(),
        content_type: "image/png".into(),
    };
    let step = trace.step("shot").clone();
    let request = ReportRequest {
        step_run_id: step.id.clone().unwrap(),
        claim: runbook_model::ClaimRef {
            id: "clm_00000000000000000000000001".parse().unwrap(),
            generation: 1.try_into().unwrap(),
        },
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::new(),
        summary: None,
        evidence: vec![png.id.to_string(), "https://example.test/ticket".into()],
        logs: vec![],
        cost: None,
    };
    let node = trace.state.node(&step.step_id).unwrap().clone();
    let accepted = validate_report_with_files(&node, &request, &step, vec![png.clone()]).unwrap();
    trace.append(accepted.into_event(), ActorRef::System, 5);
    trace.settle();
    let state = &trace.state;
    let document = build_document(state, header(state), |id| format!("/logs/{id}"));
    assert_eq!(
        document.sections[0].evidence,
        [png.id.to_string(), "https://example.test/ticket".to_owned()]
    );
    let markdown = render_markdown(state, &document);
    assert!(
        markdown.contains("- Evidence: evd\\_00000000000000000000000001 (image/png)\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("- Evidence: https://example\\.test/ticket\n"),
        "{markdown}"
    );
}
