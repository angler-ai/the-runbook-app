mod support;
use runbook_model::{ExecutionStatus, Verdict};
#[test]
fn full_flagship_manual_rollback_trace() {
    let trace = support::flagship::run(false);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state.result, serde_json::json!("rolled_back"));
    assert_eq!(trace.count("ApprovalDecided"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn recovered_loop_still_exposes_satisfaction() {
    let trace = support::flagship::run(true);
    assert!(trace.step("stabilize").satisfied);
    assert_eq!(trace.step("stabilize").verdict, Some(Verdict::Fail));
    assert_eq!(trace.state.result, serde_json::json!("kept"));
    assert_eq!(trace.state, trace.replay());
}

fn step_view(step: &runbook_engine::StepState) -> serde_json::Value {
    serde_json::json!({"schema_revision":1,"id":step.id,"path":step.step_id,"status":step.status,"verdict":step.verdict,"outputs":step.outputs,"summary":step.summary,"actor":step.actor,"claim":null,"attempt":step.attempt,"satisfied":step.satisfied,"exhausted":step.exhausted,"checklist":step.checklist,"amended":false,"amendments":[],"stale":false,"eligible_recipients":[]})
}
#[test]
fn flagship_source_is_the_spec_fixture() {
    // The full example from the language reference.
    assert_eq!(
        support::flagship::SOURCE,
        include_str!("../../runbook-spec/tests/fixtures/post-deploy.yaml")
    );
}
/// Pins the fixture bytes only: the expected value is the engine's own output
/// and RUNBOOK_UPDATE_FIXTURES rewrites it. `flagship_prd.rs` checks both the
/// live traces and this checked-in trace against hand-written PRD flows.
#[test]
fn flagship_traces_match_checked_in_contract() {
    use serde_json::json;
    let manual = support::flagship::run(false);
    let recovered = support::flagship::run(true);
    let fixture = json!({
        "format":"pure-engine-trace-v1",
        "integration_seams":{"queueing":"B5","indexed_search":"B3/B8","directory_authorization":"B1/B6","claims_and_credentials":"B6/B7","webhook_delivery":"C1"},
        "pending_language_decisions":[],
        "manual_rollback":support::flagship::serialized(&manual),
        "recovered_loop":support::flagship::serialized(&recovered)
    });
    let loop_view = step_view(recovered.step("stabilize"));
    let state = &manual.state;
    let url = runbook_model::execution_api_url("https://runbooks.example.com", &state.id);
    let web_url = runbook_model::execution_web_url(&url, &state.id);
    let completed = json!({"schema_revision":1,"id":state.id,"url":url,"web_url":web_url,"runbook_version_id":"rbv_00000000000000000000000001","runbook_version":1,"title":state.title,"status":state.status,"phase":"completed","result":state.result,"pending_terminal_status":null,"inputs":state.inputs,"fields":state.fields,"tags":state.tags,"steps":state.steps.iter().filter(|s|s.id.is_some()).map(step_view).collect::<Vec<_>>(),"amended":false,"amendments":[],"stale_step_ids":[],"rerun_of":null,"resume_brief":null});
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, expected) in [
        ("conformance/execution/post-deploy.json", fixture),
        ("api/examples/loop-recovered.json", loop_view),
        ("api/examples/post-deploy-completed.json", completed),
    ] {
        let path = root.join(path);
        if std::env::var_os("RUNBOOK_UPDATE_FIXTURES").is_some() {
            std::fs::write(
                &path,
                format!("{}\n", serde_json::to_string_pretty(&expected).unwrap()),
            )
            .unwrap();
        }
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored, expected, "{}", path.display());
    }
}
