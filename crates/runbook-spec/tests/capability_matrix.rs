//! Exhaustive key classification across every capability context.
//!
//! `capabilities.rs` keeps its context table private, so this file restates
//! it independently: for each context (reached at the top level and through
//! every nesting construct) it lists the keys that are supported, preserved
//! or rejected, and every other key of a broad candidate universe (all keys
//! the table names anywhere, plus plausible workflow keywords) must be
//! `spec.unknown_key`. A key that becomes accepted, or a rejection whose code
//! changes, fails here. Every rejected key is then published through
//! `parse` + `compile` and must yield exactly that code at exactly its own
//! pointer, so no reserved key can be silently ignored.
use runbook_spec::{CompileContext, Support, classify, compile, parse};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Expect {
    Supported,
    Preserved,
    Rejected(&'static str),
}
use Expect::*;

const UNSUPPORTED: Expect = Rejected("spec.unsupported_feature");

#[derive(Clone, Copy, Debug)]
enum Kind {
    Root,
    Metadata,
    Body,
    Execution,
    Concurrency,
    Declarations,
    Declaration { field: bool },
    Defaults,
    Retry,
    Agents,
    Nodes,
    Node,
    Instructions,
    Evidence,
    EvidenceKinds,
    Agent,
    Foreach,
    Loop,
    Switch,
    Indexed,
    Case,
    Branch,
    Wait,
    Approval,
    Option,
    Approvers,
    TimeoutPolicy,
    Set,
    End,
    Call,
}

const NODE_KEYS: &[&str] = &[
    "id",
    "title",
    "if",
    "actor",
    "assignee",
    "check",
    "skippable",
    "timeout",
    "on_failure",
    "destructive",
    "allow_auto_decision",
    "instructions",
    "skill",
    "skills",
    "labels",
    "catch_on",
    "remind_after",
    "remind_every",
    "escalate_after",
    "escalate_to",
    "with",
    "checklist",
    "outputs",
    "evidence",
    "agent",
    "steps",
    "catch",
    "finally",
    "parallel",
    "join",
    "fail_fast",
    "foreach",
    "loop",
    "switch",
    "wait",
    "approval",
    "set",
    "end",
    "call",
    "retry",
];

fn supported(kind: Kind) -> &'static [&'static str] {
    use Kind::*;
    match kind {
        Root => &["apiVersion", "kind", "metadata", "spec"],
        Metadata => &["id", "title", "description", "tags", "owners"],
        Body => &[
            "execution",
            "inputs",
            "fields",
            "vars",
            "defaults",
            "skills",
            "agents",
            "steps",
            "finally",
            "outputs",
            "report",
            "triggers",
        ],
        Execution => &["title", "tags", "timeout", "cleanup_timeout", "concurrency"],
        Concurrency => &["key", "limit", "on_conflict"],
        Declarations => &[],
        Declaration { field: false } => &[
            "type",
            "title",
            "description",
            "required",
            "min",
            "max",
            "default",
            "options",
            "items",
            "properties",
            "pattern",
        ],
        Declaration { field: true } => &[
            "type",
            "title",
            "description",
            "required",
            "min",
            "max",
            "default",
            "options",
            "items",
            "properties",
            "indexed",
            "locked",
            "pattern",
        ],
        Defaults => &[
            "actor",
            "timeout",
            "on_failure",
            "retry",
            "remind_after",
            "remind_every",
            "escalate_after",
            "escalate_to",
        ],
        Retry => &["max_attempts", "backoff", "on"],
        Agents => &["params"],
        Nodes | EvidenceKinds | Indexed => &[],
        Node => NODE_KEYS,
        Instructions => &["default", "human", "agent"],
        Evidence => &["required", "accepted"],
        Agent => &[],
        Foreach => &["items", "as", "key", "concurrency", "max_items", "steps"],
        Loop => &["until", "while", "max_iterations", "delay", "steps"],
        Switch => &["cases", "default"],
        Case => &["when", "steps"],
        Branch => &["steps"],
        Wait => &["for", "until", "signal", "timeout", "outputs"],
        Approval => &[
            "prompt",
            "min",
            "timeout",
            "escalation_timeout",
            "on_expired",
            "allow_agents",
            "escalate_to",
            "options",
            "approvers",
            "on_timeout",
        ],
        Option => &["id", "title", "auto"],
        Approvers => &["users", "teams"],
        TimeoutPolicy => &["decide"],
        Set => &["vars", "fields"],
        End => &["status", "message"],
        Call => &["runbook", "version", "with", "outputs"],
    }
}

/// Keys this context names with a non-supported outcome.
fn special(kind: Kind) -> &'static [(&'static str, Expect)] {
    use Kind::*;
    match kind {
        Agents => &[("dispatch", Rejected("spec.agents_dispatch_removed"))],
        Agent => &[
            ("tools", Preserved),
            ("commands", Preserved),
            ("hints", Supported),
            ("runner", UNSUPPORTED),
            ("model", UNSUPPORTED),
            ("max_cost_usd", UNSUPPORTED),
            ("max_tokens", UNSUPPORTED),
        ],
        _ => &[],
    }
}

fn expected(kind: Kind, key: &str) -> Expect {
    use Kind::*;
    if key.starts_with("x-") {
        return Preserved;
    }
    let index = key == "*" || key.parse::<usize>().is_ok();
    if matches!(kind, Declarations) || (index && matches!(kind, Nodes | EvidenceKinds | Indexed)) {
        return Supported;
    }
    if let Some((_, expect)) = special(kind).iter().find(|(k, _)| *k == key) {
        return *expect;
    }
    if supported(kind).contains(&key) {
        Supported
    } else {
        Rejected("spec.unknown_key")
    }
}

/// A value that passes this key's own value check (most keys have none).
fn good_value(kind: Kind, key: &str) -> Value {
    use Kind::*;
    match (kind, key) {
        (Root, "apiVersion") => json!("therunbook.app/v1"),
        (Root, "kind") => json!("Runbook"),
        (Declaration { .. }, "type") => json!("string"),
        (Declaration { .. }, "pattern") => json!(".*"),
        (Concurrency, "on_conflict") => json!("queue"),
        (Node, "join") => json!("all"),
        (Node, "skills") => json!(["sample"]),
        (Node, "labels") => json!({}),
        (Body, "triggers") => json!({}),
        (Node, "catch_on") => json!(["timeout"]),
        (Retry, "on") => json!(["timeout"]),
        (Agents, "params") => json!({}),
        (EvidenceKinds, _) => json!("link"),
        _ => Value::Null,
    }
}

const EXTRA_KEYS: &[&str] = &[
    "magic",
    "uses",
    "run",
    "env",
    "needs",
    "matrix",
    "shell",
    "secrets",
    "services",
    "container",
    "depends_on",
    "next",
    "goto",
    "schedule",
    "cron",
    "webhook",
    "script",
    "command",
    "exec",
    "eval",
    "import",
    "include",
    "extends",
    "template",
    "workflow",
    "on",
    "name",
    "version",
    "labels",
    "annotations",
    "any",
    "at_least",
    "params_schema",
    "timeout_minutes",
    "max_parallel",
    "max_attempts_total",
    "rollback",
    "compensate",
    "signal_schema",
    "ID",
    "Steps",
    "TITLE",
    "X-extension",
    "x",
    "_",
    "",
    " ",
    "id ",
    "a/b",
    "~x",
    "é",
    "0",
    "17",
    "*",
    "x-extension",
];

fn universe() -> BTreeSet<&'static str> {
    use Kind::*;
    let kinds = [
        Root,
        Metadata,
        Body,
        Execution,
        Concurrency,
        Declaration { field: true },
        Defaults,
        Retry,
        Agents,
        Node,
        Instructions,
        Evidence,
        Agent,
        Foreach,
        Loop,
        Switch,
        Case,
        Branch,
        Wait,
        Approval,
        Option,
        Approvers,
        TimeoutPolicy,
        Set,
        End,
        Call,
    ];
    let mut keys: BTreeSet<&str> = EXTRA_KEYS.iter().copied().collect();
    for kind in kinds {
        keys.extend(supported(kind));
        keys.extend(special(kind).iter().map(|(k, _)| *k));
    }
    keys
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Every context, reached at the top level and inside every construct that
/// opens a node list (including deep mixed nesting).
fn contexts() -> Vec<(String, Kind)> {
    use Kind::*;
    let mut out: Vec<(String, Kind)> = vec![
        (String::new(), Root),
        ("/metadata".into(), Metadata),
        ("/spec".into(), Body),
        ("/spec/execution".into(), Execution),
        ("/spec/execution/concurrency".into(), Concurrency),
        ("/spec/defaults".into(), Defaults),
        ("/spec/defaults/retry".into(), Retry),
        ("/spec/agents".into(), Agents),
        ("/spec/steps".into(), Nodes),
        ("/spec/finally".into(), Nodes),
    ];
    for (root, field) in [("/spec/inputs", false), ("/spec/fields", true)] {
        out.push((root.into(), Declarations));
        out.push((format!("{root}/a"), Declaration { field }));
        out.push((format!("{root}/a/items"), Declaration { field: false }));
        out.push((format!("{root}/a/properties"), Declarations));
        out.push((
            format!("{root}/a/properties/b"),
            Declaration { field: false },
        ));
        out.push((
            format!("{root}/a/items/properties/b/items"),
            Declaration { field: false },
        ));
    }
    let nodes = [
        "/spec/steps/0",
        "/spec/finally/0",
        "/spec/steps/0/steps/0",
        "/spec/steps/0/catch/0",
        "/spec/steps/0/finally/0",
        "/spec/steps/0/parallel/0",
        "/spec/steps/0/foreach/steps/0",
        "/spec/steps/0/loop/steps/0",
        "/spec/steps/0/switch/cases/0/steps/0",
        "/spec/steps/0/switch/default/steps/0",
        "/spec/finally/3/loop/steps/2/foreach/steps/1/parallel/4/switch/cases/7/steps/0",
    ];
    for node in nodes {
        for (suffix, kind) in [
            ("", Node),
            ("/instructions", Instructions),
            ("/evidence", Evidence),
            ("/evidence/required", EvidenceKinds),
            ("/evidence/accepted", EvidenceKinds),
            ("/agent", Agent),
            ("/retry", Retry),
            ("/outputs", Declarations),
            ("/outputs/o", Declaration { field: false }),
            ("/outputs/o/items", Declaration { field: false }),
            ("/steps", Nodes),
            ("/catch", Nodes),
            ("/finally", Nodes),
            ("/parallel", Nodes),
            ("/foreach", Foreach),
            ("/foreach/steps", Nodes),
            ("/loop", Loop),
            ("/loop/steps", Nodes),
            ("/switch", Switch),
            ("/switch/cases", Indexed),
            ("/switch/cases/0", Case),
            ("/switch/cases/0/steps", Nodes),
            ("/switch/default", Branch),
            ("/switch/default/steps", Nodes),
            ("/wait", Wait),
            ("/approval", Approval),
            ("/approval/options", Indexed),
            ("/approval/options/0", Option),
            ("/approval/approvers", Approvers),
            ("/approval/on_timeout", TimeoutPolicy),
            ("/set", Set),
            ("/end", End),
            ("/call", Call),
            ("/call/outputs", Declarations),
            ("/call/outputs/o", Declaration { field: false }),
        ] {
            out.push((format!("{node}{suffix}"), kind));
        }
    }
    out
}

fn actual(path: &str, value: &Value) -> Expect {
    match classify(path, value) {
        Support::Supported { .. } => Supported,
        Support::Preserved { .. } => Preserved,
        Support::Rejected { code } => Rejected(Box::leak(code.into_boxed_str())),
    }
}

#[test]
fn every_context_classifies_the_whole_key_universe_exactly() {
    let universe = universe();
    assert!(universe.len() > 150, "{}", universe.len());
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for (parent, kind) in contexts() {
        for key in &universe {
            let path = format!("{parent}/{}", escape(key));
            let value = good_value(kind, key);
            let want = expected(kind, key);
            let got = actual(&path, &value);
            checked += 1;
            if got != want {
                mismatches.push(format!("{path}: expected {want:?}, got {got:?}"));
            }
        }
    }
    assert!(checked > 40_000, "{checked}");
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(40)].join("\n")
    );
}

#[test]
fn opaque_subtrees_never_classify_keywords() {
    for (path, value, want) in [
        ("/spec/vars/v/call", json!(1), Supported),
        ("/spec/vars/v/steps/0/loop/while", json!(1), Supported),
        ("/spec/outputs/o/triggers", json!(1), Supported),
        ("/spec/skills/s/dispatch", json!(1), Supported),
        ("/spec/steps/0/with/runner", json!(1), Supported),
        ("/spec/steps/0/checklist/0/call", json!(1), Supported),
        ("/spec/steps/0/set/vars/pattern", json!(1), Supported),
        (
            "/spec/steps/0/approval/escalate_to/0/call",
            json!(1),
            Supported,
        ),
        (
            "/spec/steps/0/approval/approvers/users/0/x",
            json!(1),
            Supported,
        ),
        ("/spec/inputs/a/default/call", json!(1), Supported),
        ("/spec/inputs/a/options/0/pattern", json!(1), Supported),
        ("/metadata/tags/0/call", json!(1), Supported),
        ("/spec/execution/timeout/call", json!(1), Supported),
        ("/spec/agents/params/dispatch", json!({}), Supported),
        ("/spec/agents/params/call/while", json!(1), Supported),
        ("/spec/steps/0/agent/tools/runner", json!(1), Preserved),
        ("/spec/steps/0/agent/commands/0/call", json!(1), Preserved),
        ("/x-a/call", json!(1), Preserved),
        ("/spec/steps/0/x-a/loop/while", json!(1), Preserved),
        ("/spec/steps/0/foreach/x-a/call", json!(1), Preserved),
    ] {
        assert_eq!(actual(path, &value), want, "{path}");
    }
}

#[test]
fn value_checked_keys_reject_every_other_value_with_their_code() {
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "/apiVersion",
            json!("therunbook.app/v2"),
            "spec.unsupported_api_version",
        ),
        (
            "/apiVersion",
            json!("therunbook.app/V1"),
            "spec.unsupported_api_version",
        ),
        ("/apiVersion", json!(null), "spec.unsupported_api_version"),
        ("/apiVersion", json!(1), "spec.unsupported_api_version"),
        ("/kind", json!("StepLibrary"), "spec.unsupported_kind"),
        ("/kind", json!("runbook"), "spec.unsupported_kind"),
        ("/kind", json!(["Runbook"]), "spec.unsupported_kind"),
        (
            "/spec/inputs/a/type",
            json!("String"),
            "spec.unsupported_type",
        ),
        ("/spec/inputs/a/type", json!(""), "spec.unsupported_type"),
        ("/spec/inputs/a/type", json!(null), "spec.unsupported_type"),
        (
            "/spec/inputs/a/type",
            json!(["string"]),
            "spec.unsupported_type",
        ),
        (
            "/spec/fields/a/type",
            json!("secret_ref"),
            "spec.unsupported_type",
        ),
        (
            "/spec/steps/0/outputs/o/type",
            json!("file"),
            "spec.unsupported_type",
        ),
        (
            "/spec/steps/0/outputs/o/items/type",
            json!("map"),
            "spec.unsupported_type",
        ),
        (
            "/spec/execution/concurrency/on_conflict",
            json!("cancel"),
            "spec.unsupported_value",
        ),
        (
            "/spec/execution/concurrency/on_conflict",
            json!("Queue"),
            "spec.unsupported_value",
        ),
        (
            "/spec/execution/concurrency/on_conflict",
            json!(null),
            "spec.unsupported_value",
        ),
        ("/spec/steps/0/join", json!("ALL"), "spec.unsupported_value"),
        ("/spec/steps/0/join", json!(2), "spec.unsupported_value"),
        (
            "/spec/finally/0/steps/0/join",
            json!("some"),
            "spec.unsupported_value",
        ),
        ("/spec/steps/0/join/at_most", json!(1), "spec.unknown_key"),
        (
            "/spec/steps/0/evidence/required/0",
            json!("video"),
            "spec.unsupported_value",
        ),
        (
            "/spec/steps/0/evidence/accepted/0",
            json!("LINK"),
            "spec.unsupported_value",
        ),
        (
            "/spec/steps/0/evidence/accepted/0",
            json!(null),
            "spec.unsupported_value",
        ),
        ("/spec/agents/params", json!([]), "spec.type_error"),
        ("/spec/agents/params", json!("x"), "spec.type_error"),
        ("/spec/agents/params", json!(null), "spec.type_error"),
    ];
    for (path, value, code) in &cases {
        assert_eq!(actual(path, value), Rejected(code), "{path} = {value}");
    }
    for (path, value) in [
        ("/spec/inputs/a/type", "string"),
        ("/spec/inputs/a/type", "text"),
        ("/spec/inputs/a/type", "number"),
        ("/spec/inputs/a/type", "integer"),
        ("/spec/inputs/a/type", "boolean"),
        ("/spec/inputs/a/type", "enum"),
        ("/spec/inputs/a/type", "list"),
        ("/spec/inputs/a/type", "object"),
        ("/spec/steps/0/evidence/required/0", "log"),
        ("/spec/steps/0/evidence/required/0", "link"),
        ("/spec/steps/0/evidence/required/0", "file"),
        ("/spec/steps/0/evidence/accepted/0", "screenshot"),
    ] {
        assert_eq!(actual(path, &json!(value)), Supported, "{path} = {value}");
    }
    // LANG2: `any` and `{at_least: n}` joins; LANG3: conflict policies.
    for (path, value) in [
        ("/spec/execution/concurrency/on_conflict", json!("reject")),
        (
            "/spec/execution/concurrency/on_conflict",
            json!("cancel_previous"),
        ),
        ("/spec/steps/0/join", json!("any")),
        ("/spec/finally/0/steps/0/join", json!("any")),
        ("/spec/steps/0/join", json!({"at_least": 1})),
        ("/spec/steps/0/join/at_least", json!(2)),
    ] {
        assert_eq!(actual(path, &value), Supported, "{path} = {value}");
    }
    // Published, each rejection is the only diagnostic, at its own pointer.
    for (path, value, code) in cases {
        if path.starts_with("/apiVersion") || path.starts_with("/kind") {
            continue; // the source envelope itself is checked before capabilities
        }
        assert_eq!(
            publish_with(path, value.clone()),
            vec![(path.to_owned(), code.to_owned())]
        );
    }
}

// capabilities.rs: spec.agents.params must fit 4,096 bytes of compact JSON.
#[test]
fn agent_params_admit_exactly_4096_bytes() {
    let params = |bytes: usize| json!({"k": "x".repeat(bytes - 8)});
    assert_eq!(serde_json::to_string(&params(4096)).unwrap().len(), 4096);
    assert_eq!(actual("/spec/agents/params", &params(4096)), Supported);
    assert_eq!(
        actual("/spec/agents/params", &params(4097)),
        Rejected("spec.agents_params_too_large")
    );
    // Escapes count at their encoded size.
    let escaped = json!({"k": "\"".repeat(2044)});
    assert_eq!(serde_json::to_string(&escaped).unwrap().len(), 4096);
    assert_eq!(actual("/spec/agents/params", &escaped), Supported);
    let escaped = json!({"k": "\n".repeat(2045)});
    assert_eq!(
        actual("/spec/agents/params", &escaped),
        Rejected("spec.agents_params_too_large")
    );
    assert!(publish_with("/spec/agents/params", params(4096)).is_empty());
    assert_eq!(
        publish_with("/spec/agents/params", params(4097)),
        vec![(
            "/spec/agents/params".to_owned(),
            "spec.agents_params_too_large".to_owned()
        )]
    );
}

const BASE: &str = r#"{"apiVersion":"therunbook.app/v1","kind":"Runbook",
  "metadata":{"id":"probe","title":"Probe"},
  "spec":{"steps":[{"id":"a","title":"A"}]}}"#;

/// Set `value` at RFC 6901 `pointer` in the base document, creating arrays
/// for numeric segments and objects otherwise.
fn place(pointer: &str, value: Value) -> Value {
    let mut root: Value = serde_json::from_str(BASE).unwrap();
    let parts: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|p| p.replace("~1", "/").replace("~0", "~"))
        .collect();
    let mut node = &mut root;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        // Only these keys hold source-language lists; a numeric key under
        // any other map is just a (string) map key.
        let next_index = matches!(
            part.as_str(),
            "steps"
                | "finally"
                | "catch"
                | "parallel"
                | "cases"
                | "options"
                | "required"
                | "accepted"
        ) && parts.get(i + 1).is_some_and(|p| p.parse::<usize>().is_ok());
        let fresh = || {
            if last {
                Value::Null
            } else if next_index {
                Value::Array(vec![])
            } else {
                Value::Object(Map::new())
            }
        };
        if !(node.is_object() || node.is_array()) {
            *node = Value::Object(Map::new());
        }
        node = match node {
            Value::Array(items) => {
                let index: usize = part.parse().unwrap();
                while items.len() <= index {
                    items.push(Value::Object(Map::new()));
                }
                &mut items[index]
            }
            Value::Object(map) => {
                let entry = map.entry(part.clone()).or_insert_with(fresh);
                if !last && !(entry.is_object() || entry.is_array()) {
                    *entry = fresh();
                }
                entry
            }
            _ => unreachable!(),
        };
    }
    *node = value;
    name_nodes(&mut root, false, &mut 0);
    root
}

/// The source model requires a string id on every node: give each created
/// node in a node list a unique one.
fn name_nodes(value: &mut Value, in_list: bool, next: &mut usize) {
    match value {
        Value::Array(items) => {
            for item in items {
                name_nodes(item, in_list, next);
            }
        }
        Value::Object(map) => {
            if in_list && !map.contains_key("id") {
                *next += 1;
                map.insert("id".into(), json!(format!("node{next}")));
            }
            for (key, child) in map.iter_mut() {
                let list = matches!(key.as_str(), "steps" | "finally" | "catch" | "parallel");
                if !key.starts_with("x-") {
                    name_nodes(child, list, next);
                }
            }
        }
        _ => {}
    }
}

/// Publish the base document with `value` at `pointer`; returns every
/// diagnostic as (path, code).
fn publish_with(pointer: &str, value: Value) -> Vec<(String, String)> {
    let source = serde_json::to_string(&place(pointer, value)).unwrap();
    match parse(&source).and_then(|spec| compile(&spec, &CompileContext::default())) {
        Ok(_) => vec![],
        Err(diagnostics) => diagnostics.into_iter().map(|d| (d.path, d.code)).collect(),
    }
}

#[test]
fn every_rejected_key_in_every_context_fails_publication_at_its_pointer() {
    let universe = universe();
    let mut failures = Vec::new();
    let mut published = 0;
    let mut codes = BTreeSet::new();
    for (parent, kind) in contexts() {
        for key in &universe {
            let Rejected(code) = expected(kind, key) else {
                continue;
            };
            if matches!(kind, Kind::Nodes | Kind::EvidenceKinds | Kind::Indexed) {
                continue; // a JSON array cannot hold a named key
            }
            // Unknown keys are published under three representative node
            // nestings (classification above covers all of them); every
            // reserved key is published under every nesting.
            let sampled = !parent.starts_with("/spec/")
                || !(parent.starts_with("/spec/steps/0/")
                    || parent.starts_with("/spec/finally/3/"))
                || parent.starts_with("/spec/steps/0/foreach/steps/0");
            if code == "spec.unknown_key" && !sampled {
                continue;
            }
            let path = format!("{parent}/{}", escape(key));
            let value = match *key {
                "triggers" | "call" | "dispatch" => json!({"probe": true}),
                _ => json!(true),
            };
            let diagnostics = publish_with(&path, value);
            published += 1;
            codes.insert(code);
            if diagnostics != [(path.clone(), code.to_owned())] {
                failures.push(format!("{path}: expected only {code}, got {diagnostics:?}"));
            }
        }
    }
    assert!(published > 8_000, "{published}");
    assert_eq!(
        codes,
        BTreeSet::from([
            "spec.agents_dispatch_removed",
            "spec.unknown_key",
            "spec.unsupported_feature"
        ])
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures[..failures.len().min(40)].join("\n")
    );
}
