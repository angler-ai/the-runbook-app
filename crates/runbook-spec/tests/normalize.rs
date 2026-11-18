use runbook_spec::{normalize, parse};
use serde_json::{Value, json};

fn source(body: Value) -> runbook_spec::Spec {
    parse(
        &json!({"apiVersion":"therunbook.app/v1", "kind":"Runbook",
        "metadata":{"id":"test", "title":"Test"}, "spec":body})
        .to_string(),
    )
    .unwrap()
}
fn task(id: &str) -> Value {
    json!({"id":id,"title":id})
}

#[test]
fn duplicate_node_id_is_rejected() {
    let cases = [
        (
            json!({"steps":[task("health"),task("health")]}),
            "/spec/steps/1",
        ),
        (
            json!({"steps":[task("health"),{"id":"group","steps":[],"catch":[task("health")]}]}),
            "/spec/steps/1/catch/0",
        ),
        (
            json!({"steps":[task("health"),{"id":"group","steps":[],"finally":[task("health")]}]}),
            "/spec/steps/1/finally/0",
        ),
        (
            json!({"steps":[task("health"),{"id":"branch","switch":{"cases":[{"when":"${{ true }}","steps":[task("health")]}]}}]}),
            "/spec/steps/1/switch/cases/0/steps/0",
        ),
        (
            json!({"steps":[task("health"),{"id":"branch","switch":{"cases":[],"default":{"steps":[task("health")]}}}]}),
            "/spec/steps/1/switch/default/steps/0",
        ),
        (
            json!({"steps":[task("health")],"finally":[task("health")]}),
            "/spec/finally/0",
        ),
    ];
    for (body, path) in cases {
        let errors = normalize(&source(body)).unwrap_err();
        assert_eq!(errors[0].code, "spec.duplicate_id");
        assert_eq!(errors[0].path, path);
    }
}

#[test]
fn control_keywords_and_shapes_are_validated() {
    for (node, code, suffix) in [
        (
            json!({"id":"bad","steps":[],"parallel":[]}),
            "spec.control_conflict",
            "",
        ),
        (
            json!({"id":"bad","steps":{}}),
            "spec.invalid_shape",
            "/steps",
        ),
        (
            json!({"id":"bad","foreach":{"items":"${{ inputs.items }}","as":"item","steps":[]}}),
            "spec.required",
            "/foreach/max_items",
        ),
        (
            json!({"id":"bad","loop":{"until":"${{ true }}","max_iterations":0,"steps":[]}}),
            "spec.invalid_value",
            "/loop/max_iterations",
        ),
        (
            json!({"id":"bad","title":"Bad","actor":"robot"}),
            "spec.invalid_value",
            "/actor",
        ),
        (
            json!({"id":"bad","title":"Bad","timeout":"18446744073709551615d"}),
            "spec.invalid_value",
            "/timeout",
        ),
        (
            json!({"id":"group","steps":[{"id":"Bad","title":"Bad"}]}),
            "spec.invalid_id",
            "/steps/0/id",
        ),
        (
            json!({"id":"bad","wait":{"for":"1s","signal":"go"}}),
            "spec.invalid_shape",
            "/wait",
        ),
    ] {
        let errors = normalize(&source(json!({"steps":[node]}))).unwrap_err();
        assert_eq!(errors[0].code, code, "{suffix}");
        assert_eq!(errors[0].path, format!("/spec/steps/0{suffix}"));
    }
    let normalized=normalize(&source(json!({"steps":[
        {"id":"group","steps":[],"catch":[],"finally":[]},
        {"id":"parallel","parallel":[],"join":"all","fail_fast":true},
        {"id":"each","foreach":{"items":"${{ inputs.items }}","as":"item","max_items":2,"steps":[]}},
        {"id":"loop","loop":{"until":"${{ true }}","max_iterations":2,"delay":"1s","steps":[]}},
        {"id":"switch","switch":{"cases":[{"when":"${{ true }}","steps":[]}],"default":{"steps":[]}}},
        {"id":"wait","wait":{"signal":"go","timeout":"1h30m"}},
        {"id":"approval","approval":{"prompt":"Continue?","options":["yes"],"approvers":{"users":["alice"]}}},
        {"id":"set","set":{"vars":{"result":"done"}}},
        {"id":"end","end":{"status":"succeeded","message":"Done"}}
    ]}))).unwrap();
    assert_eq!(normalized.steps.len(), 9);
    use runbook_spec::ir::{NodeKind, WaitKind};
    let NodeKind::Wait(wait) = &normalized.steps[5].kind else {
        panic!("expected wait")
    };
    assert_eq!(wait.kind, WaitKind::Signal("go".into()));
    assert_eq!(wait.timeout.unwrap().millis, 5_400_000);
}

#[test]
fn task_defaults_are_effective_and_local() {
    use runbook_spec::ir::{Actor, FailurePolicy, NodeKind};
    let plain = normalize(&source(json!({"steps":[task("plain")]}))).unwrap();
    let NodeKind::Task(t) = &plain.steps[0].kind else {
        panic!()
    };
    assert_eq!(t.actor, Actor::Any);
    assert_eq!(t.retry.max_attempts, 1);
    assert_eq!(t.retry.backoff, None);
    assert_eq!(plain.steps[0].on_failure, FailurePolicy::Fail);
    let out=normalize(&source(json!({"defaults":{"actor":"agent","timeout":"15m","on_failure":"handoff","retry":{"max_attempts":3,"backoff":"1s"}},"steps":[
        {"id":"group","steps":[task("inherited"),{"id":"overridden","title":"Override","actor":"human","timeout":"2s","on_failure":"continue","retry":{"max_attempts":2}}]},
        {"id":"control","set":{"vars":{"x":1}},"on_failure":"continue"}
    ]}))).unwrap();
    assert_eq!(out.defaults.actor, Actor::Agent);
    assert_eq!(out.steps[0].on_failure, FailurePolicy::Fail);
    assert_eq!(out.steps[0].timeout, None);
    assert_eq!(out.steps[1].on_failure, FailurePolicy::Continue);
    let NodeKind::Group(g) = &out.steps[0].kind else {
        panic!()
    };
    let NodeKind::Task(a) = &g.children[0].kind else {
        panic!()
    };
    let NodeKind::Task(b) = &g.children[1].kind else {
        panic!()
    };
    assert_eq!(a.actor, Actor::Agent);
    assert_eq!(a.retry, out.defaults.retry);
    let retry_defaults = &out.defaults.retry;
    assert_eq!(retry_defaults.max_attempts, 3);
    assert_eq!(retry_defaults.backoff.unwrap().millis, 1000);
    assert_eq!(g.children[0].timeout.unwrap().millis, 900_000);
    assert_eq!(g.children[0].on_failure, FailurePolicy::Handoff);
    assert_eq!(b.actor, Actor::Human);
    let explicit = &b.retry;
    assert_eq!(explicit.max_attempts, 2);
    assert_eq!(explicit.backoff.unwrap().millis, 1000);
    assert_eq!(g.children[1].timeout.unwrap().millis, 2000);
    assert_eq!(g.children[1].on_failure, FailurePolicy::Continue);
}

#[test]
fn data_schemas_are_reused_in_normalized_outputs() {
    use runbook_spec::{DataType, ir::NodeKind, validate_data};
    let out=normalize(&source(json!({"inputs":{"service":{"type":"string","required":true}},"steps":[{"id":"health","title":"Health","outputs":{"status_code":{"type":"integer","required":true}}}]}))).unwrap();
    let NodeKind::Task(task) = &out.steps[0].kind else {
        panic!()
    };
    let DataType::Object { properties } = &task.outputs.kind else {
        panic!()
    };
    assert!(matches!(properties["status_code"].kind, DataType::Integer));
    let errors = validate_data(&task.outputs, &json!({"status_code":"503"}));
    assert_eq!(errors[0].code, "data.type_error");
    assert_eq!(errors[0].path, "status_code");
    assert!(validate_data(&task.outputs, &json!({"status_code":503})).is_empty());
    let err=normalize(&source(json!({"steps":[{"id":"health","title":"Health","outputs":{"a/~.b":{"type":"integer","required":"yes"}}}]}))).unwrap_err();
    assert_eq!(err[0].path, "/spec/steps/0/outputs/a~1~0.b/required");
    let mut fields = serde_json::Map::new();
    for i in 0..10 {
        fields.insert(format!("f{i}"), json!({"type":"string","indexed":true}));
    }
    assert!(normalize(&source(json!({"fields":fields,"steps":[]}))).is_ok());
    fields.insert("eleventh".into(), json!({"type":"string","indexed":true}));
    let errors = normalize(&source(json!({"fields":fields,"steps":[]}))).unwrap_err();
    assert_eq!(errors[0].code, "spec.indexed_field_limit");
    assert_eq!(errors[0].path, "/spec/fields");
}

#[test]
fn all_flagship_configuration_survives_normalization() {
    use runbook_spec::ir::*;
    let yaml = include_str!("fixtures/post-deploy.yaml");
    let out = normalize(&parse(yaml).unwrap()).unwrap();
    assert_eq!(out.source_yaml, yaml);
    assert_eq!(out.metadata.owners, vec![Recipient::Team("oncall".into())]);
    assert_eq!(out.metadata.tags, vec!["deploy", "production"]);
    assert_eq!(out.execution.timeout.unwrap().millis, 7_200_000);
    assert_eq!(out.execution.cleanup_timeout.millis, 1_800_000);
    assert_eq!(
        out.execution.concurrency.as_ref().unwrap().key,
        "${{ inputs.service }}"
    );
    assert_eq!(out.execution.concurrency.as_ref().unwrap().limit, 1);
    assert_eq!(out.variables["result"], json!("undetermined"));
    assert_eq!(out.skills["error_rates"], "./skills/check-error-rates");
    assert_eq!(
        out.agent_params.as_ref(),
        json!({"repo":"acme/api", "workflow":"runbook-resume.yml"}).as_object()
    );
    assert_eq!(
        out.steps.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        vec!["smoke", "regions", "stabilize", "decide"]
    );
    let NodeKind::Parallel { children, .. } = &out.steps[0].kind else {
        panic!()
    };
    let NodeKind::Task(health) = &children[0].kind else {
        panic!()
    };
    assert!(health.check);
    assert!(
        matches!(&health.instructions,Some(Instructions::Markdown(s)) if s.contains("${{ inputs.service }}"))
    );
    let NodeKind::Task(login) = &children[1].kind else {
        panic!()
    };
    assert_eq!(
        login.checklist,
        vec!["Login page loads", "Dashboard shows data"]
    );
    let NodeKind::Foreach(each) = &out.steps[1].kind else {
        panic!()
    };
    assert_eq!(
        (each.alias.as_str(), each.concurrency, each.max_items),
        ("region", 3, 12)
    );
    let NodeKind::Task(rate) = &each.children[0].kind else {
        panic!()
    };
    assert_eq!(rate.parameters["region"], json!("${{ region }}"));
    assert_eq!(rate.parameters["window"], json!("10m"));
    assert_eq!(rate.evidence.required, vec![EvidenceKind::Link]);
    let NodeKind::Loop(loop_) = &out.steps[2].kind else {
        panic!()
    };
    assert_eq!(loop_.max_iterations, 3);
    assert_eq!(loop_.delay.unwrap().millis, 300_000);
    let NodeKind::Switch(switch) = &out.steps[3].kind else {
        panic!()
    };
    let NodeKind::Approval(approval) = &switch.cases[0].children[0].kind else {
        panic!()
    };
    assert_eq!(approval.approvers, vec![Recipient::Team("oncall".into())]);
    assert_eq!(
        approval.escalate_to,
        Some(vec![Recipient::Team("eng-leads".into())])
    );
    assert_eq!(
        approval
            .options
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        vec!["rollback", "keep"]
    );
    let NodeKind::Task(rollback) = &switch.cases[0].children[1].kind else {
        panic!()
    };
    assert!(rollback.destructive);
    assert_eq!(rollback.skill.as_deref(), Some("rollback"));
    assert_eq!(out.outputs["result"], json!("${{ vars.result }}"));
    assert_eq!(out.finally[0].id, "notify");

    let companion = source(
        json!({"report":"Result: ${{ outputs.result }}","agents":{"params":{"subscriber":"custom"}},"x-meta":{"opaque":true},"steps":[
            {"id":"human","title":"Human","assignee":"team:oncall","instructions":{"default":"Default","human":"Human text","agent":"Agent text"},"skippable":true,"allow_auto_decision":true,"evidence":{"accepted":["link","log"]},"agent":{"tools":["curl"],"commands":["curl /health"]}}
        ]}),
    );
    let out = normalize(&companion).unwrap();
    assert_eq!(out.report.as_deref(), Some("Result: ${{ outputs.result }}"));
    assert_eq!(out.source_json["spec"]["x-meta"], json!({"opaque":true}));
    let NodeKind::Task(t) = &out.steps[0].kind else {
        panic!()
    };
    assert!(
        matches!(&t.instructions,Some(Instructions::Audience{human:Some(s),..}) if s=="Human text")
    );
    assert!(matches!(&t.assignee,Some(Assignee::Recipient(Recipient::Team(s))) if s=="oncall"));
    assert_eq!(t.agent.tools, Some(json!(["curl"])));
    assert!(t.skippable && t.allow_auto_decision);
}

#[test]
fn normalization_resource_boundaries_are_explicit() {
    let base = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: limits, title: Limits}\nspec: {steps: []}\n";
    let mut exact = base.to_owned();
    exact.push('#');
    exact.push_str(&" ".repeat(1_048_576 - exact.len()));
    assert_eq!(exact.len(), 1_048_576);
    assert!(normalize(&parse(&exact).unwrap()).is_ok());
    exact.push(' ');
    assert_eq!(
        parse(&exact)
            .map(|_| ())
            .expect_err("oversized source rejected")[0]
            .code,
        "spec.source_limit"
    );
    let invalid = "[".repeat(1_048_577);
    assert_eq!(
        parse(&invalid)
            .map(|_| ())
            .expect_err("oversized source rejected")[0]
            .code,
        "spec.source_limit"
    );
    for (count, accepted) in [(4096, true), (4097, false)] {
        let out = normalize(&source(
            json!({"steps":(0..count).map(|i|task(&format!("n{i}"))).collect::<Vec<_>>()}),
        ));
        if accepted {
            assert_eq!(out.unwrap().steps.len(), count);
        } else {
            assert_eq!(out.unwrap_err()[0].code, "spec.node_limit");
        }
    }
    for (depth, accepted) in [(32, true), (33, false)] {
        let mut node = task("leaf");
        for i in 0..depth {
            node = json!({"id":format!("g{i}"),"steps":[node]});
        }
        let out = normalize(&source(json!({"steps":[node]})));
        if accepted {
            assert!(out.is_ok());
        } else {
            assert_eq!(out.unwrap_err()[0].code, "spec.control_depth_limit");
        }
    }
    // Opaque extension nesting is not control nesting; root/spec/x-deep adds2.
    let mut deep = json!(0);
    for _ in 0..126 {
        deep = json!([deep]);
    }
    let out = normalize(&source(json!({"steps":[],"x-deep":deep}))).unwrap();
    assert_eq!(out.steps.len(), 0);
    fn count(v: &Value) -> usize {
        1 + match v {
            Value::Array(a) => a.iter().map(count).sum(),
            Value::Object(m) => m.values().map(count).sum(),
            _ => 0,
        }
    }
    let base = source(json!({"steps":[],"x-wide":[]}));
    let overhead = count(base.source_json());
    for (total, accepted) in [(262_144, true), (262_145, false)] {
        let json=json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"steps":[],"x-wide":vec![0;total-overhead]}}).to_string();
        let parsed = parse(&json);
        if accepted {
            assert!(normalize(&parsed.unwrap()).is_ok());
        } else {
            assert_eq!(
                parsed
                    .map(|_| ())
                    .expect_err("oversized value count rejected")[0]
                    .code,
                "spec.source_value_limit"
            );
        }
    }
}

#[test]
fn behavior_fields_cannot_be_silently_ignored_on_control_nodes() {
    for (key, value) in [
        ("actor", json!("agent")),
        ("retry", json!({"max_attempts":2})),
        ("outputs", json!({"x":{"type":"string"}})),
        ("instructions", json!("Do this")),
        ("check", json!(true)),
        ("join", json!("all")),
    ] {
        let mut node = json!({"id":"set","set":{"vars":{"x":1}}});
        node[key] = value;
        let errors = normalize(&source(json!({"steps":[node]}))).unwrap_err();
        assert_eq!(errors[0].code, "spec.invalid_node_field");
        assert_eq!(errors[0].path, format!("/spec/steps/0/{key}"));
    }
}

#[test]
fn foreach_defaults_to_item_and_serial_execution() {
    use runbook_spec::ir::NodeKind;
    let spec = source(json!({"steps":[
        {"id":"implicit","foreach":{"items":"${{ inputs.items }}","max_items":2,"steps":[]}},
        {"id":"explicit","foreach":{"items":"${{ inputs.items }}","as":"entry","concurrency":4,"max_items":2,"steps":[]}}
    ]}));
    let original = spec.source_json().clone();
    let out = normalize(&spec).unwrap();
    for (node, alias, concurrency) in [(&out.steps[0], "item", 1), (&out.steps[1], "entry", 4)] {
        let NodeKind::Foreach(each) = &node.kind else {
            panic!()
        };
        assert_eq!(each.alias.as_str(), alias);
        assert_eq!(each.concurrency, concurrency);
    }
    assert_eq!(out.source_json, original);
}

#[test]
fn approval_recipient_omission_stays_distinct_for_policy_validation() {
    use runbook_spec::ir::{ApprovalTimeout, ExpiredPolicy, NodeKind};
    let mut body = json!({"steps":[{"id":"approval","approval":{"prompt":"Decide","options":[{"id":"rollback","title":"Roll back","auto":true}],"timeout":"30m","on_timeout":{"decide":"rollback"},"allow_agents":true}}]});
    let first = normalize(&source(body.clone())).unwrap();
    let NodeKind::Approval(a) = &first.steps[0].kind else {
        panic!()
    };
    assert_eq!(a.escalate_to, None);
    assert_eq!(a.on_timeout, ApprovalTimeout::Decide("rollback".into()));
    assert_eq!(a.escalation_timeout, a.timeout);
    assert_eq!(a.on_expired, ExpiredPolicy::Pause);
    assert!(a.options[0].auto && a.allow_agents);
    body["steps"][0]["approval"]["escalate_to"] = json!([]);
    let second = normalize(&source(body)).unwrap();
    let NodeKind::Approval(b) = &second.steps[0].kind else {
        panic!()
    };
    assert_eq!(b.escalate_to, Some(vec![]));
}

#[test]
fn declaration_map_extensions_remain_opaque_not_fields() {
    use runbook_spec::DataType;
    let original = source(
        json!({"inputs":{"x-catalog":{"untyped":"metadata"},"settings":{"type":"object","x-note":{"x-nested":"retained"},"properties":{"x-editor":{"ignored":true},"enabled":{"type":"boolean"}}},"literal":{"type":"string","default":"${{ x-never-executed }}"}},"steps":[]}),
    );
    let out = normalize(&original).unwrap();
    let DataType::Object { properties } = &out.inputs.kind else {
        panic!()
    };
    assert_eq!(properties.len(), 2);
    assert_eq!(
        properties["settings"].extensions["x-note"],
        json!({"x-nested":"retained"})
    );
    assert!(
        matches!(&properties["literal"].default,runbook_spec::DefaultValue::Value(v) if v==&json!("${{ x-never-executed }}"))
    );
    let DataType::Object { properties } = &properties["settings"].kind else {
        panic!()
    };
    assert_eq!(properties.len(), 1);
    assert!(properties.contains_key("enabled"));
    assert_eq!(out.source_json, original.source_json().clone());
}

#[test]
fn metadata_id_is_a_kebab_case_url_slug() {
    for id in [
        "",
        "../bad",
        "Bad Name",
        "UPPER",
        "under_score",
        "-first",
        "last-",
        "two--gaps",
        "é",
    ] {
        let spec = parse(
            &json!({"apiVersion":"therunbook.app/v1", "kind":"Runbook",
            "metadata":{"id":id,"title":"Test"},"spec":{"steps":[task("health")]}})
            .to_string(),
        )
        .unwrap();
        let errors = normalize(&spec).unwrap_err();
        assert_eq!(errors[0].code, "spec.invalid_id", "{id:?}");
        assert_eq!(errors[0].path, "/metadata/id");
    }
    for id in ["post-deploy", "a", "9-checks"] {
        let spec = parse(
            &json!({"apiVersion":"therunbook.app/v1", "kind":"Runbook",
            "metadata":{"id":id,"title":"Test"},"spec":{"steps":[task("health")]}})
            .to_string(),
        )
        .unwrap();
        assert_eq!(normalize(&spec).unwrap().metadata.id, id);
    }
}

#[test]
fn agent_params_normalization_preserves_opaque_values() {
    let params = json!({"repo":"acme/api","x-opaque":{"dispatch":{"github":null},"empty":[],"expression":"${{ missing.root }}"},"large":18446744073709551616_u128});
    let parsed = source(json!({"agents":{"params":params},"steps":[]}));
    let normalized = normalize(&parsed).unwrap();
    assert_eq!(normalized.agent_params.as_ref(), params.as_object());
    assert_eq!(normalized.source_json["spec"]["agents"]["params"], params);
    assert!(
        normalize(&source(json!({"steps":[]})))
            .unwrap()
            .agent_params
            .is_none()
    );
    for value in [Value::Null, json!([]), json!(false), json!(5), json!("no")] {
        let errors = normalize(&source(json!({"agents":{"params":value},"steps":[]}))).unwrap_err();
        assert_eq!(errors[0].code, "spec.type_error");
        assert_eq!(errors[0].path, "/spec/agents/params");
    }
}

#[test]
fn execution_concurrency_defaults_to_queue() {
    use runbook_spec::ir::ConcurrencyPolicy;
    for policy in [None, Some("queue")] {
        let mut body = json!({"execution":{"concurrency":{"key":"service","limit":2}},"steps":[]});
        if let Some(policy) = policy {
            body["execution"]["concurrency"]["on_conflict"] = json!(policy);
        }
        let spec = source(body);
        let original = spec.source_json().clone();
        let out = normalize(&spec).unwrap();
        assert_eq!(
            out.execution.concurrency.unwrap().on_conflict,
            ConcurrencyPolicy::Queue
        );
        assert_eq!(out.source_json, original);
    }
}

#[test]
fn partial_task_retry_inherits_each_omitted_default() {
    use runbook_spec::ir::NodeKind;
    let spec = source(
        json!({"defaults":{"retry":{"max_attempts":3,"backoff":"2s"}},"steps":[
            {"id":"attempts","title":"Attempts","retry":{"max_attempts":2}},
            {"id":"backoff","title":"Backoff","retry":{"backoff":"5s"}},
            {"id":"empty","title":"Empty","retry":{}},
            {"id":"omitted","title":"Omitted"},
            {"id":"explicit","title":"Explicit","retry":{"max_attempts":4,"backoff":"7s"}}
        ]}),
    );
    let original = spec.source_json().clone();
    let out = normalize(&spec).unwrap();
    for (node, attempts, backoff) in [
        (&out.steps[0], 2, 2000),
        (&out.steps[1], 3, 5000),
        (&out.steps[2], 3, 2000),
        (&out.steps[3], 3, 2000),
        (&out.steps[4], 4, 7000),
    ] {
        let NodeKind::Task(task) = &node.kind else {
            panic!()
        };
        let retry = &task.retry;
        assert_eq!(retry.max_attempts, attempts);
        assert_eq!(retry.backoff.map(|v| v.millis), Some(backoff));
    }
    assert_eq!(out.source_json, original);
}
