//! Behavior of the checked-in pilot source profile, not publication eligibility.
use runbook_spec::{
    Support, classify, normalize, parse, parse_declarations, schema_json, validate_capabilities,
};
use serde_json::{Value, json};

fn validator() -> jsonschema::Validator {
    let schema: Value =
        serde_json::from_str(include_str!("../../../schemas/runbook-v1.json")).unwrap();
    jsonschema::validator_for(&schema).expect("source schema compiles offline")
}

fn document(body: Value) -> Value {
    json!({"apiVersion":"therunbook.app/v1","kind":"Runbook",
        "metadata":{"id":"checks","title":"Checks"},"spec":body})
}

fn task(id: &str) -> Value {
    json!({"id":id,"title":id})
}

fn accepts(validator: &jsonschema::Validator, candidate: &Value) {
    let errors: Vec<_> = validator
        .iter_errors(candidate)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{candidate}\n{errors:#?}");
}

fn rejects(validator: &jsonschema::Validator, candidate: &Value) {
    assert!(
        !validator.is_valid(candidate),
        "unexpectedly accepted {candidate}"
    );
}

fn normalizes(candidate: &Value) {
    let source = parse(&candidate.to_string()).unwrap();
    assert!(normalize(&source).is_ok(), "{:?}", normalize(&source));
}

#[test]
fn source_schema_is_self_contained_and_distinct_from_data_schema() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../schemas/runbook-v1.json")).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["$id"], "https://therunbook.app/schemas/v1.json");
    fn local_refs(value: &Value, root: &Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "$ref" {
                        let reference = value.as_str().unwrap();
                        assert!(reference.starts_with("#/"), "{reference}");
                        assert!(root.pointer(&reference[1..]).is_some(), "{reference}");
                    }
                    local_refs(value, root);
                }
            }
            Value::Array(items) => {
                for item in items {
                    local_refs(item, root);
                }
            }
            _ => {}
        }
    }
    local_refs(&schema, &schema);
    let validator = validator();
    accepts(&validator, &document(json!({"steps":[]})));
    let output = json!({"status_code":200});
    rejects(&validator, &output);
    let data_schema =
        parse_declarations(&json!({"status_code":{"type":"integer","required":true}})).unwrap();
    let data_validator = jsonschema::validator_for(&schema_json(&data_schema)).unwrap();
    assert!(data_validator.is_valid(&output));
    assert!(!data_validator.is_valid(&json!({"status_code":"200"})));
}

#[test]
fn source_schema_accepts_minimal_and_unchanged_flagship() {
    let validator = validator();
    let minimal = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: { id: store-opening, title: Store opening }\nspec:\n  steps:\n    - { id: lights, title: Turn on lights and music }\n    - { id: till, title: Count the till float, outputs: { amount: { type: number } } }\n    - { id: doors, title: 'Unlock the doors at 9:00' }\n";
    for yaml in [minimal, include_str!("fixtures/post-deploy.yaml")] {
        let source = parse(yaml).unwrap();
        accepts(&validator, source.source_json());
        assert!(normalize(&source).is_ok());
    }
}

#[test]
fn source_schema_covers_every_supported_node_shape() {
    let validator = validator();
    let nodes = [
        json!({"id":"task","title":"","actor":"any","assignee":"user:jo","if":"${{ true }}","instructions":{"default":"D","human":"H","agent":"A"},"checklist":["check"],"check":true,"skippable":false,"destructive":false,"allow_auto_decision":true,"skill":"check","with":{},"outputs":{"code":{"type":"integer"}},"evidence":{"required":["link"],"accepted":["log"]},"retry":{"backoff":"01s"}}),
        json!({"id":"group","steps":[task("child")],"catch":[task("catcher")],"finally":[task("cleanup")]}),
        json!({"id":"parallel","parallel":[task("branch")],"join":"all","fail_fast":true}),
        json!({"id":"parallel","parallel":[task("branch")],"join":"any"}),
        json!({"id":"parallel","parallel":[task("branch")],"join":{"at_least":1}}),
        json!({"id":"foreach","foreach":{"items":"${{ [1] }}","max_items":1,"steps":[task("item_task")]}}),
        json!({"id":"loop","loop":{"until":"${{ true }}","max_iterations":1,"delay":"0s1ms","steps":[task("iteration")]}}),
        json!({"id":"loop","loop":{"while":"${{ false }}","max_iterations":1,"steps":[task("iteration")]}}),
        json!({"id":"switch","switch":{"cases":[{"when":"${{ true }}","steps":[task("case_task")]}],"default":{"steps":[task("fallback")]}}}),
        json!({"id":"wait","wait":{"for":"1h30m","timeout":"2h"}}),
        json!({"id":"approval","approval":{"prompt":"Proceed?","options":["no",{"id":"yes","title":"Yes","auto":true}],"approvers":{"users":["jo"],"teams":["ops"]},"min":1,"timeout":"1h","on_timeout":{"decide":"yes"},"escalate_to":["jo","team:ops","user:jo"],"escalation_timeout":"1h","on_expired":"pause","allow_agents":true}}),
        json!({"id":"set","set":{"vars":{"value":1},"fields":{"severity":"none"}}}),
        json!({"id":"end","end":{"status":"succeeded","message":"Done"}}),
    ];
    for node in nodes {
        let candidate = document(
            json!({"steps":[node],"defaults":{"retry":{}},"execution":{"concurrency":{"key":"key","limit":1}}}),
        );
        accepts(&validator, &candidate);
        normalizes(&candidate);
    }
    for node in [
        json!({"id":"g","steps":[],"catch":[],"finally":[]}),
        json!({"id":"p","parallel":[]}),
        json!({"id":"s","switch":{"cases":[],"default":{"steps":[]}}}),
        json!({"id":"w","wait":{"until":"${{ true }}"}}),
        json!({"id":"w","wait":{"signal":"ready"}}),
        json!({"id":"a","approval":{"prompt":"?","options":["ok"],"timeout":"1m","on_timeout":"escalate","on_expired":"fail"}}),
        json!({"id":"t","title":"T","retry":{"max_attempts":2},"instructions":"Do it"}),
        json!({"id":"s","set":{}}),
    ] {
        let candidate = document(json!({"steps":[node],"finally":[]}));
        accepts(&validator, &candidate);
        normalizes(&candidate);
    }
}

#[test]
fn source_schema_rejects_structural_mistakes() {
    let validator = validator();
    let minimal = document(json!({"steps":[task("check")]}));
    accepts(&validator, &minimal);
    for key in ["metadata", "spec"] {
        let mut candidate = minimal.clone();
        candidate.as_object_mut().unwrap().remove(key);
        rejects(&validator, &candidate);
    }
    // LANG7: `apiVersion`, `kind`, `metadata.title` and task titles are
    // optional (defaulted or derived from IDs); IDs stay required.
    let mut lean = minimal.clone();
    for key in ["apiVersion", "kind"] {
        lean.as_object_mut().unwrap().remove(key);
    }
    lean["metadata"].as_object_mut().unwrap().remove("title");
    lean["spec"]["steps"] = json!([{"id":"check"}]);
    accepts(&validator, &lean);
    normalizes(&lean);
    let mut no_id = lean.clone();
    no_id["metadata"].as_object_mut().unwrap().remove("id");
    rejects(&validator, &no_id);
    for (pointer, bad) in [
        ("/apiVersion", json!("v2")),
        ("/kind", json!("Task")),
        ("/metadata", json!([])),
        ("/metadata/id", json!("bad_slug")),
        ("/metadata/id", json!("-bad")),
        ("/metadata/title", json!(1)),
        ("/spec", json!([])),
        ("/spec/steps", json!({})),
    ] {
        let mut candidate = minimal.clone();
        *candidate.pointer_mut(pointer).unwrap() = bad;
        rejects(&validator, &candidate);
    }
    for node in [
        json!({"title":"Missing ID"}),
        task("Bad"),
        task("a-b"),
        task(&"a".repeat(64)),
        json!({"id":"g","steps":[],"parallel":[]}),
        json!({"id":"g","title":"G","steps":[],"actor":"human"}),
        json!({"id":"g","steps":[],"retry":{}}),
        json!({"id":"t","title":"T","catch":[]}),
        json!({"id":"t","title":"T","actor":"robot"}),
        json!({"id":"t","title":"T","on_failure":"ignore"}),
        json!({"id":"t","title":"T","check":1}),
        json!({"id":"t","title":"T","instructions":{"other":"x"}}),
        json!({"id":"t","title":"T","with":[]}),
        json!({"id":"t","title":"T","checklist":[1]}),
        json!({"id":"t","title":"T","unexpected":null}),
        json!({"id":"g","steps":{}}),
        json!({"id":"f","foreach":{"items":"[]","steps":[]}}),
        json!({"id":"l","loop":{"until":"true","steps":[]}}),
        json!({"id":"s","switch":{"cases":[{"steps":[]}]}}),
        json!({"id":"s","switch":{"cases":[],"default":{}}}),
        json!({"id":"w","wait":{}}),
        json!({"id":"w","wait":{"for":"1s","signal":"go"}}),
        json!({"id":"a","approval":{"prompt":"?","options":[]}}),
        json!({"id":"a","approval":{"prompt":"?","options":[""]}}),
        json!({"id":"a","approval":{"prompt":"?","options":[{}]}}),
        json!({"id":"a","approval":{"prompt":"?","options":[{"id":"","auto":true}]}}),
        json!({"id":"a","approval":{"prompt":"?","options":[{"id":"yes","auto":"true"}]}}),
        json!({"id":"a","approval":{"prompt":"?","options":["yes"],"approvers":{"users":[""]}}}),
        json!({"id":"a","approval":{"prompt":"?","options":["yes"],"on_timeout":{"other":"yes"}}}),
        json!({"id":"a","approval":{"prompt":"?","options":["yes"],"on_expired":"succeed"}}),
        json!({"id":"e","end":{"status":"done"}}),
        json!({"id":"s","set":{"vars":[]}}),
        json!(null),
    ] {
        rejects(&validator, &document(json!({"steps":[node]})));
    }
    for value in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!(4294967296_u64),
        json!("1"),
    ] {
        for body in [
            json!({"steps":[],"execution":{"concurrency":{"key":"k","limit":value}}}),
            json!({"steps":[],"defaults":{"retry":{"max_attempts":value}}}),
            json!({"steps":[{"id":"f","foreach":{"items":"[]","max_items":value,"steps":[]}}]}),
            json!({"steps":[{"id":"f","foreach":{"items":"[]","max_items":1,"concurrency":value,"steps":[]}}]}),
            json!({"steps":[{"id":"l","loop":{"until":"true","max_iterations":value,"steps":[]}}]}),
            json!({"steps":[{"id":"a","approval":{"prompt":"?","options":["ok"],"min":value}}]}),
        ] {
            rejects(&validator, &document(body));
        }
    }
    for value in [json!(1), json!(4294967295_u64), json!(1.0)] {
        let candidate = document(
            json!({"steps":[],"defaults":{"retry":{"max_attempts":value}},"execution":{"concurrency":{"key":"k","limit":value}}}),
        );
        accepts(&validator, &candidate);
        normalizes(&candidate);
    }
    for duration in ["0s", "00ms", "1", "1.5s", "-1s", "PT1S", "1s junk", ""] {
        rejects(
            &validator,
            &document(json!({"steps":[],"execution":{"timeout":duration}})),
        );
    }
    for body in [
        json!({}),
        json!({"steps":[],"skills":{"a":1}}),
        json!({"steps":[],"execution":{"concurrency":{"limit":1}}}),
        json!({"steps":[],"vars":[]}),
        json!({"steps":[],"outputs":[]}),
    ] {
        rejects(&validator, &document(body));
    }
    let mut candidate = minimal;
    candidate["metadata"] = json!({"id":"9-checks","title":""});
    accepts(&validator, &candidate);
    // metadata.id is at most 63 bytes, like node IDs (R52).
    candidate["metadata"]["id"] = json!("a".repeat(64));
    rejects(&validator, &candidate);
    candidate["metadata"]["id"] = json!("a".repeat(63));
    accepts(&validator, &candidate);
    for owner in ["", "team:", "user:", "unknown:jo"] {
        candidate["metadata"]["owners"] = json!([owner]);
        rejects(&validator, &candidate);
    }
}

#[test]
fn source_schema_enforces_pilot_capabilities_contextually() {
    let validator = validator();
    let mut cases = vec![
        ("dispatch", json!({"steps":[],"agents":{"dispatch":null}})),
        ("dispatch", json!({"steps":[],"agents":{"dispatch":{}}})),
        ("triggers", json!({"steps":[],"triggers":[]})),
        (
            "call",
            json!({"steps":[{"id":"t","title":"T","call":"other"}]}),
        ),
        (
            "while",
            json!({"steps":[{"id":"l","loop":{"until":"true","while":"true","max_iterations":1,"steps":[]}}]}),
        ),
        (
            "until",
            json!({"steps":[{"id":"l","loop":{"max_iterations":1,"steps":[]}}]}),
        ),
        (
            "join",
            json!({"steps":[{"id":"p","parallel":[],"join":"some"}]}),
        ),
        (
            "at_least",
            json!({"steps":[{"id":"p","parallel":[],"join":{"at_least":0}}]}),
        ),
        (
            "on_conflict",
            json!({"steps":[],"execution":{"concurrency":{"key":"k","limit":1,"on_conflict":"cancel"}}}),
        ),
    ];
    for key in ["runner", "model", "max_cost_usd", "max_tokens"] {
        cases.push((
            key,
            json!({"steps":[{"id":"t","title":"T","agent":{key:null}}]}),
        ));
    }
    // UP1 made `file` and `screenshot` supported; other kinds stay refused.
    for kind in ["video", "photo"] {
        cases.push((
            kind,
            json!({"steps":[{"id":"t","title":"T","evidence":{"required":[kind]}}]}),
        ));
        cases.push((
            kind,
            json!({"steps":[{"id":"t","title":"T","evidence":{"accepted":[kind]}}]}),
        ));
    }
    // LANG5 supported multi_enum, date, datetime, duration, url, email, user.
    // secret_ref remains rejected.
    cases.push((
        "secret_ref",
        json!({"steps":[],"inputs":{"value":{"type":"secret_ref"}}}),
    ));
    for (name, body) in cases {
        rejects(&validator, &document(body));
        let candidate = document(
            json!({"steps":[{"id":"t","title":"T","with":{name:{"pattern":true}}}],"inputs":{name:{"type":"string"}},"vars":{name:null},"outputs":{name:[1]},"agents":{"params":{name:true}}}),
        );
        accepts(&validator, &candidate);
        normalizes(&candidate);
    }
}

#[test]
fn source_schema_preserves_extensions_and_advisory_values() {
    let validator = validator();
    // Every language-object context is exercised with each JSON extension shape.
    let base = document(json!({
        "execution":{"concurrency":{"key":"k","limit":1}},
        "inputs":{"value":{"type":"list","items":{"type":"object","properties":{"child":{"type":"string"}}}}},
        "fields":{"value":{"type":"string"}},"skills":{},"agents":{"params":{}},"defaults":{"retry":{}},
        "steps":[
            {"id":"t","title":"T","instructions":{},"outputs":{"code":{"type":"integer"}},"evidence":{},"agent":{},"retry":{}},
            {"id":"g","steps":[],"catch":[],"finally":[]},
            {"id":"p","parallel":[]},
            {"id":"f","foreach":{"items":"[]","max_items":1,"steps":[]}},
            {"id":"l","loop":{"until":"true","max_iterations":1,"steps":[]}},
            {"id":"s","switch":{"cases":[{"when":"true","steps":[]}],"default":{"steps":[]}}},
            {"id":"w","wait":{"signal":"go"}},
            {"id":"a","approval":{"prompt":"?","options":[{"id":"ok"}],"approvers":{},"on_timeout":{"decide":"ok"}}},
            {"id":"v","set":{}},{"id":"e","end":{"status":"succeeded"}}
        ]
    }));
    let paths = [
        "",
        "/metadata",
        "/spec",
        "/spec/execution",
        "/spec/execution/concurrency",
        "/spec/inputs",
        "/spec/inputs/value",
        "/spec/inputs/value/items",
        "/spec/inputs/value/items/properties",
        "/spec/inputs/value/items/properties/child",
        "/spec/fields",
        "/spec/fields/value",
        "/spec/skills",
        "/spec/agents",
        "/spec/defaults",
        "/spec/defaults/retry",
        "/spec/steps/0",
        "/spec/steps/0/instructions",
        "/spec/steps/0/outputs",
        "/spec/steps/0/outputs/code",
        "/spec/steps/0/evidence",
        "/spec/steps/0/agent",
        "/spec/steps/0/retry",
        "/spec/steps/1",
        "/spec/steps/2",
        "/spec/steps/3",
        "/spec/steps/3/foreach",
        "/spec/steps/4",
        "/spec/steps/4/loop",
        "/spec/steps/5",
        "/spec/steps/5/switch",
        "/spec/steps/5/switch/cases/0",
        "/spec/steps/5/switch/default",
        "/spec/steps/6",
        "/spec/steps/6/wait",
        "/spec/steps/7",
        "/spec/steps/7/approval",
        "/spec/steps/7/approval/options/0",
        "/spec/steps/7/approval/approvers",
        "/spec/steps/7/approval/on_timeout",
        "/spec/steps/8",
        "/spec/steps/8/set",
        "/spec/steps/9",
        "/spec/steps/9/end",
    ];
    for path in paths {
        // Declaration/skill maps accept user-chosen names; language objects do not.
        if ![
            "/spec/inputs",
            "/spec/inputs/value/items/properties",
            "/spec/fields",
            "/spec/skills",
            "/spec/steps/0/outputs",
        ]
        .contains(&path)
        {
            let mut candidate = base.clone();
            candidate
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("unknown_keyword".into(), json!(null));
            rejects(&validator, &candidate);
        }
        let mut candidate = base.clone();
        *candidate.pointer_mut(path).unwrap() = json!([]);
        rejects(&validator, &candidate);
        for extension in [
            json!(null),
            json!(7),
            json!("text"),
            json!(false),
            json!([{"dispatch":null}]),
            json!({"runner":"shell","type":"secret_ref","pattern":".*"}),
        ] {
            let mut candidate = base.clone();
            candidate
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("x-test".into(), extension.clone());
            accepts(&validator, &candidate);
            assert!(
                matches!(
                    classify(&format!("{path}/x-test"), &extension),
                    Support::Preserved { .. }
                ),
                "{path}"
            );
            let source = parse(&candidate.to_string()).unwrap();
            assert!(validate_capabilities(&source).is_empty());
        }
    }
    for value in [
        json!(null),
        json!(1),
        json!(false),
        json!("command"),
        json!(["a"]),
        json!({"runner":true,"dispatch":null}),
    ] {
        let candidate = document(
            json!({"steps":[{"id":"t","title":"T","agent":{"tools":value,"commands":value}}]}),
        );
        accepts(&validator, &candidate);
        normalizes(&candidate);
        for key in ["tools", "commands"] {
            assert!(matches!(
                classify(&format!("/spec/steps/0/agent/{key}"), &value),
                Support::Preserved { .. }
            ));
        }
    }
    for key in ["unknown", "runner", "model", "max_tokens", "max_cost_usd"] {
        rejects(
            &validator,
            &document(json!({"steps":[{"id":"t","title":"T","agent":{key:null}}]})),
        );
    }
}

#[test]
fn source_schema_agents_params_is_vendor_neutral() {
    let validator = validator();
    for params in [
        json!({}),
        json!({"dispatch":{"github":{"model":"x"}},"x-local":null,"expression":"${{ missing.root }}","array":[true,4,null]}),
    ] {
        let candidate = document(json!({"steps":[],"agents":{"params":params}}));
        accepts(&validator, &candidate);
        normalizes(&candidate);
    }
    for path in [
        "/spec/agents/params/dispatch/github/model",
        "/spec/agents/params/x-local",
        "/spec/agents/params/x-local/runner",
    ] {
        assert!(matches!(
            classify(path, &json!(null)),
            Support::Supported { .. }
        ));
    }
    for params in [
        json!(null),
        json!([]),
        json!("params"),
        json!(1),
        json!(false),
    ] {
        rejects(
            &validator,
            &document(json!({"steps":[],"agents":{"params":params}})),
        );
    }
    rejects(
        &validator,
        &document(json!({"steps":[],"agents":{"params":{},"dispatch":null}})),
    );
    for size in [4096, 4097] {
        let params = json!({"v":"a".repeat(size-8)});
        assert_eq!(serde_json::to_vec(&params).unwrap().len(), size);
        let candidate = document(json!({"steps":[],"agents":{"params":params}}));
        accepts(&validator, &candidate);
        let errors = validate_capabilities(&parse(&candidate.to_string()).unwrap());
        if size == 4096 {
            assert!(errors.is_empty());
        } else {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].code, "spec.agents_params_too_large");
        }
    }
}

#[test]
fn source_schema_declarations_match_source_contexts() {
    let validator = validator();
    let valid = [
        json!({"type":"string","min":0,"max":10,"default":"hello","required":true,"title":"Name","description":"A name"}),
        json!({"type":"text","max":100}),
        json!({"type":"number","min":-1.5,"max":4}),
        json!({"type":"integer","min":-1.5,"default":1}),
        json!({"type":"boolean","default":false}),
        json!({"type":"enum","options":["","ok"],"default":"ok"}),
        json!({"type":"list","items":{"type":"object","properties":{"nested":{"type":"list","items":{"type":"integer"}}}},"min":0}),
        json!({"type":"object"}),
        json!({"type":"object","properties":{"type":{"type":"string"},"pattern":{"type":"string"},"a.b/c":{"type":"string"},"x-note":null}}),
        json!({"type":"string","pattern":"^[a-z]+$"}),
        json!({"type":"multi_enum","options":["a","b"],"default":["a"]}),
        json!({"type":"date","pattern":"^\\d{4}-\\d{2}-\\d{2}$","default":"2026-11-12"}),
        json!({"type":"datetime","default":"2026-11-12T10:00:00Z"}),
        json!({"type":"duration","default":"1h30m"}),
        json!({"type":"url","default":"https://example.com"}),
        json!({"type":"email","default":"user@example.com"}),
        json!({"type":"user","default":"user:jo"}),
    ];
    for declaration in valid {
        let candidate = document(
            json!({"steps":[{"id":"t","title":"T","outputs":{"value":declaration,"x-meta":17}}],"inputs":{"value":declaration,"x-meta":[]},"fields":{"value":declaration,"x-meta":null}}),
        );
        accepts(&validator, &candidate);
        normalizes(&candidate);
    }
    for declaration in [
        json!({}),
        json!({"type":"list"}),
        json!({"type":"enum"}),
        json!({"type":"enum","options":[]}),
        json!({"type":"enum","options":["x","x"]}),
        json!({"type":"enum","options":[1]}),
        json!({"type":"multi_enum"}),
        json!({"type":"multi_enum","options":[]}),
        json!({"type":"multi_enum","options":["x","x"]}),
        json!({"type":"multi_enum","options":[1]}),
        json!({"type":"string","items":{"type":"string"}}),
        json!({"type":"integer","options":["x"]}),
        json!({"type":"date","options":["x"]}),
        json!({"type":"list","items":{"type":"string"},"properties":{}}),
        json!({"type":"object","properties":[]}),
        json!({"type":"boolean","min":0}),
        json!({"type":"enum","options":["x"],"max":1}),
        json!({"type":"object","min":0}),
        json!({"type":"string","min":-1}),
        json!({"type":"text","max":1.5}),
        json!({"type":"list","items":{"type":"string"},"max":-1}),
        json!({"type":"number","min":"1"}),
        json!({"type":"string","required":[]}),
        json!({"type":"string","title":1}),
        json!({"type":"string","description":false}),
        json!({"type":"string","default":null}),
        json!({"type":"boolean","default":"true"}),
    ] {
        rejects(
            &validator,
            &document(json!({"steps":[],"inputs":{"value":declaration}})),
        );
    }
    for flag in ["indexed", "locked"] {
        let field = json!({"type":"string",flag:true});
        accepts(
            &validator,
            &document(json!({"steps":[],"fields":{"value":field}})),
        );
        for declaration in [
            field.clone(),
            json!({"type":"list","items":field}),
            json!({"type":"object","properties":{"nested":field}}),
        ] {
            rejects(
                &validator,
                &document(json!({"steps":[],"inputs":{"value":declaration}})),
            );
            rejects(
                &validator,
                &document(
                    json!({"steps":[{"id":"t","title":"T","outputs":{"value":declaration}}]}),
                ),
            );
        }
        for declaration in [
            json!({"type":"list","items":field}),
            json!({"type":"object","properties":{"nested":field}}),
            json!({"type":"string",flag:"true"}),
        ] {
            rejects(
                &validator,
                &document(json!({"steps":[],"fields":{"value":declaration}})),
            );
        }
    }
}

#[test]
fn source_schema_does_not_claim_semantic_validation() {
    let validator = validator();
    for (candidate, code) in [
        (
            document(json!({"steps":[task("same"),{"id":"group","steps":[task("same")]}]})),
            "spec.duplicate_id",
        ),
        (
            document(
                json!({"steps":[],"inputs":{"choice":{"type":"enum","options":["ok"],"default":"absent"}}}),
            ),
            "spec.invalid_schema",
        ),
    ] {
        accepts(&validator, &candidate);
        let errors = normalize(&parse(&candidate.to_string()).unwrap()).unwrap_err();
        assert!(errors.iter().any(|error| error.code == code), "{errors:?}");
    }
}

#[path = "support/structural_cases.rs"]
mod structural_cases;
#[test]
fn shared_compiler_structural_profile_matches_source_artifact() {
    let validator = validator();
    for (source, expected) in structural_cases::cases() {
        assert_eq!(validator.is_valid(&source), expected, "{source}");
    }
}

/// LANG2: `fail_fast: true` is valid only with `join: all` (or no join), in
/// the schema exactly as in the compiler.
#[test]
fn schema_and_compiler_agree_on_fail_fast_with_joins() {
    let validator = validator();
    for join in [
        None,
        Some(json!("all")),
        Some(json!("any")),
        Some(json!({"at_least": 1})),
    ] {
        for fail_fast in [None, Some(false), Some(true)] {
            let mut node = json!({"id":"p","parallel":[task("a"), task("b")]});
            if let Some(join) = &join {
                node["join"] = join.clone();
            }
            if let Some(fail_fast) = fail_fast {
                node["fail_fast"] = json!(fail_fast);
            }
            let candidate = document(json!({ "steps": [node] }));
            let schema_ok = validator.is_valid(&candidate);
            let compiler_ok = runbook_spec::compile(
                &parse(&candidate.to_string()).unwrap(),
                &runbook_spec::CompileContext::default(),
            )
            .is_ok();
            assert_eq!(
                schema_ok, compiler_ok,
                "join {join:?}, fail_fast {fail_fast:?}"
            );
            let all = join.as_ref().is_none_or(|j| j == "all");
            assert_eq!(compiler_ok, all || fail_fast != Some(true), "join {join:?}");
        }
    }
}

#[test]
fn call_nodes_match_the_compiler() {
    let validator = validator();
    let call = document(json!({"steps": [{"id": "migrate", "call": {
        "runbook": "db-migrate", "version": 3, "with": {"env": "prod"},
        "outputs": {"rows": {"type": "integer"}}, "x-ui": {}
    }}]}));
    accepts(&validator, &call);
    normalizes(&call);
    for bad in [
        json!({"runbook": "db-migrate", "timeout": "1h"}),
        json!({"runbook": "Not A Slug"}),
        json!({"version": 3}),
        json!({"runbook": "db-migrate", "version": 0}),
    ] {
        rejects(
            &validator,
            &document(json!({"steps": [{"id": "migrate", "call": bad}]})),
        );
    }
    // `call` is a control keyword: never alongside task keys or another one.
    rejects(
        &validator,
        &document(json!({"steps": [{"id": "x", "title": "X", "actor": "human",
            "call": {"runbook": "a"}}]})),
    );
    rejects(
        &validator,
        &document(json!({"steps": [{"id": "x", "wait": {"for": "1m"},
            "call": {"runbook": "a"}}]})),
    );
}
