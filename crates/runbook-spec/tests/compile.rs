use runbook_spec::{CompileContext, MAX_COMPILE_CONTEXT_BYTES, compile, parse, restore};
use serde_json::json;
fn context() -> CompileContext {
    CompileContext {
        skills: [
            ("./skills/check-error-rates".into(), [1; 32]),
            ("./skills/rollback-service".into(), [2; 32]),
        ]
        .into(),
        teams: [
            (
                "oncall".into(),
                "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            ),
            (
                "eng-leads".into(),
                "team_01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap(),
            ),
        ]
        .into(),
        ..Default::default()
    }
}
fn source(body: serde_json::Value) -> runbook_spec::Spec {
    parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":body}).to_string()).unwrap()
}
#[test]
fn flagship_budget_and_hash_are_stable() {
    let spec = parse(include_str!("fixtures/post-deploy.yaml")).unwrap();
    let first = compile(&spec, &context()).unwrap();
    let second = compile(&spec, &context()).unwrap();
    assert_eq!(first.worst_case_step_runs(), 20);
    assert_eq!(first.skill_hashes(), second.skill_hashes());
    assert_eq!(first.checked_sites().count(), 14);
    assert_eq!(
        first
            .checked_sites()
            .map(|s| s.template.expressions().count())
            .sum::<usize>(),
        15
    );
    assert_eq!(first.hash(), second.hash());
    let restored = restore(first.snapshot_bytes()).unwrap();
    assert_eq!(first.ir(), restored.ir());
    assert_eq!(first.hash(), restored.hash());
    assert_eq!(
        first.worst_case_step_runs(),
        restored.worst_case_step_runs()
    );
}
#[test]
fn compile_context_requires_object_project_vars() {
    let spec = source(json!({"steps":[{"id":"task","title":"${{ project.vars.region }}"}]}));
    let mut ctx = CompileContext {
        project_variables: runbook_spec::parse_schema(&json!({"type":"string"})).unwrap(),
        ..Default::default()
    };
    assert_eq!(
        compile(&spec, &ctx).unwrap_err()[0].code,
        "spec.invalid_schema"
    );
    ctx.project_variables =
        runbook_spec::parse_declarations(&json!({"region":{"type":"string"}})).unwrap();
    compile(&spec, &ctx).unwrap();
    let errors = compile(&spec, &CompileContext::default()).unwrap_err();
    assert_eq!(errors[0].path, "/spec/steps/0/title");
}
#[test]
fn compile_context_budget_exact_and_one_over() {
    let spec = source(json!({"steps":[]}));
    let mut ctx = CompileContext {
        project_variables: runbook_spec::parse_declarations(
            &json!({"p":{"type":"string","description":""}}),
        )
        .unwrap(),
        ..Default::default()
    };
    let base=json!({"skills":{},"users":{},"teams":{},"project_variables":{"p":{"type":"string","required":false,"description":""}}}).to_string().len();
    let runbook_spec::DataType::Object { properties } = &mut ctx.project_variables.kind else {
        panic!()
    };
    properties.get_mut("p").unwrap().description =
        Some("x".repeat(MAX_COMPILE_CONTEXT_BYTES - base));
    let compiled = compile(&spec, &ctx).unwrap();
    restore(compiled.snapshot_bytes()).unwrap();
    let runbook_spec::DataType::Object { properties } = &mut ctx.project_variables.kind else {
        panic!()
    };
    properties
        .get_mut("p")
        .unwrap()
        .description
        .as_mut()
        .unwrap()
        .push('x');
    let invalid_expression = source(json!({"steps":[{"id":"task","title":"${{ missing.name }}"}]}));
    assert_eq!(
        compile(&invalid_expression, &ctx).unwrap_err()[0].code,
        "spec.context_limit"
    );
}
#[test]
fn compiled_runbook_is_input_independent() {
    let mut spec = source(json!({"steps":[{"id":"task","title":"Task"}]}));
    let mut ctx = CompileContext::default();
    let compiled = compile(&spec, &ctx).unwrap();
    let original = compiled.snapshot_bytes().to_vec();
    spec.raw_source = "changed".into();
    spec.metadata.title = "changed".into();
    ctx.project_variables =
        runbook_spec::parse_declarations(&json!({"extra":{"type":"string"}})).unwrap();
    assert_eq!(compiled.snapshot_bytes(), original);
    assert_eq!(compiled.ir().metadata.title, "Test");
}

#[path = "support/structural_cases.rs"]
mod structural_cases;
#[test]
fn compile_structural_conformance_matches_artifact() {
    for (source, expected) in structural_cases::cases() {
        let result =
            parse(&source.to_string()).and_then(|spec| compile(&spec, &CompileContext::default()));
        assert_eq!(result.is_ok(), expected, "{source}: {result:?}");
    }
}
#[test]
fn compile_pins_only_required_facts() {
    let spec = parse(include_str!("fixtures/post-deploy.yaml")).unwrap();
    let mut ctx = context();
    let compiled = compile(&spec, &ctx).unwrap();
    ctx.skills.insert("unused".repeat(1_048_576), [9; 32]);
    ctx.teams.insert(
        "unused".into(),
        "team_01ARZ3NDEKTSV4RRFFQ69G5FAX".parse().unwrap(),
    );
    assert_eq!(compiled.hash(), compile(&spec, &ctx).unwrap().hash());
    ctx.skills
        .insert("./skills/rollback-service".into(), [9; 32]);
    assert_ne!(compiled.hash(), compile(&spec, &ctx).unwrap().hash());
    ctx.skills.remove("./skills/check-error-rates");
    let errors = compile(&spec, &ctx).unwrap_err();
    assert_eq!(errors[0].code, "spec.unresolved_skill");
    assert_eq!(errors[0].path, "/spec/steps/1/foreach/steps/0/skill");
}

#[test]
fn checked_values_are_read_only_runtime_inputs() {
    use runbook_spec::{CheckedValue, CompileContext, compile, parse, restore};
    let source = parse("apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: runtime, title: Runtime}\nspec:\n  vars: {n: 1}\n  outputs: {result: {literal: [true], expression: '${{ vars.n }}'}}\n  steps: []\n").unwrap();
    let original = compile(&source, &CompileContext::default()).unwrap();
    for compiled in [&original, &restore(original.snapshot_bytes()).unwrap()] {
        let CheckedValue::Object(fields) = compiled.checked_value("/spec/outputs/result").unwrap()
        else {
            panic!("object");
        };
        let CheckedValue::Template(index) = fields["expression"] else {
            panic!("template");
        };
        assert_eq!(
            compiled.checked_site(index).unwrap().path,
            "/spec/outputs/result/expression"
        );
        let CheckedValue::List(items) = &fields["literal"] else {
            panic!("list");
        };
        assert!(matches!(
            &items[0],
            CheckedValue::Literal(serde_json::Value::Bool(true))
        ));
    }
}

#[test]
fn initializer_environment_permits_dependency_order_without_placeholder_values() {
    use runbook_expr::TypeDef;
    let source = runbook_spec::parse("apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: init, title: Init}\nspec:\n  vars: {copied: '${{ vars.n }}', n: 1}\n  steps: []").unwrap();
    let c = runbook_spec::compile(&source, &runbook_spec::CompileContext::default()).unwrap();
    assert_eq!(c.initialization_order(), ["n", "copied"]);
    let site = c
        .checked_sites()
        .find(|s| s.path == "/spec/vars/copied")
        .unwrap();
    let TypeDef::Record(init_vars) = site
        .environment
        .type_definition(site.environment.binding_roots()["vars"].ty)
        .unwrap()
    else {
        panic!("vars record");
    };
    assert!(init_vars.values().all(|d| !d.required));
    let TypeDef::Record(normal_vars) = c
        .environment()
        .type_definition(c.environment().binding_roots()["vars"].ty)
        .unwrap()
    else {
        panic!("vars record");
    };
    assert!(normal_vars.values().all(|d| d.required));
}
