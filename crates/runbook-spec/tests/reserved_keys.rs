//! D2: no unfinished reserved field is silently executable.
//!
//! Every `rejected` rule in `conformance/spec/pilot-capabilities.json` is
//! placed into an otherwise valid runbook and sent through the same
//! `parse` + `compile` pair that publication runs
//! (`runbook_core::publish::prepare`). Each must fail with a diagnostic at
//! its own source pointer (or at the rejected container that holds it), so a
//! reserved key can never be published and then ignored at run time.
//! `preserved` rules (extensions, advisory agent context) must not be
//! reported by the capability pass: they are kept verbatim, never executed.
use runbook_spec::{CompileContext, Support, classify, compile, parse};
use serde_json::{Map, Value, json};

const BASE: &str = r#"{
  "apiVersion": "therunbook.app/v1",
  "kind": "Runbook",
  "metadata": {"id": "probe", "title": "Probe"},
  "spec": {"steps": [{"id": "a", "title": "A", "instructions": "Do it."}]}
}"#;

/// Keys whose values are lists in the source language; every other
/// container created on the way to a rule is a map.
const LIST_KEYS: &[&str] = &[
    "steps",
    "finally",
    "catch",
    "parallel",
    "cases",
    "options",
    "required",
    "accepted",
    "escalate_to",
    "checklist",
];

fn rules(class: &str) -> Vec<(String, Value)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/spec/pilot-capabilities.json");
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    manifest["rules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["class"] == class)
        .map(|r| (r["path"].as_str().unwrap().to_owned(), r["value"].clone()))
        .collect()
}

/// Place `value` at the dotted rule `path` inside `root`, creating
/// containers as needed. `*` selects the first array element or a map
/// entry named `probe`. Returns the concrete RFC 6901 pointer.
fn place(root: &mut Value, path: &str, value: Value) -> String {
    let parts: Vec<&str> = path.split('.').collect();
    let mut pointer = String::new();
    let mut node = root;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let next_is_index = parts
            .get(i + 1)
            .is_some_and(|n| *n == "*" || n.parse::<usize>().is_ok());
        let fresh = || {
            if last {
                Value::Null
            } else if next_is_index && LIST_KEYS.contains(part) {
                Value::Array(vec![])
            } else {
                Value::Object(Map::new())
            }
        };
        node = match node {
            Value::Array(items) => {
                let index = if *part == "*" {
                    0
                } else {
                    part.parse().unwrap()
                };
                while items.len() <= index {
                    items.push(Value::Object(Map::new()));
                }
                pointer.push_str(&format!("/{index}"));
                &mut items[index]
            }
            Value::Object(map) => {
                let key = if *part == "*" { "probe" } else { part };
                pointer.push_str(&format!("/{}", key.replace('~', "~0").replace('/', "~1")));
                let created = fresh();
                let entry = map.entry(key.to_owned()).or_insert(created);
                // A rule under a key the base already holds as a scalar.
                if !last && !(entry.is_object() || entry.is_array()) {
                    *entry = fresh();
                }
                entry
            }
            other => panic!("cannot place {path}: {other} is not a container"),
        };
    }
    *node = value;
    pointer
}

fn compile_json(source: &Value) -> Result<(), Vec<runbook_spec::Diagnostic>> {
    let text = serde_json::to_string_pretty(source).unwrap();
    parse(&text)
        .and_then(|spec| compile(&spec, &CompileContext::default()))
        .map(|_| ())
}

#[test]
fn the_probe_runbook_compiles() {
    let base: Value = serde_json::from_str(BASE).unwrap();
    compile_json(&base).expect("the base probe must be valid");
}

#[test]
fn every_rejected_pilot_key_fails_compilation_at_its_source_location() {
    let rejected = rules("rejected");
    // LANG1–LANG5 moved rules to supported (plan §2.9); LANG9b, triggers.
    assert!(rejected.len() >= 12, "the profile lists the reserved keys");
    for (path, value) in rejected {
        let mut source: Value = serde_json::from_str(BASE).unwrap();
        let pointer = place(&mut source, &path, value.clone());
        let diagnostics = match compile_json(&source) {
            Ok(()) => panic!("{path} = {value} compiled: a reserved key would be ignored"),
            Err(diagnostics) => diagnostics,
        };
        let hit = diagnostics
            .iter()
            .find(|d| is_rejection_of(&source, &pointer, d))
            .unwrap_or_else(|| {
                panic!("{path} = {value}: no rejection at {pointer}: {diagnostics:?}")
            });
        if hit.path == pointer {
            // At its own location the diagnostic is the classifier's code.
            assert_eq!(
                Some(hit.code.as_str()),
                classify(&pointer, &value).code(),
                "{path}"
            );
        }
    }
}

/// A diagnostic rejects the key at `pointer` when it is a capability
/// rejection of that key itself, or of a container holding it: the
/// classifier must call that container (with its source value) rejected.
/// Any other diagnostic, such as a structural error on the enclosing step,
/// does not count: the key might still be silently accepted.
fn is_rejection_of(source: &Value, pointer: &str, d: &runbook_spec::Diagnostic) -> bool {
    let at_or_above = d.path == pointer
        || (pointer.starts_with(&d.path) && pointer[d.path.len()..].starts_with('/'));
    if !at_or_above || !d.code.starts_with("spec.") {
        return false;
    }
    let Some(value) = source.pointer(&d.path) else {
        return false;
    };
    matches!(classify(&d.path, value), Support::Rejected { .. })
        && classify(&d.path, value).code() == Some(d.code.as_str())
}

#[test]
fn a_parent_diagnostic_counts_only_when_the_parent_is_rejected() {
    let diagnostic = |path: &str, code: &str| runbook_spec::Diagnostic {
        message: None,
        code: code.into(),
        path: path.into(),
        line: 1,
        column: 1,
    };
    // `agent: {runner: {pool: …}}` is reported once, on the rejected `runner`.
    let mut source: Value = serde_json::from_str(BASE).unwrap();
    let pointer = place(
        &mut source,
        "spec.steps.*.agent.runner.pool",
        json!("daily"),
    );
    assert!(is_rejection_of(
        &source,
        &pointer,
        &diagnostic("/spec/steps/0/agent/runner", "spec.unsupported_feature")
    ));
    // A deliberate break: an unrelated error on the supported step above
    // the key (or on the document) must not stand in for its rejection.
    let mut source: Value = serde_json::from_str(BASE).unwrap();
    let pointer = place(&mut source, "spec.steps.*.agent.runner", json!(true));
    for (path, code) in [
        ("/spec/steps/0", "spec.invalid_node_field"),
        ("/spec/steps/0", "spec.unsupported_feature"),
        ("", "spec.unknown_key"),
    ] {
        assert!(
            !is_rejection_of(&source, &pointer, &diagnostic(path, code)),
            "{path} {code}"
        );
    }
    // A diagnostic beside the key, not above it, never counts.
    assert!(!is_rejection_of(
        &source,
        &pointer,
        &diagnostic("/spec/steps/0/agent/runne", "spec.unknown_key")
    ));
}

#[test]
fn preserved_pilot_keys_are_never_reported_as_capabilities() {
    let preserved = rules("preserved");
    assert!(!preserved.is_empty());
    for (path, value) in preserved {
        let mut source: Value = serde_json::from_str(BASE).unwrap();
        let pointer = place(&mut source, &path, value.clone());
        let spec = parse(&serde_json::to_string_pretty(&source).unwrap())
            .unwrap_or_else(|d| panic!("{path}: {d:?}"));
        let reported: Vec<_> = runbook_spec::validate_capabilities(&spec)
            .into_iter()
            .filter(|d| pointer.starts_with(&d.path))
            .collect();
        assert!(reported.is_empty(), "{path}: {reported:?}");
        assert!(
            matches!(classify(&pointer, &json!(value)), Support::Preserved { .. }),
            "{path}"
        );
    }
}

/// UP1 moved uploaded evidence (`file`, `screenshot`) from rejected to
/// supported (later-plans rule 1): both compile in `required` and
/// `accepted`, and a kind nobody implements is still refused at its pointer.
#[test]
fn uploaded_evidence_kinds_compile_and_unknown_kinds_do_not() {
    for list in ["required", "accepted"] {
        for kind in ["file", "screenshot"] {
            let mut source: Value = serde_json::from_str(BASE).unwrap();
            place(
                &mut source,
                &format!("spec.steps.*.evidence.{list}.0"),
                json!(kind),
            );
            compile_json(&source).unwrap_or_else(|d| panic!("{list} {kind}: {d:?}"));
        }
        let mut source: Value = serde_json::from_str(BASE).unwrap();
        let pointer = place(
            &mut source,
            &format!("spec.steps.*.evidence.{list}.0"),
            json!("video"),
        );
        let diagnostics = compile_json(&source).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|d| d.path == pointer && d.code == "spec.unsupported_value"),
            "{diagnostics:?}"
        );
    }
}
