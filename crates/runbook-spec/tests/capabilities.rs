use runbook_spec::{Support, classify};
use serde_json::json;

#[test]
fn unknown_key_is_rejected() {
    assert_eq!(
        classify("spec.magic", &json!(true)).code(),
        Some("spec.unknown_key")
    );
}

#[test]
fn behavior_keys_are_never_ignored() {
    for (name, path, value, owner) in [
        (
            "queue admission",
            "spec.execution.concurrency.on_conflict",
            json!("queue"),
            "B5",
        ),
        (
            "indexed severity",
            "spec.fields.severity.indexed",
            json!(true),
            "B3/B8",
        ),
        (
            "checklist",
            "spec.steps[0].checklist",
            json!(["Login page loads"]),
            "A8",
        ),
        (
            "skill parameters",
            "spec.steps[0].with.region",
            json!("us-west"),
            "A4/A5/A20/B3",
        ),
        (
            "audience instructions",
            "spec.steps[0].instructions.human",
            json!("Inspect"),
            "A20",
        ),
        (
            "report template",
            "spec.report",
            json!("# Result"),
            "A20/B8",
        ),
    ] {
        assert_eq!(
            classify(path, &value),
            Support::Supported {
                owner: owner.into()
            },
            "{name}"
        );
    }
}

#[test]
fn named_pilot_controls_and_schema_children_are_supported() {
    let cases = [
        ("apiVersion", json!("therunbook.app/v1")),
        ("kind", json!("Runbook")),
        ("metadata.owners[0]", json!("team:ops")),
        ("spec.execution.tags[0]", json!("ops")),
        ("spec.execution.cleanup_timeout", json!("30m")),
        (
            "spec.inputs.request.properties.parts.items.type",
            json!("integer"),
        ),
        ("spec.inputs.request.properties.parts.items.min", json!(0)),
        ("spec.fields.severity.locked", json!(true)),
        ("spec.defaults.retry.max_attempts", json!(3)),
        ("spec.defaults.retry.backoff", json!("5m")),
        ("spec.defaults.on_failure", json!("handoff")),
        ("spec.agents.params", json!({"repo":"acme/api"})),
        ("spec.agents.params.x-any.nested", json!(null)),
        ("spec.steps[0].retry.backoff", json!("5m")),
        ("spec.steps[0].evidence.accepted[0]", json!("log")),
        // UP1: uploaded evidence, finalized by size and magic bytes.
        ("spec.steps[0].evidence.required[0]", json!("file")),
        ("spec.steps[0].evidence.accepted[0]", json!("screenshot")),
        ("spec.steps[0].catch[0].instructions.default", json!("Help")),
        ("spec.steps[0].finally[0].id", json!("cleanup")),
        ("spec.steps[0].parallel[0].join", json!("all")),
        ("spec.steps[0].fail_fast", json!(true)),
        ("spec.steps[0].foreach.key", json!("${{ item.id }}")),
        (
            "spec.steps[0].foreach.steps[0].outputs.item.type",
            json!("text"),
        ),
        ("spec.steps[0].loop.max_iterations", json!(3)),
        // LANG1.
        ("spec.steps[0].loop.while", json!("${{ true }}")),
        // LANG3.
        ("spec.execution.concurrency.on_conflict", json!("reject")),
        (
            "spec.execution.concurrency.on_conflict",
            json!("cancel_previous"),
        ),
        // LANG2.
        ("spec.steps[0].join", json!("any")),
        ("spec.steps[0].join", json!({"at_least": 2})),
        ("spec.steps[0].join.at_least", json!(1)),
        ("spec.steps[0].loop.steps[0].wait.signal", json!("ready")),
        ("spec.steps[0].switch.cases[0].when", json!("${{ true }}")),
        (
            "spec.steps[0].switch.default.steps[0].end.status",
            json!("succeeded"),
        ),
        (
            "spec.steps[0].switch.default.steps[0].end.message",
            json!("Done"),
        ),
        ("spec.steps[0].approval.options[0].auto", json!(true)),
        ("spec.steps[0].approval.options[0].id", json!("keep")),
        ("spec.steps[0].approval.options[0].title", json!("Keep")),
        ("spec.steps[0].approval.options[1]", json!("rollback")),
        ("spec.steps[0].approval.approvers.users[0]", json!("alice")),
        (
            "spec.steps[0].approval.on_timeout.decide",
            json!("rollback"),
        ),
        ("spec.steps[0].approval.on_expired", json!("pause")),
        ("spec.steps[0].approval.allow_agents", json!(false)),
        ("spec.steps[0].approval.escalation_timeout", json!("30m")),
        ("spec.steps[0].approval.escalate_to[0]", json!("team:leads")),
        ("spec.steps[0].set.vars.result", json!("healthy")),
        ("spec.finally[0].allow_auto_decision", json!(true)),
    ];
    for (path, value) in cases {
        assert!(
            matches!(classify(path, &value), Support::Supported { owner } if !owner.is_empty()),
            "{path}"
        );
    }
    for kind in [
        "string", "text", "number", "integer", "boolean", "enum", "list", "object",
    ] {
        assert!(
            matches!(
                classify("spec.inputs.value.type", &json!(kind)),
                Support::Supported { .. }
            ),
            "{kind}"
        );
    }
}

#[test]
fn deferred_behavior_and_unknown_keywords_are_rejected() {
    for (path, value) in [
        ("apiVersion", json!("therunbook.app/v2")),
        ("kind", json!("StepLibrary")),
        ("spec.execution.concurrency.on_conflict", json!("cancel")),
        ("spec.triggers.manual", json!(true)),
        ("spec.steps[0].call.timeout", json!("1h")),
        // R0-3: retired in favour of agent.name.
        ("spec.steps[0].agent.runner", json!("hosted")),
        ("spec.steps[0].evidence.required[0]", json!("video")),
        ("spec.steps[0].evidence.accepted[0]", json!("photo")),
        ("spec.inputs.name.magic", json!(true)),
        ("spec.inputs.name.properties.nested.magic", json!(true)),
        ("spec.steps[0].instructions.magic", json!("Help")),
        ("spec.steps[0].foreach.magic", json!(true)),
        ("spec.agents.dispatch.github.token", json!("never YAML")),
        ("spec.steps[0].approval.options[0].magic", json!(true)),
        ("spec.fields.foo.items.indexed", json!(true)),
    ] {
        assert!(
            matches!(classify(path, &value), Support::Rejected { .. }),
            "{path}"
        );
    }
    for kind in ["secret_ref", "future_type"] {
        assert!(
            matches!(
                classify("spec.inputs.value.type", &json!(kind)),
                Support::Rejected { .. }
            ),
            "{kind}"
        );
    }
}

#[test]
fn arbitrary_data_keys_are_not_confused_with_keywords() {
    for path in [
        "spec.vars.result.magic",
        "spec.outputs.result",
        "spec.skills.error_rates",
        "spec.inputs.type.type",
        "spec.fields.steps.properties.magic.type",
        "spec.steps[0].outputs.pattern.type",
        "spec.steps[0].with.magic.pattern",
        "spec.inputs.config.default.arbitrary.pattern",
        "spec.inputs.config.options[0].magic",
        "/spec/vars/a.b/a~1b~0c",
        "spec.steps[0].set.fields.magic",
        "spec.steps[0].set.vars.pattern",
    ] {
        assert!(
            matches!(classify(path, &json!("string")), Support::Supported { .. }),
            "{path}"
        );
    }
}

#[test]
fn only_extensions_and_advisory_agent_data_are_preserved() {
    for path in [
        "x-root.nested",
        "metadata.x-display.magic",
        "spec.inputs.x-form",
        "spec.inputs.name.x-ui.widget",
        "spec.steps[0].approval.options[0].x-display.magic",
        "spec.steps[0].agent.tools[0]",
        "spec.steps[0].agent.commands[0].command",
    ] {
        assert!(
            matches!(classify(path, &json!("data")), Support::Preserved { reason } if !reason.is_empty()),
            "{path}"
        );
    }
}

fn walk(value: &serde_json::Value, path: &str, leaves: &mut Vec<(String, serde_json::Value)>) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                let key = key.replace('~', "~0").replace('/', "~1");
                walk(child, &format!("{path}/{key}"), leaves);
            }
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            for (index, child) in items.iter().enumerate() {
                walk(child, &format!("{path}/{index}"), leaves);
            }
        }
        _ => leaves.push((path.into(), value.clone())),
    }
}

#[test]
fn every_flagship_leaf_has_an_owner() {
    // The full example (the A1 fixture).
    let yaml = include_str!("fixtures/post-deploy.yaml");
    let spec = runbook_spec::parse(yaml).unwrap();
    let mut leaves = Vec::new();
    walk(spec.source_json(), "", &mut leaves);
    assert!(leaves.len() > 100, "walk the full example, not a subset");
    for (path, value) in leaves {
        assert!(
            matches!(classify(&path, &value), Support::Supported { owner } if !owner.is_empty()),
            "{path}: {value}"
        );
    }
    assert!(runbook_spec::validate_capabilities(&spec).is_empty());
}

#[test]
fn rejected_capabilities_report_source_locations_without_cascades() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: example, title: Example}\nspec:\n  magic: true\n  inputs:\n    bad: {type: secret_ref}\n  steps:\n    - id: work\n      title: Work\n      evidence: {required: [video]}\n      agent: {runner: hosted}\n      x-config: {call: retained}\n";
    let spec = runbook_spec::parse(yaml).unwrap();
    let diagnostics = runbook_spec::validate_capabilities(&spec);
    assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");
    for (path, code) in [
        ("/spec/magic", "spec.unknown_key"),
        ("/spec/inputs/bad/type", "spec.unsupported_type"),
        (
            "/spec/steps/0/evidence/required/0",
            "spec.unsupported_value",
        ),
        ("/spec/steps/0/agent/runner", "spec.retired_key"),
    ] {
        let diagnostic = diagnostics.iter().find(|d| d.path == path).unwrap();
        assert_eq!(diagnostic.code, code);
        let span = spec.source_spans[path];
        assert_eq!(
            (diagnostic.line, diagnostic.column),
            (span.line, span.column)
        );
    }
}

#[test]
fn extension_subtrees_remain_preserved_inside_data_maps() {
    for path in [
        "spec.vars.x-state.future",
        "spec.inputs.foo.default.x-hint.pattern",
        "spec.steps[0].with.x-render.magic",
        "spec.outputs.x-result.nested",
    ] {
        assert!(
            matches!(classify(path, &json!(true)), Support::Preserved { .. }),
            "{path}"
        );
    }
    let spec = runbook_spec::parse("apiVersion: therunbook.app/v1\nkind: Runbook\nx-root: {magic: true}\nmetadata: {id: example, title: Example, x-meta: {future: true}}\nspec:\n  vars: {user: {pattern: text}}\n  inputs: {config: {type: object, default: {magic: true}, x-ui: {pattern: any}}}\n  steps:\n    - id: work\n      title: Work\n      agent: {tools: [{any: data}], commands: ['echo advisory']}\n      with: {pattern: arbitrary}\n      x-nested: {call: future}\n").unwrap();
    let before = spec.source_json().clone();
    assert!(runbook_spec::validate_capabilities(&spec).is_empty());
    assert_eq!(spec.source_json(), &before);
}

#[test]
fn checked_in_profile_expands_all_flagship_leaf_paths() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/spec/pilot-capabilities.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let spec = runbook_spec::parse(include_str!("fixtures/post-deploy.yaml")).unwrap();
    let mut leaves = Vec::new();
    walk(spec.source_json(), "", &mut leaves);
    let expanded = manifest["flagship_leaves"].as_array().unwrap();
    assert_eq!(expanded.len(), leaves.len());
    for (path, value) in leaves {
        let entry = expanded
            .iter()
            .find(|entry| entry["path"] == path)
            .unwrap_or_else(|| panic!("missing {path}"));
        assert_eq!(
            classify(&path, &value),
            Support::Supported {
                owner: entry["owner"].as_str().unwrap().into()
            },
            "{path}"
        );
    }
    for entry in manifest["rules"].as_array().unwrap() {
        let actual = classify(entry["path"].as_str().unwrap(), &entry["value"]);
        match entry["class"].as_str().unwrap() {
            "supported" => assert_eq!(
                actual,
                Support::Supported {
                    owner: entry["owner"].as_str().unwrap().into()
                },
                "{entry}"
            ),
            "preserved" => assert!(matches!(actual, Support::Preserved { .. }), "{entry}"),
            "rejected" => {
                assert!(matches!(actual, Support::Rejected { .. }), "{entry}");
                if let Some(code) = entry["code"].as_str() {
                    assert_eq!(actual.code(), Some(code), "{entry}");
                }
            }
            other => panic!("unknown class {other}"),
        }
    }
}

#[test]
fn control_containers_have_their_behavior_owners() {
    for (path, owner) in [
        ("spec.steps[0].steps", "A7/A14/A19"),
        ("spec.steps[0].catch", "A7/A14/A19"),
        ("spec.steps[0].finally", "A7/A14/A19"),
        ("spec.steps[0].parallel", "A11"),
        ("spec.steps[0].foreach.steps", "A12"),
        ("spec.steps[0].loop.steps", "A13"),
        ("spec.steps[0].switch.cases[0].steps", "A10"),
    ] {
        assert_eq!(
            classify(path, &json!([])),
            Support::Supported {
                owner: owner.into()
            },
            "{path}"
        );
    }
}

#[test]
fn agents_params_is_supported_pass_through() {
    let params = json!({"repo":"acme/api", "workflow":"runbook-resume.yml",
        "x-data":{"dispatch":{"github":null}, "empty":[], "text":"${{ not_an_expression }}"}});
    assert!(matches!(
        classify("spec.agents.params", &params),
        Support::Supported { .. }
    ));
    let exact = json!({"p":"a".repeat(4088)});
    assert_eq!(serde_json::to_vec(&exact).unwrap().len(), 4096);
    assert!(matches!(
        classify("spec.agents.params", &exact),
        Support::Supported { .. }
    ));
    let over = json!({"p":"a".repeat(4089)});
    assert_eq!(serde_json::to_vec(&over).unwrap().len(), 4097);
    assert_eq!(
        classify("spec.agents.params", &over).code(),
        Some("spec.agents_params_too_large")
    );
    for value in [
        json!(null),
        json!([]),
        json!("object"),
        json!(true),
        json!(7),
    ] {
        assert_eq!(
            classify("spec.agents.params", &value).code(),
            Some("spec.type_error")
        );
    }
}

#[test]
fn agents_dispatch_is_rejected_with_guidance() {
    for path in [
        "spec.agents.dispatch",
        "spec.agents.dispatch.github",
        "/spec/agents/dispatch/x-future",
    ] {
        let support = classify(path, &json!({"repo":"acme/api"}));
        assert_eq!(support.code(), Some("spec.agents_dispatch_removed"));
    }
    let spec = runbook_spec::parse("apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec:\n  agents:\n    dispatch:\n      github: {repo: acme/api}\n  steps: []\n").unwrap();
    let errors = runbook_spec::validate_capabilities(&spec);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, "/spec/agents/dispatch");
    let message = errors[0].message.as_deref().unwrap();
    assert!(message.contains("project webhook subscriptions"));
    assert!(message.contains("spec.agents.params"));
    let wire = serde_json::to_value(&errors[0]).unwrap();
    assert_eq!(wire["message"], message);
    let old: runbook_spec::Diagnostic = serde_json::from_value(json!({
        "code":"spec.unknown_key","path":"/spec/magic","line":1,"column":1
    }))
    .unwrap();
    assert!(old.message.is_none());
    assert!(serde_json::to_value(old).unwrap().get("message").is_none());
}

#[test]
fn agents_params_limit_counts_encoded_utf8_and_escapes() {
    let mut value = json!({
        "quote\"\\\n\u{0}é": [null, true, false, [], {}, "\u{8}\u{c}\r\t/💾"],
        "$serde_json::private::Number": "ordinary object data",
        "integer": 18446744073709551616_u128,
        "decimal": serde_json::from_str::<serde_json::Value>("1.2300e+200").unwrap(),
        "padding": ""
    });
    let length = serde_json::to_vec(&value).unwrap().len();
    value["padding"] = json!("a".repeat(4096 - length));
    assert_eq!(serde_json::to_vec(&value).unwrap().len(), 4096);
    assert!(matches!(
        classify("spec.agents.params", &value),
        Support::Supported { .. }
    ));
    value["padding"] = json!("a".repeat(4097 - length));
    assert_eq!(serde_json::to_vec(&value).unwrap().len(), 4097);
    assert_eq!(
        classify("spec.agents.params", &value).code(),
        Some("spec.agents_params_too_large")
    );
}

#[test]
fn agents_params_rejects_large_borrowed_trees_without_recursive_traversal() {
    let mut nested = json!(null);
    for _ in 0..100_000 {
        nested = serde_json::Value::Array(vec![nested]);
    }
    let mut object = serde_json::Map::new();
    object.insert("deep".into(), nested);
    let mut value = serde_json::Value::Object(object);
    let rejected =
        classify("spec.agents.params", &value).code() == Some("spec.agents_params_too_large");
    // The caller owns the huge input, including responsibility for safe teardown.
    let mut nested = value.as_object_mut().unwrap().remove("deep").unwrap();
    while let serde_json::Value::Array(mut children) = nested {
        nested = children.pop().unwrap();
    }
    assert!(rejected);
    let wide = json!({"wide":vec![serde_json::Value::Null; 100_000]});
    assert_eq!(
        classify("spec.agents.params", &wide).code(),
        Some("spec.agents_params_too_large")
    );
}
