//! Walkthrough #2: the execution document lists sections in runbook order.
//! Iterations of a `foreach` or `loop` sit right after their container, in
//! iteration order, not after every other declared step (the engine admits
//! iteration bodies later than the steps it declares up front).
#[path = "../../runbook-engine/tests/support/mod.rs"]
mod support;
use runbook_core::execution_document::{build_document, runbook_order};
use runbook_engine::ExecutionState;
use runbook_model::ExecutionView;
use std::num::NonZeroU32;

const SOURCE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: order, title: Order}\n\
    spec:\n  inputs: {regions: {type: list, items: {type: string}, default: [us-east, eu-west]}}\n  \
    steps:\n    \
    - id: both\n      parallel:\n        \
    - id: regions\n          foreach: {items: '${{ inputs.regions }}', as: region, concurrency: 2, max_items: 2, \
    steps: [{id: error_rate, title: 'Error rate in ${{ region }}', actor: human, assignee: user:alice}]}\n        \
    - id: settle\n          loop: {until: '${{ false }}', max_iterations: 2, \
    steps: [{id: recheck, title: Recheck, actor: human, assignee: user:alice}]}\n        \
    - id: signoff\n          \
    approval: {prompt: 'Ship?', options: [yes], approvers: {users: [alice]}}\n        \
    - id: last\n          title: Last\n          actor: human\n          assignee: user:alice\n";

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

#[test]
fn sections_follow_runbook_order_with_iterations_under_their_container() {
    let state = state();
    let document = build_document(&state, header(&state), |id| format!("/logs/{id}"));
    let paths: Vec<&str> = document.sections.iter().map(|s| s.path.as_str()).collect();
    let recheck = paths
        .iter()
        .find(|p| p.ends_with(".recheck"))
        .copied()
        .unwrap_or_else(|| panic!("no loop iteration in {paths:?}"));
    assert_eq!(
        paths,
        [
            "both",
            "regions",
            "regions[0].error_rate",
            "regions[1].error_rate",
            "settle",
            recheck,
            "signoff",
            "last",
        ],
    );
}

#[test]
fn runbook_order_keeps_every_materialized_step_once() {
    let state = state();
    let ordered = runbook_order(&state);
    let mut ids: Vec<_> = ordered.iter().filter_map(|s| s.id.clone()).collect();
    let mut expected: Vec<_> = state.steps.iter().filter_map(|s| s.id.clone()).collect();
    assert_eq!(ids.len(), expected.len());
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected);
}
