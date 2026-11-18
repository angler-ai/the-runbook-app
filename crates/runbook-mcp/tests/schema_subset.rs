//! CF7: tool arguments are checked by `runbook_mcp::schema`, a validator
//! for exactly the JSON Schema keywords the tool catalog uses (it keeps
//! `jsonschema` out of the Cloudflare Worker). These tests keep the subset
//! honest: every tool schema uses only implemented keywords, and on a corpus
//! of arguments the subset agrees with `jsonschema` (a dev-dependency).
use runbook_mcp::{schema, tool_definitions};
use serde_json::{Value, json};

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const CLAIM: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3";
const RBV: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXYZ6";

fn schemas() -> Vec<(String, Value)> {
    tool_definitions()
        .into_iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_owned(),
                t["inputSchema"].clone(),
            )
        })
        .collect()
}

#[test]
fn every_tool_schema_uses_only_implemented_keywords() {
    let all = schemas();
    assert!(all.len() > 10);
    for (name, input) in &all {
        let unsupported = schema::unsupported(input);
        assert!(
            unsupported.is_empty(),
            "{name} uses keywords the validator does not implement: {unsupported:?}"
        );
    }
}

#[test]
fn an_unimplemented_keyword_is_reported_and_fails_closed() {
    let s = json!({"type": "object", "properties": {"a": {"type": "string", "format": "uuid"}}});
    assert_eq!(
        schema::unsupported(&s),
        vec!["/properties/a/format".to_owned()]
    );
    let problems = schema::validate(&s, &json!({"a": "x"}));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("format"), "{problems:?}");
}

#[test]
fn problems_carry_the_field_path() {
    let report = schemas()
        .into_iter()
        .find(|(n, _)| n == "report_step")
        .unwrap()
        .1;
    let problems = schema::validate(
        &report,
        &json!({"step_run_id": SR, "claim": {"id": "nope", "generation": 0}, "status": "done", "extra": 1}),
    );
    let joined = problems.join("\n");
    for expected in ["/claim/id:", "/claim/generation:", "/status:", "extra"] {
        assert!(joined.contains(expected), "missing {expected} in {joined}");
    }
}

/// Arguments for every tool: the empty object, an unknown key, wrong types
/// for each declared property, and well-formed values.
fn corpus(input: &Value) -> Vec<Value> {
    let mut out = vec![
        json!({}),
        json!({"unexpected": true}),
        json!([]),
        json!("x"),
    ];
    let props = input["properties"].as_object().cloned().unwrap_or_default();
    let good = |key: &str, p: &Value| -> Value {
        match (key, p["type"].as_str()) {
            ("execution_id", _) => json!(EXE),
            ("step_run_id", _) => json!(SR),
            ("claim", _) => json!({"id": CLAIM, "generation": 1}),
            ("version", _) => json!(RBV),
            (_, Some("string")) => match p["enum"].as_array() {
                Some(e) => e[0].clone(),
                None => json!("ops"),
            },
            (_, Some("integer")) => json!(1),
            (_, Some("number")) => json!(1.5),
            (_, Some("boolean")) => json!(true),
            (_, Some("array")) => json!(["a"]),
            (_, Some("object")) => json!({}),
            _ => json!(1),
        }
    };
    let mut full = serde_json::Map::new();
    for (key, p) in &props {
        full.insert(key.clone(), good(key, p));
    }
    out.push(Value::Object(full.clone()));
    for (key, p) in &props {
        for bad in [
            json!(null),
            json!(0),
            json!(-1),
            json!(1000),
            json!(2.5),
            json!(""),
            json!("x".repeat(300)),
            json!("exe_bad"),
            json!(["a", 1]),
            json!({"id": CLAIM}),
            json!({"id": CLAIM, "generation": 1, "x": 1}),
            json!(true),
            json!(3),
            json!(RBV),
            json!({"k": "v"}),
        ] {
            let mut one = full.clone();
            one.insert(key.clone(), bad);
            out.push(Value::Object(one));
        }
        let mut missing = full.clone();
        missing.remove(key);
        out.push(Value::Object(missing));
        let _ = p;
    }
    out
}

#[test]
fn the_subset_agrees_with_jsonschema_on_every_tool() {
    let mut checked = 0;
    for (name, input) in schemas() {
        let reference = jsonschema::validator_for(&input).unwrap();
        for args in corpus(&input) {
            let ours = schema::validate(&input, &args);
            assert_eq!(
                ours.is_empty(),
                reference.is_valid(&args),
                "{name}: {args} -> {ours:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 500, "only {checked} cases");
}

#[test]
fn one_of_needs_exactly_one_match() {
    let s = json!({"oneOf": [{"type": "integer", "minimum": 1}, {"type": "string", "pattern": "^rbv_"}]});
    assert!(schema::validate(&s, &json!(2)).is_empty());
    assert!(schema::validate(&s, &json!("rbv_x")).is_empty());
    assert!(!schema::validate(&s, &json!(0)).is_empty());
    assert!(!schema::validate(&s, &json!("v1")).is_empty());
    let both = json!({"oneOf": [{"type": "integer"}, {"type": "number"}]});
    assert!(!schema::validate(&both, &json!(1)).is_empty());
}

#[test]
fn lengths_count_characters_and_integers_accept_whole_floats() {
    let s = json!({"type": "string", "minLength": 2, "maxLength": 2});
    assert!(schema::validate(&s, &json!("éé")).is_empty());
    assert!(!schema::validate(&s, &json!("é")).is_empty());
    assert!(schema::validate(&json!({"type": "integer"}), &json!(1.0)).is_empty());
    assert!(!schema::validate(&json!({"type": "integer"}), &json!(1.5)).is_empty());
}
