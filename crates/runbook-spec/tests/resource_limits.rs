//! Publish-time resource limits at their exact boundaries, through the same
//! `parse` + `compile` pair publication runs, plus strict-YAML rejections
//! with their exact codes and locations.
use runbook_spec::{CompileContext, CompiledRunbook, Diagnostic, compile, parse};
use serde_json::{Value, json};

fn publish(source: &str) -> Result<CompiledRunbook, Vec<Diagnostic>> {
    parse(source).and_then(|spec| compile(&spec, &CompileContext::default()))
}
fn document(spec: Value) -> String {
    json!({
        "apiVersion": "therunbook.app/v1", "kind": "Runbook",
        "metadata": {"id": "limits", "title": "Limits"},
        "spec": spec,
    })
    .to_string()
}
/// A spec whose `inputs.items` is a list, for foreach sources.
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

// budget.rs: the worst-case step budget is exact. With the PRD §17 caps
// (R52) the largest iteration bounds publish and the count stays exact; the
// u64 overflow diagnostic is reachable only on mutated intermediate trees
// (tests/budget.rs).
// 50 iterations x (100 items + 2 steps) = 5,100.
#[test]
fn step_budget_at_the_publish_caps_is_exact() {
    let source = with_items(json!({"steps": [{"id": "outer", "loop": {
        "until": "${{ false }}", "max_iterations": 50,
        "steps": [
            {"id": "fan", "foreach": {
                "items": "${{ inputs.items }}", "max_items": 100,
                "steps": [task("each")]
            }},
            task("one"), task("two")
        ]
    }}]}));
    assert_eq!(publish(&source).unwrap().worst_case_step_runs(), 5_100);
}

// normalize.rs: max_items and max_iterations are positive and at most the
// PRD §17 caps (100 and 50, R52; tests/prd_limits.rs has the boundaries);
// foreach concurrency is any positive u32.
#[test]
fn iteration_bounds_admit_positive_values_up_to_the_caps() {
    let foreach = |key: &str, value: Value| {
        let mut group =
            json!({"items": "${{ inputs.items }}", "max_items": 1, "steps": [task("t")]});
        group[key] = value;
        with_items(json!({"steps": [{"id": "f", "foreach": group}]}))
    };
    let looped = |value: Value| {
        document(json!({"steps": [{"id": "l", "loop": {
            "until": "${{ true }}", "max_iterations": value, "steps": [task("t")]
        }}]}))
    };
    let compiled = publish(&foreach("max_items", json!(100))).unwrap();
    assert_eq!(compiled.worst_case_step_runs(), 100);
    let compiled = publish(&looped(json!(50))).unwrap();
    assert_eq!(compiled.worst_case_step_runs(), 50);
    rejected(
        &foreach("max_items", json!(u32::MAX)),
        "spec.limit_exceeded",
        "/spec/steps/0/foreach/max_items",
    );
    rejected(
        &looped(json!(u32::MAX)),
        "spec.limit_exceeded",
        "/spec/steps/0/loop/max_iterations",
    );
    assert!(publish(&foreach("concurrency", json!(u32::MAX))).is_ok());
    for bad in [
        json!(u64::from(u32::MAX) + 1),
        json!(0),
        json!(-1),
        json!(1.5),
        json!("3"),
        json!(null),
        json!(true),
    ] {
        for key in ["max_items", "concurrency"] {
            rejected(
                &foreach(key, bad.clone()),
                "spec.invalid_value",
                &format!("/spec/steps/0/foreach/{key}"),
            );
        }
        rejected(
            &looped(bad),
            "spec.invalid_value",
            "/spec/steps/0/loop/max_iterations",
        );
    }
}

// Every node with an id is a field of the `steps` expression record, whose
// schema admits 256 fields (runbook-expr limits::RECORD_FIELDS). This, not
// normalize.rs MAX_CONTROL_NODES (4,096), is the effective publish limit.
#[test]
fn publication_admits_256_identified_nodes() {
    let tasks = |n: usize| (0..n).map(|i| task(&format!("t{i}"))).collect::<Vec<_>>();
    let compiled = publish(&document(json!({"steps": tasks(256)}))).unwrap();
    assert_eq!(compiled.worst_case_step_runs(), 256);
    rejected(
        &document(json!({"steps": tasks(257)})),
        "expression.schema_limit",
        "/spec/steps/256",
    );
    // Control nodes, cleanup and nested children all count.
    // A group, not a parallel: parallel branches are capped at 20 (R52).
    let parallel = |n: usize| json!([{"id": "p", "steps": tasks(n)}]);
    assert!(publish(&document(json!({"steps": parallel(255)}))).is_ok());
    let diagnostics = publish(&document(json!({"steps": parallel(256)})))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "expression.schema_limit");
    let split = document(json!({"steps": tasks(128), "finally": (128..256)
        .map(|i| task(&format!("t{i}"))).collect::<Vec<_>>()}));
    assert!(publish(&split).is_ok());
    let sets = |n: usize| {
        (0..n)
            .map(|i| json!({"id": format!("s{i}"), "set": {"vars": {}}}))
            .collect::<Vec<_>>()
    };
    assert!(publish(&document(json!({"steps": sets(256)}))).is_ok());
    rejected(
        &document(json!({"steps": sets(257)})),
        "expression.schema_limit",
        "/spec/steps/256",
    );
}

/// Wrap `child` in the container kind `kind`; returns the node and the
/// pointer suffix from the new node to `child`.
fn wrap(kind: usize, depth: usize, child: Value) -> (Value, String) {
    let id = format!("n{depth}");
    match kind % 8 {
        0 => (json!({"id": id, "steps": [child]}), "/steps/0".into()),
        1 => (json!({"id": id, "parallel": [child]}), "/parallel/0".into()),
        2 => (
            json!({"id": id, "foreach": {
                "items": "${{ inputs.items }}", "as": format!("item{depth}"),
                "max_items": 1, "steps": [child]
            }}),
            "/foreach/steps/0".into(),
        ),
        3 => (
            json!({"id": id, "loop": {"until": "${{ true }}", "max_iterations": 1, "steps": [child]}}),
            "/loop/steps/0".into(),
        ),
        4 => (
            json!({"id": id, "switch": {"cases": [{"when": "${{ true }}", "steps": [child]}]}}),
            "/switch/cases/0/steps/0".into(),
        ),
        5 => (
            json!({"id": id, "switch": {"cases": [], "default": {"steps": [child]}}}),
            "/switch/default/steps/0".into(),
        ),
        6 => (
            json!({"id": id, "steps": [], "catch": [child]}),
            "/catch/0".into(),
        ),
        _ => (
            json!({"id": id, "steps": [], "finally": [child]}),
            "/finally/0".into(),
        ),
    }
}

/// `levels` containers chosen by `kinds` (cycled), around one task; returns
/// the source and the leaf's pointer.
fn nested(kinds: &[usize], levels: usize) -> (String, String) {
    let mut node = task("leaf");
    let mut path = String::new();
    for depth in (0..levels).rev() {
        let (wrapped, suffix) = wrap(kinds[depth % kinds.len()], depth, node);
        node = wrapped;
        path = suffix + &path;
    }
    (
        with_items(json!({"steps": [node]})),
        format!("/spec/steps/0{path}"),
    )
}

// normalize.rs MAX_CONTROL_DEPTH = 32: a task inside 32 groups, parallels,
// catch or finally blocks (in any mix) is accepted; inside 33 it is not.
#[test]
fn control_depth_admits_32_nested_containers() {
    for kinds in [&[0][..], &[1], &[6], &[7], &[0, 1, 6, 7], &[7, 6, 1, 0]] {
        let (source, _) = nested(kinds, 32);
        assert!(
            publish(&source).is_ok(),
            "{kinds:?}: {:?}",
            publish(&source).err()
        );
        let (source, leaf) = nested(kinds, 33);
        rejected(&source, "spec.control_depth_limit", &leaf);
    }
}

// Each foreach or loop level nests the `steps` expression record type, whose
// graph admits depth 16 (runbook-expr limits::TYPE_DEPTH): at most four
// iteration levels publish, whatever plain containers sit between them.
#[test]
fn iteration_nesting_is_bounded_by_the_expression_type_depth() {
    for kinds in [&[2][..], &[3], &[2, 3], &[3, 0, 2, 1]] {
        let iterations = |levels: usize| {
            (0..levels)
                .filter(|depth| matches!(kinds[depth % kinds.len()], 2 | 3))
                .count()
        };
        let mut levels = 1;
        while iterations(levels + 1) <= 4 {
            levels += 1;
        }
        let (source, _) = nested(kinds, levels);
        assert!(
            publish(&source).is_ok(),
            "{kinds:?} x{levels}: {:?}",
            publish(&source).err()
        );
        let (source, _) = nested(kinds, levels + 1);
        rejected(&source, "expression.schema_limit", "/spec");
    }
}

// parse.rs: source nesting is bounded at 128 JSON levels, which bounds
// switch nesting before the control depth: each case level costs five.
#[test]
fn switch_nesting_is_bounded_by_source_nesting() {
    for (kind, admitted) in [(4, 24), (5, 31)] {
        let (source, _) = nested(&[kind], admitted);
        assert!(publish(&source).is_ok(), "{kind}");
        let (source, _) = nested(&[kind], admitted + 1);
        let diagnostics = publish(&source).map(|_| ()).unwrap_err();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "spec.yaml_nesting_limit", "{kind}");
        assert_eq!(diagnostics[0].path.matches('/').count(), 129, "{kind}");
    }
}

// normalize.rs: node ids are 1-63 bytes of [a-z0-9_] starting with a letter.
#[test]
fn node_ids_admit_63_bytes_of_the_id_alphabet() {
    let one = |id: &str| document(json!({"steps": [task(id)]}));
    for id in ["a".repeat(63), "a_1".into(), "z9".into(), "a".into()] {
        assert!(publish(&one(&id)).is_ok(), "{id}");
    }
    for id in [
        "a".repeat(64),
        String::new(),
        "1a".into(),
        "_a".into(),
        "a-b".into(),
        "aB".into(),
        "A".into(),
        "a b".into(),
        "a.b".into(),
        "é".into(),
        "a\u{0}".into(),
    ] {
        rejected(&one(&id), "spec.invalid_id", "/spec/steps/0/id");
    }
    let nested = |id: &str| {
        document(json!({"steps": [{"id": "group", "steps": [], "finally": [task(id)]}]}))
    };
    assert!(publish(&nested(&"b".repeat(63))).is_ok());
    rejected(
        &nested(&"b".repeat(64)),
        "spec.invalid_id",
        "/spec/steps/0/finally/0/id",
    );
}

// Expression envelopes still apply at publish, located at the YAML scalar:
// tree depth 32, 4,096 expression bytes and 64 expressions per template.
#[test]
fn expression_limits_apply_at_publish_with_their_codes() {
    let at = |key: &str, value: String| {
        let mut node = task("a");
        node[key] = json!(value);
        document(json!({"steps": [node]}))
    };
    let bang = |n: usize| format!("${{{{ {}true }}}}", "!".repeat(n));
    assert!(publish(&at("if", bang(32))).is_ok());
    rejected(
        &at("if", bang(33)),
        "expression.compile_limit",
        "/spec/steps/0/if",
    );
    let padded = |n: usize| format!("${{{{{}true}}}}", " ".repeat(n));
    assert!(publish(&at("if", padded(4092))).is_ok());
    rejected(
        &at("if", padded(4093)),
        "expression.compile_limit",
        "/spec/steps/0/if",
    );
    for key in ["title", "instructions"] {
        assert!(publish(&at(key, "${{true}}".repeat(64))).is_ok(), "{key}");
        rejected(
            &at(key, "${{true}}".repeat(65)),
            "expression.compile_limit",
            &format!("/spec/steps/0/{key}"),
        );
    }
    let mut node = task("a");
    node["with"] = json!({"x": "${{true}}".repeat(65)});
    rejected(
        &document(json!({"steps": [node]})),
        "expression.compile_limit",
        "/spec/steps/0/with/x",
    );
    // Operations outside the profile are unsupported, not type errors.
    let assign = |expression: &str| {
        document(json!({"steps": [{"id": "s", "set": {"vars": {"v": expression}}}]}))
    };
    for expression in ["${{ 1 + 1 }}", "${{ 1 / 0 }}", "${{ [1].map(x, x) }}"] {
        rejected(
            &assign(expression),
            "expression.unsupported",
            "/spec/steps/0/set/vars/v",
        );
    }
}

const HEAD: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n";

#[test]
fn strict_yaml_rejections_have_exact_codes_and_locations() {
    for (body, code, path, line, column) in [
        // Tags of every spelling, including the core schema's own.
        (
            "metadata: {id: !!str a, title: A}\nspec: {steps: []}\n",
            "spec.yaml_feature_forbidden",
            "",
            3,
            16,
        ),
        (
            "metadata: {id: !<tag:yaml.org,2002:str> a, title: A}\nspec: {steps: []}\n",
            "spec.yaml_feature_forbidden",
            "",
            3,
            16,
        ),
        (
            "metadata: {id: a, title: A}\nspec: {steps: []}\nx-b: !!binary aGVsbG8=\n",
            "spec.yaml_feature_forbidden",
            "",
            5,
            6,
        ),
        // Anchors on scalars and sequences, and aliases to nothing.
        (
            "metadata: {id: a, title: &t A}\nspec: {steps: []}\n",
            "spec.yaml_feature_forbidden",
            "",
            3,
            26,
        ),
        (
            "metadata: {id: a, title: A}\nspec: {steps: &s []}\n",
            "spec.yaml_feature_forbidden",
            "",
            4,
            15,
        ),
        (
            "metadata: {id: a, title: *nothing}\nspec: {steps: []}\n",
            "spec.yaml_feature_forbidden",
            "",
            3,
            26,
        ),
        // A quoted merge key is still a merge key.
        (
            "metadata: {\"<<\": {id: a}, title: A}\nspec: {steps: []}\n",
            "spec.yaml_feature_forbidden",
            "/metadata/<<",
            3,
            12,
        ),
        // Duplicates are found after quoting and escapes are decoded.
        (
            "metadata: {id: a, \"id\": b, title: A}\nspec: {steps: []}\n",
            "spec.duplicate_key",
            "/metadata/id",
            3,
            19,
        ),
        (
            "metadata: {'title': a, id: b, title: A}\nspec: {steps: []}\n",
            "spec.duplicate_key",
            "/metadata/title",
            3,
            31,
        ),
        (
            "metadata: {id: a, \"\\x69d\": b, title: A}\nspec: {steps: []}\n",
            "spec.duplicate_key",
            "/metadata/id",
            3,
            19,
        ),
        (
            "metadata: {id: a, title: A}\nspec:\n  steps:\n    - id: s\n      title: S\n      with: {a: {b: {c: 1, c: 2}}}\n",
            "spec.duplicate_key",
            "/spec/steps/0/with/a/b/c",
            8,
            28,
        ),
        (
            "metadata: {id: a, title: A}\nspec: {steps: []}\nkind: Runbook\n",
            "spec.duplicate_key",
            "/kind",
            5,
            1,
        ),
        // Keys must be strings.
        (
            "metadata: {id: a, title: A}\nspec: {steps: []}\n? [x]\n: y\n",
            "spec.non_string_key",
            "",
            5,
            3,
        ),
        (
            "metadata: {id: a, title: A, null: 1}\nspec: {steps: []}\n",
            "spec.non_string_key",
            "/metadata",
            3,
            29,
        ),
        (
            "metadata: {id: a, title: A, true: 1}\nspec: {steps: []}\n",
            "spec.non_string_key",
            "/metadata",
            3,
            29,
        ),
        // Non-JSON floats.
        (
            "metadata: {id: a, title: A}\nspec: {steps: []}\nx-n: .nan\n",
            "spec.non_json_scalar",
            "/x-n",
            5,
            6,
        ),
        (
            "metadata: {id: a, title: A}\nspec: {steps: []}\nx-n: -.inf\n",
            "spec.non_json_scalar",
            "/x-n",
            5,
            6,
        ),
    ] {
        let source = format!("{HEAD}{body}");
        let diagnostics = parse(&source).map(|_| ()).unwrap_err();
        let got: Vec<_> = diagnostics
            .iter()
            .map(|d| (d.code.as_str(), d.path.as_str(), d.line, d.column))
            .collect();
        assert_eq!(got, vec![(code, path, line, column)], "{body}");
    }
    // A %TAG directive is rejected; an explicit %YAML 1.2 directive is not.
    let tagged = format!(
        "%TAG ! tag:example.com,2000:\n---\n{HEAD}metadata: {{id: a, title: A}}\nspec: {{steps: []}}\n"
    );
    let diagnostics = parse(&tagged).map(|_| ()).unwrap_err();
    assert_eq!(
        (
            diagnostics[0].code.as_str(),
            diagnostics[0].line,
            diagnostics[0].column
        ),
        ("spec.yaml_feature_forbidden", 1, 1)
    );
    let versioned =
        format!("%YAML 1.2\n---\n{HEAD}metadata: {{id: a, title: A}}\nspec: {{steps: []}}\n");
    assert!(publish(&versioned).is_ok());
    // A quoted numeric key is an ordinary string key.
    let quoted =
        format!("{HEAD}metadata: {{id: a, title: A}}\nspec: {{steps: []}}\nx-m: {{\"1\": 1}}\n");
    assert!(publish(&quoted).is_ok());
}

// CF13 review round 1: a runbook declares at most 256 outputs whether or
// not it has a `report` (before, only a `report` held them), so the
// engine's budget on all of a run's outputs together has a bounded number
// of entries to reserve.
#[test]
fn publication_admits_256_outputs_with_or_without_a_report() {
    let outputs = |n: usize| {
        (0..n)
            .map(|i| (format!("o{i}"), json!("${{ inputs.x }}")))
            .collect::<serde_json::Map<_, _>>()
    };
    let spec = |n: usize| {
        document(json!({
            "inputs": {"x": {"type": "string", "default": "a"}},
            "steps": [task("a")],
            "outputs": outputs(n),
        }))
    };
    assert!(publish(&spec(256)).is_ok());
    rejected(&spec(257), "expression.schema_limit", "/spec/outputs");
}
