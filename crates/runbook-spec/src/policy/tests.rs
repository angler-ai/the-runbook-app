use super::*;
use serde_json::{Value, json};
fn ir(steps: Value) -> NormalizedRunbook {
    crate::normalize(&crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"steps":steps}}).to_string()).unwrap()).unwrap()
}
fn gate(id: &str, options: Value) -> Value {
    json!({"id":id,"approval":{"prompt":"Choose","options":options,"timeout":"1m","on_timeout":{"decide":"rollback"}}})
}
#[test]
fn automatic_option_must_be_declared() {
    for options in [
        json!(["keep"]),
        json!(["rollback"]),
        json!([{"id":"rollback","auto":false}]),
    ] {
        let diagnostics = validate_automatic_options(&ir(json!([gate("gate", options)])));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "spec.auto_option_required");
        assert_eq!(
            diagnostics[0].path,
            "/spec/steps/0/approval/on_timeout/decide"
        );
    }
    assert!(
        validate_automatic_options(&ir(json!([gate(
            "gate",
            json!([{"id":"rollback","auto":true}])
        )])))
        .is_empty()
    );
}
#[test]
fn automatic_option_check_covers_every_control_body() {
    let bad = |id| gate(id, json!(["keep"]));
    let mut runbook = ir(json!([
        {"id":"group","steps":[bad("child")],"catch":[bad("catch")],"finally":[bad("cleanup")]},
        {"id":"parallel","parallel":[bad("parallel_child")]},
        {"id":"each","foreach":{"items":"${{ [1] }}","max_items":1,"steps":[bad("each_child")]}},
        {"id":"repeat","loop":{"until":"${{ true }}","max_iterations":1,"steps":[bad("loop_child")]}},
        {"id":"switch","switch":{"cases":[{"when":"${{ false }}","steps":[bad("case_child")]}],"default":{"steps":[bad("default_child")]}}}
    ]));
    let mut final_runbook = ir(json!([bad("final_child")]));
    final_runbook.steps[0].source_path = "/spec/finally/0".into();
    runbook.finally = final_runbook.steps;
    let diagnostics = validate_automatic_options(&runbook);
    let expected = [
        "/spec/steps/0/steps/0",
        "/spec/steps/0/catch/0",
        "/spec/steps/0/finally/0",
        "/spec/steps/1/parallel/0",
        "/spec/steps/2/foreach/steps/0",
        "/spec/steps/3/loop/steps/0",
        "/spec/steps/4/switch/cases/0/steps/0",
        "/spec/steps/4/switch/default/steps/0",
        "/spec/finally/0",
    ];
    assert_eq!(
        diagnostics
            .iter()
            .map(|d| d.path.clone())
            .collect::<Vec<_>>(),
        expected.map(|p| format!("{p}/approval/on_timeout/decide"))
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| d.code == "spec.auto_option_required")
    );
}
#[test]
fn manual_options_do_not_require_auto_opt_in() {
    let runbook = ir(
        json!([{"id":"gate","approval":{"prompt":"Go?","options":["keep","rollback"],"on_timeout":"escalate","timeout":"1m"}}]),
    );
    assert!(validate_automatic_options(&runbook).is_empty());
}

#[test]
fn policy_nested_loops_are_memoized() {
    let mut body = json!([{ "id":"gate","approval":{"prompt":"Go?","options":[{"id":"rollback","auto":true}],"timeout":"1m","on_timeout":{"decide":"rollback"}}}]);
    for i in 0..30 {
        body = json!([{ "id":format!("loop{i}"),"loop":{"until":"${{ false }}","max_iterations":2,"steps":body}}]);
    }
    let runbook = ir(body);
    let found = super::region::analyze(&runbook, "gate", true, false);
    assert!(found.memo_entries <= 3 * 31);
    assert!(found.memo_entries >= 31);
}
