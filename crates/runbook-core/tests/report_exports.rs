//! REP2 golden-structure tests on the flagship document (the PRD's
//! `post-deploy` runbook, run by the engine's own flagship trace): the JUnit
//! and PDF exports carry every step, name people, and are deterministic.
#[path = "../../runbook-engine/tests/support/mod.rs"]
mod support;

use runbook_core::{
    execution_document::{build_document, render_markdown_named},
    report_junit::render_junit_named,
};
use runbook_engine::ExecutionState;
use runbook_model::{ExecutionDocumentView, ExecutionView, StepRunId, UserId};
use runbook_report_pdf::{inspect, render_pdf};
use std::{collections::BTreeMap, num::NonZeroU32};

fn header(state: &ExecutionState) -> ExecutionView {
    ExecutionView {
        overdue_steps: vec![],
        url: String::new(),
        web_url: String::new(),
        schema_revision: NonZeroU32::MIN,
        id: state.id.clone(),
        runbook_id: None,
        runbook_slug: Some("post-deploy".into()),
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

fn logs(id: &StepRunId) -> String {
    format!("/v1/step-runs/{id}/logs")
}

fn names() -> BTreeMap<UserId, String> {
    BTreeMap::from([
        (
            "usr_00000000000000000000000001".parse().unwrap(),
            "Alice".into(),
        ),
        (
            "usr_00000000000000000000000002".parse().unwrap(),
            "Bob".into(),
        ),
    ])
}

/// The terminal flagship run (manual rollback) and its document.
fn flagship() -> (ExecutionState, ExecutionDocumentView) {
    let trace = support::flagship::run(false);
    assert!(matches!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    ));
    let document = build_document(&trace.state, header(&trace.state), logs);
    assert!(document.sections.len() > 5, "the flagship has many steps");
    (trace.state, document)
}

#[test]
fn flagship_junit_has_every_step_and_names_people() {
    let (state, document) = flagship();
    let xml = render_junit_named(&state, &document, &names());
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n"));
    assert_eq!(xml.matches("<testsuite ").count(), 1);
    assert_eq!(
        xml.matches("<testcase ").count(),
        document.sections.len(),
        "{xml}"
    );
    for section in &document.sections {
        let name = runbook_core::report_junit::xml_escape(&section.path);
        assert!(
            xml.contains(&format!("classname=\"post-deploy\" name=\"{name}\"")),
            "missing {name}: {xml}"
        );
    }
    // The flagship's human steps were done by Alice; the export says so.
    assert!(xml.contains("Actor: Alice"), "{xml}");
    // Outputs are data and stay as recorded (`decided_by: usr_…`); only the
    // actor and decision lines name people.
    assert!(xml.contains("Decision: Alice chose "), "{xml}");
    assert!(
        !xml.contains("human usr_"),
        "people by name, never by id: {xml}"
    );
    assert_eq!(xml, render_junit_named(&state, &document, &names()));
}

#[test]
fn flagship_pdf_has_its_titles_and_steps() {
    let (state, document) = flagship();
    let markdown = render_markdown_named(&state, &document, &names());
    let pdf = render_pdf(&markdown);
    assert!(pdf.starts_with(b"%PDF-"));
    inspect::check_structure(&pdf).expect("cross-reference table");
    let pages = inspect::page_count(&pdf);
    assert!(pages >= 2, "{pages} pages");
    assert_eq!(
        inspect::info_title(&pdf).as_deref(),
        Some(document.header.title.as_str())
    );
    // Wrapped lines rejoin with spaces.
    let text = inspect::extract_text(&pdf).replace(['\n', '\u{c}'], " ");
    assert!(text.contains(&document.header.title), "{text}");
    assert!(text.contains("Status: succeeded"), "{text}");
    for section in &document.sections {
        let heading = format!("{} ({})", section.title, section.path);
        assert!(text.contains(&heading), "missing {heading:?} in {text}");
    }
    assert!(text.contains("Actor: Alice"), "{text}");
    assert!(text.contains(&format!("Page {pages} of {pages}")), "{text}");
    assert!(pdf == render_pdf(&markdown), "deterministic");
}
