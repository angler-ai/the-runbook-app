//! LANG7: fewer required fields (derived titles, default `apiVersion` and
//! `kind`) and the opt-in strict assignment (`assignment: strict`).
use runbook_spec::ir::{AssignmentMode, NodeKind};
use runbook_spec::{
    CompileContext, Support, classify, compile, derived_title, normalize, parse, restore,
};
use serde_json::{Value, json};

fn errors(source: &str) -> Vec<(String, String)> {
    match parse(source) {
        Err(errors) => errors,
        Ok(spec) => normalize(&spec).expect_err("must not normalize"),
    }
    .into_iter()
    .map(|d| (d.code, d.path))
    .collect()
}

fn document(body: Value) -> String {
    json!({"apiVersion":"therunbook.app/v1","kind":"Runbook",
        "metadata":{"id":"test","title":"Test"},"spec":body})
    .to_string()
}

#[test]
fn a_title_is_derived_from_an_id() {
    for (id, title) in [
        ("check-health", "Check health"),
        ("check_health", "Check health"),
        ("deploy", "Deploy"),
        ("a__b--c", "A b c"),
        ("v2-rollout", "V2 rollout"),
        ("x", "X"),
    ] {
        assert_eq!(derived_title(id), title, "{id}");
    }
}

#[test]
fn a_minimal_runbook_needs_only_ids() {
    let source = "metadata: {id: check-health}\nspec:\n  steps:\n    - id: ping_api\n    - id: verify\n      title: Verify it\n";
    let spec = parse(source).unwrap();
    assert_eq!(spec.api_version, "therunbook.app/v1");
    assert_eq!(spec.kind, "Runbook");
    assert_eq!(spec.metadata.title, "Check health");
    let compiled = compile(&spec, &CompileContext::default()).unwrap();
    let ir = compiled.ir();
    assert_eq!(ir.metadata.title, "Check health");
    assert_eq!(ir.steps[0].title.as_deref(), Some("Ping api"));
    assert_eq!(ir.steps[1].title.as_deref(), Some("Verify it"));
    // The derived title is part of the stored definition: it survives a
    // restore and is the same on every compile.
    let restored = restore(compiled.snapshot_bytes()).unwrap();
    assert_eq!(restored.ir(), ir);
    let again = compile(&parse(source).unwrap(), &CompileContext::default()).unwrap();
    assert_eq!(again.hash(), compiled.hash());
}

#[test]
fn control_nodes_keep_their_optional_titles() {
    let spec = parse(&document(json!({"steps":[
        {"id":"group","steps":[{"id":"inner"}]}
    ]})))
    .unwrap();
    let ir = normalize(&spec).unwrap();
    assert_eq!(ir.steps[0].title, None);
    let NodeKind::Group(group) = &ir.steps[0].kind else {
        panic!("a group")
    };
    assert_eq!(group.children[0].title.as_deref(), Some("Inner"));
}

#[test]
fn ids_stay_required_and_explicit_versions_are_still_checked() {
    assert_eq!(
        errors("metadata: {title: T}\nspec: {steps: []}\n"),
        [("spec.source_shape".into(), "/metadata/id".into())]
    );
    assert_eq!(
        errors("metadata: {id: t}\nspec: {steps: [{title: T}]}\n"),
        [("spec.source_shape".into(), "/spec/steps/0/id".into())]
    );
    assert_eq!(
        errors("apiVersion: therunbook.app/v2\nmetadata: {id: t}\nspec: {steps: []}\n"),
        [("spec.unsupported_api_version".into(), "/apiVersion".into())]
    );
    assert_eq!(
        errors("kind: Playbook\nmetadata: {id: t}\nspec: {steps: []}\n"),
        [("spec.unsupported_kind".into(), "/kind".into())]
    );
    assert_eq!(
        errors("apiVersion: 1\nmetadata: {id: t}\nspec: {steps: []}\n"),
        [("spec.source_shape".into(), "/apiVersion".into())]
    );
    assert_eq!(
        errors("metadata: {id: t, title: 3}\nspec: {steps: []}\n"),
        [("spec.source_shape".into(), "/metadata/title".into())]
    );
}

#[test]
fn strict_assignment_is_an_opt_in_on_tasks_with_an_assignee() {
    let spec = parse(&document(json!({"steps":[
        {"id":"page","title":"Page","assignee":"team:oncall","assignment":"strict"},
        {"id":"note","title":"Note","assignee":"team:oncall","assignment":"guidance"},
        {"id":"free","title":"Free","assignee":"team:oncall"},
        {"id":"owner","title":"Owner","assignee":"${{ inputs.owner }}","assignment":"strict"}
    ],"inputs":{"owner":{"type":"string","required":true}}})))
    .unwrap();
    let ir = normalize(&spec).unwrap();
    let mode = |i: usize| match &ir.steps[i].kind {
        NodeKind::Task(task) => task.assignment,
        _ => panic!("a task"),
    };
    assert_eq!(mode(0), AssignmentMode::Strict);
    assert_eq!(mode(1), AssignmentMode::Guidance);
    assert_eq!(mode(2), AssignmentMode::Guidance);
    assert_eq!(mode(3), AssignmentMode::Strict);

    for (node, code, path) in [
        (
            json!({"id":"t","title":"T","assignment":"strict"}),
            "spec.strict_assignment_without_assignee",
            "/spec/steps/0/assignment",
        ),
        (
            json!({"id":"t","title":"T","assignee":"user:sam","assignment":"only"}),
            "spec.invalid_value",
            "/spec/steps/0/assignment",
        ),
        (
            json!({"id":"t","assignment":"strict","wait":{"for":"1s"}}),
            "spec.invalid_node_field",
            "/spec/steps/0/assignment",
        ),
    ] {
        assert_eq!(
            errors(&document(json!({"steps":[node]}))),
            [(code.to_owned(), path.to_owned())],
            "{node}"
        );
    }
    assert!(matches!(
        classify("/spec/steps/0/assignment", &json!("strict")),
        Support::Supported { .. }
    ));
}

#[test]
fn a_strict_assignment_survives_compile_and_restore() {
    let spec = parse(&document(json!({"steps":[
        {"id":"owner","title":"Owner","assignee":"${{ inputs.owner }}","assignment":"strict"}
    ],"inputs":{"owner":{"type":"string","required":true}}})))
    .unwrap();
    let compiled = compile(&spec, &CompileContext::default()).unwrap();
    let restored = restore(compiled.snapshot_bytes()).unwrap();
    assert_eq!(restored.ir(), compiled.ir());
    assert_eq!(restored.hash(), compiled.hash());
    // A guidance assignee compiles to the same definition as before LANG7.
    let plain = |assignment: Option<&str>| {
        let mut node = json!({"id":"owner","title":"Owner","assignee":"${{ inputs.owner }}"});
        if let Some(mode) = assignment {
            node["assignment"] = json!(mode);
        }
        let source =
            document(json!({"steps":[node],"inputs":{"owner":{"type":"string","required":true}}}));
        compile(&parse(&source).unwrap(), &CompileContext::default())
            .unwrap()
            .ir()
            .steps
            .clone()
    };
    assert_eq!(plain(None), plain(Some("guidance")));
    assert_ne!(plain(None), plain(Some("strict")));
}
