//! C4: step titles and approval prompts are rendered per step instance
//! (iteration bindings included) in the resume brief, the execution document
//! and inbox assignments, the same way agent work notices render them. A
//! failing template shows the raw text; rendered text stays escaped in
//! Markdown.
#[path = "../../runbook-engine/tests/support/mod.rs"]
mod support;
use runbook_core::{
    catalog::AssignmentKind,
    execution_document::{build_document, render_markdown},
    reads::resume_brief,
    runtime::assignments,
};
use runbook_engine::ExecutionState;
use runbook_model::{ActorKind, ExecutionView};
use std::num::NonZeroU32;

const SOURCE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: titles, title: Titles}\n\
    spec:\n  inputs: {regions: {type: list, items: {type: string}, default: [us-east, eu-west]}, \
    version: {type: string, default: 'v2 [x](y)'}}\n  steps:\n    \
    - id: both\n      parallel:\n        \
    - id: regions\n          foreach: {items: '${{ inputs.regions }}', as: region, concurrency: 2, max_items: 2, \
    steps: [{id: error_rate, title: 'Error rate in ${{ region }}', actor: human, assignee: user:alice}]}\n        \
    - id: signoff\n          title: 'Sign off ${{ inputs.version }}'\n          \
    approval: {prompt: 'Ship ${{ inputs.version }}?', options: [yes], approvers: {users: [alice]}}\n        \
    - id: broken\n          title: 'Broken ${{ inputs.regions[99] }}'\n          actor: human\n          assignee: user:alice\n";

fn state() -> ExecutionState {
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let trace = support::run_document(SOURCE, &[], directory);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    trace.state
}

fn header(state: &ExecutionState) -> ExecutionView {
    ExecutionView {
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
    }
}

const RENDERED: [&str; 3] = [
    "Error rate in us-east",
    "Error rate in eu-west",
    "Broken ${{ inputs.regions[99] }}",
];

#[test]
fn brief_lists_ready_work_under_rendered_titles() {
    let state = state();
    let brief = resume_brief(&state, Some(ActorKind::Human));
    let mut titles: Vec<_> = brief.ready_work.iter().map(|w| w.title.as_str()).collect();
    titles.sort();
    let mut expected = RENDERED.to_vec();
    expected.sort();
    assert_eq!(titles, expected);
}

#[test]
fn document_renders_titles_and_prompt_and_escapes_them_in_markdown() {
    let state = state();
    let document = build_document(&state, header(&state), |id| format!("/logs/{id}"));
    let title = |path: &str| {
        document
            .sections
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.title.clone())
            .unwrap()
    };
    assert_eq!(title("regions[0].error_rate"), "Error rate in us-east");
    assert_eq!(title("regions[1].error_rate"), "Error rate in eu-west");
    assert_eq!(title("broken"), "Broken ${{ inputs.regions[99] }}");
    assert_eq!(title("signoff"), "Sign off v2 [x](y)");
    let approval = document
        .sections
        .iter()
        .find_map(|s| s.approval.as_ref())
        .unwrap();
    assert_eq!(approval.prompt, "Ship v2 [x](y)?");
    let markdown = render_markdown(&state, &document);
    assert!(
        markdown.contains("## Error rate in us\\-east (regions\\[0\\]\\.error\\_rate)"),
        "{markdown}"
    );
    assert!(
        markdown.contains("**Approval:** Ship v2 \\[x\\]\\(y\\)?"),
        "{markdown}"
    );
    assert!(!markdown.contains("[x](y)"), "{markdown}");
}

#[test]
fn inbox_assignments_carry_rendered_titles() {
    let state = state();
    let found = assignments(&state, &Default::default(), state.sequence, &[]);
    let title = |kind: AssignmentKind, path: &str| {
        found
            .iter()
            .find(|a| a.kind == kind && a.path == path)
            .map(|a| a.title.clone())
            .unwrap_or_else(|| panic!("{path} missing: {found:?}"))
    };
    assert_eq!(
        title(AssignmentKind::Work, "regions[0].error_rate"),
        "Error rate in us-east"
    );
    assert_eq!(
        title(AssignmentKind::Work, "regions[1].error_rate"),
        "Error rate in eu-west"
    );
    assert_eq!(
        title(AssignmentKind::Work, "broken"),
        "Broken ${{ inputs.regions[99] }}"
    );
    assert_eq!(
        title(AssignmentKind::Approval, "signoff"),
        "Sign off v2 [x](y)"
    );
}
