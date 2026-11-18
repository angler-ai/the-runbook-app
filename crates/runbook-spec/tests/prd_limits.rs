//! R52: PRD §17 limits that no §22 decision overrides are enforced at
//! publish, at their exact boundaries (limit, limit + 1).
use runbook_spec::{CompileContext, CompiledRunbook, Diagnostic, compile, parse};
use serde_json::{Value, json};

fn publish(source: &str) -> Result<CompiledRunbook, Vec<Diagnostic>> {
    parse(source).and_then(|spec| compile(&spec, &CompileContext::default()))
}
fn document_with_id(id: &str, spec: Value) -> String {
    json!({
        "apiVersion": "therunbook.app/v1", "kind": "Runbook",
        "metadata": {"id": id, "title": "Limits"},
        "spec": spec,
    })
    .to_string()
}
fn document(spec: Value) -> String {
    document_with_id("limits", spec)
}
fn with_items(mut spec: Value) -> String {
    spec["inputs"] =
        json!({"items": {"type": "list", "items": {"type": "string"}, "default": ["a"]}});
    document(spec)
}
fn task(id: &str) -> Value {
    json!({"id": id, "title": id})
}
#[track_caller]
fn rejected(source: &str, code: &str, path: &str) {
    let diagnostics = publish(source).map(|_| ()).unwrap_err();
    assert_eq!(
        diagnostics
            .iter()
            .map(|d| (d.code.as_str(), d.path.as_str()))
            .collect::<Vec<_>>(),
        vec![(code, path)]
    );
}

#[test]
fn foreach_items_are_capped_at_100() {
    let foreach = |n: u32| {
        with_items(json!({"steps": [{"id": "f", "foreach": {
            "items": "${{ inputs.items }}", "max_items": n, "steps": [task("t")]
        }}]}))
    };
    assert!(publish(&foreach(100)).is_ok());
    rejected(
        &foreach(101),
        "spec.limit_exceeded",
        "/spec/steps/0/foreach/max_items",
    );
}

#[test]
fn loop_iterations_are_capped_at_50() {
    let looped = |n: u32| {
        document(json!({"steps": [{"id": "l", "loop": {
            "until": "${{ true }}", "max_iterations": n, "steps": [task("t")]
        }}]}))
    };
    assert!(publish(&looped(50)).is_ok());
    rejected(
        &looped(51),
        "spec.limit_exceeded",
        "/spec/steps/0/loop/max_iterations",
    );
}

#[test]
fn parallel_branches_are_capped_at_20() {
    let fan = |n: usize| {
        document(json!({"steps": [{"id": "p", "parallel":
            (0..n).map(|i| task(&format!("b{i}"))).collect::<Vec<_>>()
        }]}))
    };
    assert!(publish(&fan(20)).is_ok());
    rejected(&fan(21), "spec.limit_exceeded", "/spec/steps/0/parallel");
}

#[test]
fn metadata_id_is_at_most_63_bytes() {
    let steps = json!({"steps": [task("t")]});
    assert!(publish(&document_with_id(&"a".repeat(63), steps.clone())).is_ok());
    rejected(
        &document_with_id(&"a".repeat(64), steps),
        "spec.invalid_id",
        "/metadata/id",
    );
}

#[test]
fn negating_the_smallest_integer_is_a_publish_error() {
    let guarded = |expr: &str| {
        document(json!({"steps": [{"id": "t", "title": "T", "if": format!("${{{{ {expr} }}}}")}]}))
    };
    assert!(publish(&guarded("-9223372036854775808 < 0")).is_ok());
    let diagnostics = publish(&guarded("-(-9223372036854775808) > 0"))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, "expression.integer_overflow");
}

// The editor-facing source schema states the same caps.
#[test]
fn source_schema_states_the_caps() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../schemas/runbook-v1.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let doc = |id: &str, spec: Value| -> Value {
        serde_json::from_str(&document_with_id(id, spec)).unwrap()
    };
    let foreach = |n: u32| json!({"steps": [{"id": "f", "foreach": {"items": "${{ inputs.items }}", "max_items": n, "steps": []}}]});
    let looped = |n: u32| json!({"steps": [{"id": "l", "loop": {"until": "${{ true }}", "max_iterations": n, "steps": []}}]});
    let fan = |n: usize| json!({"steps": [{"id": "p", "parallel": (0..n).map(|i| task(&format!("b{i}"))).collect::<Vec<_>>()}]});
    for (ok, bad) in [
        (doc("limits", foreach(100)), doc("limits", foreach(101))),
        (doc("limits", looped(50)), doc("limits", looped(51))),
        (doc("limits", fan(20)), doc("limits", fan(21))),
        (
            doc(&"a".repeat(63), json!({"steps": []})),
            doc(&"a".repeat(64), json!({"steps": []})),
        ),
    ] {
        assert!(validator.is_valid(&ok), "{ok}");
        assert!(!validator.is_valid(&bad), "{bad}");
    }
}

#[test]
fn a_duration_wait_is_capped_at_7_days() {
    let wait = |duration: &str| {
        document(json!({"steps": [{"id": "w", "wait": {"for": duration}}, task("after")]}))
    };
    for at_limit in ["7d", "168h", "604800000ms", "6d23h59m59s1000ms"] {
        assert!(publish(&wait(at_limit)).is_ok(), "{at_limit}");
    }
    for over in ["604800001ms", "7d1ms", "8d"] {
        rejected(&wait(over), "spec.limit_exceeded", "/spec/steps/0/wait/for");
    }
    let diagnostic = publish(&wait("8d")).map(|_| ()).unwrap_err().remove(0);
    let message = diagnostic.message.expect("the limit is explained");
    assert!(message.contains("7 days"), "{message}");
    // The cap is on how long the wait parks the run, not on its timeout or
    // on signal waits (bounded by the execution duration instead).
    assert!(
        publish(&document(json!({"steps": [
            {"id": "w", "wait": {"signal": "go", "timeout": "30d"}}
        ]})))
        .is_ok()
    );
}
