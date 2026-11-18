use super::*;
use serde_json::{Value, json};

#[test]
fn native_wasm_scope_corpus_matches_from_fresh_build() {
    use std::{path::PathBuf, process::Command};
    struct FreshTarget(PathBuf);
    impl Drop for FreshTarget {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove test-owned Wasm target");
        }
    }
    let target = (0..1000)
        .find_map(|attempt| {
            let path = std::env::temp_dir().join(format!(
                "runbook-scope-parity-{}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => Some(FreshTarget(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => panic!("create fresh scope Wasm target: {error}"),
            }
        })
        .expect("allocate fresh scope Wasm target");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let build = Command::new(env!("CARGO"))
        .current_dir(&manifest)
        .args([
            "build",
            "--locked",
            "--package",
            "runbook-spec",
            "--example",
            "scope_parity",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target.0)
        .output()
        .expect("build fresh scope Wasm example");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let output = Command::new("node")
        .arg(manifest.join("../runbook-expr/tests/wasm-runner.mjs"))
        .arg(
            target
                .0
                .join("wasm32-unknown-unknown/debug/examples/scope_parity.wasm"),
        )
        .output()
        .expect("execute scope Wasm example");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wasm: Value = serde_json::from_slice(&output.stdout).expect("scope corpus JSON");
    let native = corpus::corpus();
    assert_eq!(native.as_array().unwrap().len(), 23);
    assert_eq!(native, wasm);
}

#[test]
fn inferred_arrays_reject_unrepresentable_nullable_elements() {
    for root in ["vars", "outputs"] {
        let mut body = json!({"steps":[{"id":"a","title":"A"}]});
        body[root] = json!({"xs":["${{ steps.a.verdict }}"]});
        if root == "outputs" {
            body["report"] = json!("${{ outputs.xs[0] }}");
        }
        let error = compiled(body).unwrap_err();
        assert_eq!(error.code, "spec.uninferable_type");
        assert_eq!(error.path, format!("/spec/{root}/xs"));
        assert!(error.line > 0 && error.column > 0);
    }
    // A record element can retain nullable fields; the list element itself
    // remains non-nullable. Opaque arrays need no inferred element type.
    let record_list = compiled(
        json!({"outputs":{"xs":[{"value":"${{ steps.a.verdict }}"}]},
        "report":"Value: ${{ outputs.xs[0].value }}",
        "steps":[{"id":"a","title":"A"}]}),
    )
    .unwrap();
    let report = record_list
        .sites
        .iter()
        .find(|site| site.path == "/spec/report")
        .unwrap();
    assert_eq!(
        report
            .template
            .expressions()
            .next()
            .unwrap()
            .expression
            .result_type(),
        ResultType::Nullable(Box::new(ResultType::String))
    );
    compiled(json!({"outputs":{"xs":["${{ steps.a.verdict }}",null]},
        "steps":[{"id":"a","title":"A"}]}))
    .unwrap();
}

#[test]
fn typed_lists_accept_optional_record_shapes_without_reinferring() {
    let list = json!({"type":"list","items":{"type":"object",
        "properties":{"a":{"type":"string"}}}});
    for values in [
        json!([{"a":"valid"}]),
        json!([{}]),
        json!([{}, {"a":"valid"}]),
        json!(["${{ vars.item }}"]),
    ] {
        for destination in ["fields", "vars"] {
            let mut body = json!({"inputs":{"xs":list}, "fields":{"xs":list},
                "vars":{"xs":"${{ inputs.xs }}", "item":{"a":"valid"}},
                "steps":[{"id":"s","set":{}}]});
            body["steps"][0]["set"][destination] = json!({"xs":values});
            compiled(body).unwrap();
        }
    }
    for (values, member, field_code, var_code) in [
        (
            json!([{"a":1}]),
            "a",
            "data.type_error",
            "expression.type_error",
        ),
        (
            json!([{"other":"bad"}]),
            "other",
            "data.unknown_field",
            "data.unknown_field",
        ),
    ] {
        for destination in ["fields", "vars"] {
            let mut body = json!({"inputs":{"xs":list}, "fields":{"xs":list},
                "vars":{"xs":"${{ inputs.xs }}"},
                "steps":[{"id":"s","set":{}}]});
            body["steps"][0]["set"][destination] = json!({"xs":values});
            let error = compiled(body).unwrap_err();
            if destination == "fields" {
                assert_eq!(error.code, field_code);
                assert_eq!(error.path, "/spec/steps/0/set/fields/xs");
            } else {
                assert_eq!(error.code, var_code);
                assert_eq!(error.path, format!("/spec/steps/0/set/vars/xs/0/{member}"));
            }
        }
    }
}

fn source(body: Value) -> crate::Spec {
    crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":body}).to_string()).unwrap()
}

#[test]
fn step_output_scope_is_closed_optional_and_nullable() {
    let source = source(
        json!({"steps":[{"id":"health","title":"Health","outputs":{"code":{"type":"integer","required":true}}},{"id":"next","title":"Next","if":"${{ steps.health.outputs.code == 503 && steps.health.verdict == null }}"}]}),
    );
    let ir = crate::normalize(&source).unwrap();
    assert!(check_for_test(&ir, &source).is_ok());
}

#[test]
fn vars_cycles_unknown_names_and_uninferable_types_fail() {
    for vars in [
        json!({"a":"${{ vars.a }}"}),
        json!({"a":"${{ vars.b }}","b":"${{ vars.a }}"}),
        json!({"a":"${{ vars.missing }}"}),
        json!({"a":null}),
        json!({"a":[]}),
    ] {
        let s = source(json!({"vars":vars,"steps":[]}));
        assert!(
            check_for_test(&crate::normalize(&s).unwrap(), &s).is_err(),
            "{s:?}"
        );
    }
}
#[test]
fn iteration_and_report_bindings_are_lexical() {
    let s = source(
        json!({"inputs":{"regions":{"type":"list","items":{"type":"string"}}},"steps":[{"id":"regions","foreach":{"items":"${{ inputs.regions }}","as":"region","key":"${{ region }}","max_items":2,"steps":[{"id":"child","title":"${{ region }}","if":"${{ index >= 0 }}"}] }}],"outputs":{"result":"ok"},"report":"${{ outputs.result }}"}),
    );
    check_for_test(&crate::normalize(&s).unwrap(), &s).unwrap();
    let s = source(json!({"steps":[{"id":"outside","title":"${{ region }}"}]}));
    assert!(check_for_test(&crate::normalize(&s).unwrap(), &s).is_err());
}
#[test]
fn literal_and_computed_assignments_keep_destination_types() {
    for assignment in [
        json!({"vars":{"result":1}}),
        json!({"vars":{"missing":"ok"}}),
        json!({"fields":{"severity":"bad"}}),
    ] {
        let s = source(
            json!({"vars":{"result":"ok"},"fields":{"severity":{"type":"enum","options":["none","major"]}},"steps":[{"id":"assign","set":assignment}]}),
        );
        assert!(check_for_test(&crate::normalize(&s).unwrap(), &s).is_err());
    }
}
#[test]
fn aggregate_admission_precedes_handle_allocation() {
    let title = "${{1}}".repeat(64);
    let nodes: Vec<_> = (0..17)
        .map(|i| json!({"id":format!("n{i}"),"title":title}))
        .collect();
    let s = source(json!({"steps":nodes}));
    assert!(check_for_test(&crate::normalize(&s).unwrap(), &s).is_err());
}

fn compiled(body: Value) -> Result<CheckedScopes> {
    let s = source(body);
    check_for_test(&crate::normalize(&s).unwrap(), &s)
}
fn record(env: &Environment, id: TypeId) -> &BTreeMap<String, FieldDecl> {
    let TypeDef::Record(fields) = env.handle.type_definition(id).unwrap() else {
        panic!("record")
    };
    fields
}
fn root_fields<'a>(c: &'a CheckedScopes, name: &str) -> &'a BTreeMap<String, FieldDecl> {
    let e = &c.environments[0];
    record(e, e.roots[name].ty)
}

#[test]
fn flagship_checks_fifteen_segments_at_fourteen_sites() {
    let source = crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap();
    let c = check_for_test(&crate::normalize(&source).unwrap(), &source).unwrap();
    assert_eq!(c.sites.len(), 14);
    assert_eq!(
        c.sites
            .iter()
            .map(|s| s.template.expressions().count())
            .sum::<usize>(),
        15
    );
    let paths: Vec<_> = c.sites.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "/spec/execution/title",
            "/spec/execution/concurrency/key",
            "/spec/steps/0/parallel/0/instructions",
            "/spec/steps/1/foreach/items",
            "/spec/steps/1/foreach/steps/0/title",
            "/spec/steps/1/foreach/steps/0/with/region",
            "/spec/steps/2/if",
            "/spec/steps/2/loop/until",
            "/spec/steps/3/switch/cases/0/when",
            "/spec/steps/3/switch/cases/0/steps/0/approval/prompt",
            "/spec/steps/3/switch/cases/0/steps/1/title",
            "/spec/steps/3/switch/cases/0/steps/1/if",
            "/spec/steps/3/switch/cases/0/steps/2/set/vars/result",
            "/spec/outputs/result"
        ]
    );
    for site in &c.sites {
        let env = &c.environments[c.scopes[site.scope].environment].handle;
        for expr in site.template.expressions() {
            assert_eq!(
                expr.expression.descriptor().schema_fingerprint,
                env.fingerprint()
            );
        }
    }
    assert_eq!(c.environments.len(), 3);
}
#[test]
fn vars_forward_references_ignore_mapping_order() {
    let c = compiled(json!({"vars":{"b":"${{ vars.a }}","a":"seed"},"steps":[]})).unwrap();
    assert_eq!(c.initialization_order, ["a", "b"]);
    let fields = root_fields(&c, "vars");
    assert_eq!(fields["a"].ty, fields["b"].ty);
    assert_eq!(
        c.environments[0].handle.type_definition(fields["a"].ty),
        Some(&TypeDef::String)
    );
    assert_eq!(
        c.sites[0]
            .template
            .expressions()
            .next()
            .unwrap()
            .expression
            .descriptor()
            .schema_fingerprint,
        c.environments[c.scopes[c.sites[0].scope].environment]
            .handle
            .fingerprint()
    );
    compiled(json!({"vars":{"a":"literal vars.a"},"steps":[]})).unwrap();
    compiled(json!({"inputs":{"names":{"type":"list","items":{"type":"string"}}},"vars":{"a":"${{ inputs.names.exists(vars, vars == 'a') }}"},"steps":[]})).unwrap();
}
#[test]
fn record_var_types_do_not_reuse_foreign_ids() {
    let body = json!({"inputs":{"record":{"type":"object","properties":{"name":{"type":"string","required":true}}}},"vars":{"a":"${{ inputs.record }}","b":"${{ vars.a.name }}"},"steps":[{"id":"set_record","set":{"vars":{"a":{"name":"ok"}}}}]});
    let c = compiled(body.clone()).unwrap();
    assert_eq!(c.initialization_order, ["a", "b"]);
    let mut bad = body;
    bad["steps"][0]["set"]["vars"]["a"]["name"] = json!(5);
    assert_eq!(compiled(bad).unwrap_err().code, "expression.type_error");
    for site in c.sites {
        for expr in site.template.expressions() {
            assert_eq!(
                expr.expression.descriptor().schema_fingerprint,
                c.environments[c.scopes[site.scope].environment]
                    .handle
                    .fingerprint()
            );
        }
    }
}
#[test]
fn declared_schemas_lower_without_numeric_coercion() {
    for (kind, text, ok, ty) in [
        ("integer", "1.0", true, TypeDef::Int),
        ("integer", "9223372036854775807", true, TypeDef::Int),
        ("integer", "9223372036854775808", false, TypeDef::Int),
        ("number", "1", true, TypeDef::Double),
        ("number", "1e-400", true, TypeDef::Double),
        ("number", "1.8e308", false, TypeDef::Double),
        ("integer", "\"503\"", false, TypeDef::Int),
    ] {
        let value: Value = serde_json::from_str(text).unwrap();
        let c = compiled(
            json!({"fields":{"value":{"type":kind}},"steps":[{"id":"assign","set":{"fields":{"value":value}}}]}),
        );
        assert_eq!(c.is_ok(), ok, "{kind} {text}: {c:?}");
        if let Ok(c) = c {
            assert_eq!(
                c.environments[0]
                    .handle
                    .type_definition(root_fields(&c, "fields")["value"].ty),
                Some(&ty)
            );
        }
    }
}
#[test]
fn complete_scope_limits_count_generated_wrappers() {
    for (depth, ok) in [(15, true), (16, false)] {
        let mut schema = json!({"type":"string"});
        for _ in 0..depth {
            schema = json!({"type":"list","items":schema});
        }
        let result = compiled(json!({"inputs":{"deep":schema},"steps":[]}));
        assert_eq!(result.is_ok(), ok, "{depth}: {result:?}");
        if let Err(e) = result {
            assert_eq!(e.code, "expression.schema_limit");
        }
    }
    for (count, ok) in [(256, true), (257, false)] {
        let fields: serde_json::Map<_, _> = (0..count)
            .map(|i| (format!("v{i}"), json!({"type":"string"})))
            .collect();
        assert_eq!(compiled(json!({"inputs":fields,"steps":[]})).is_ok(), ok);
    }
}
#[test]
fn opaque_data_is_not_scanned_and_parameter_types_survive() {
    let c=compiled(json!({"agents":{"params":{"s":"${{ missing }}","empty":[],"nothing":null}},"x-test":"${{ missing }}","steps":[{"id":"a","title":"A","agent":{"tools":["${{ missing }}"],"commands":["${{ missing }}"]},"with":{"empty":[],"nothing":null,"n":1,"text":"10m"}}]})).unwrap();
    assert!(c.sites.is_empty());
    assert!(matches!(&c.values["/spec/steps/0/with/empty"],CheckedValue::List(v) if v.is_empty()));
    assert!(matches!(
        &c.values["/spec/steps/0/with/nothing"],
        CheckedValue::Literal(Value::Null)
    ));
}

thread_local! {static SCRATCH:std::cell::RefCell<Vec<std::sync::Weak<TypeEnv>>>=const{std::cell::RefCell::new(Vec::new())};static MAX_SCRATCH:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
pub(super) fn record_scratch(env: &Arc<TypeEnv>) {
    SCRATCH.with(|v| {
        let mut v = v.borrow_mut();
        v.push(Arc::downgrade(env));
        MAX_SCRATCH.with(|max| {
            max.set(
                max.get()
                    .max(v.iter().filter(|e| e.strong_count() > 0).count()),
            )
        });
    });
}
#[test]
fn sites_share_environments_and_inference_temporaries_are_released() {
    SCRATCH.with(|v| v.borrow_mut().clear());
    MAX_SCRATCH.with(|v| v.set(0));
    let vars: serde_json::Map<_, _> = (0..30)
        .map(|i| (format!("v{i}"), json!("${{ inputs.name }}")))
        .collect();
    let nodes: Vec<_> = (0..40)
        .map(|i| json!({"id":format!("n{i}"),"title":"${{ inputs.name }}"}))
        .collect();
    let c =
        compiled(json!({"inputs":{"name":{"type":"string"}},"vars":vars,"steps":nodes})).unwrap();
    assert_eq!(c.sites.len(), 70);
    assert_eq!(c.environments.len(), 2);
    for site in &c.sites {
        assert!(Arc::ptr_eq(
            &c.environments[c.scopes[site.scope].environment].handle,
            &c.environments[usize::from(site.path.starts_with("/spec/vars/"))].handle
        ));
    }
    SCRATCH.with(|v| assert!(v.borrow().iter().all(|e| e.strong_count() == 0)));
    MAX_SCRATCH.with(|v| assert_eq!(v.get(), 1));
}
#[test]
fn nullable_fixed_variables_accept_null_assignments() {
    compiled(json!({"vars":{"v":"${{ steps.health.verdict }}"},"steps":[{"id":"health","title":"Health"},{"id":"assign","set":{"vars":{"v":null}}}]})).unwrap();
}

#[test]
fn aggregate_exact_segment_boundary_and_reservation_are_atomic() {
    let mut nodes: Vec<_> = (0..16)
        .map(|i| json!({"id":format!("n{i}"),"title":"${{1}}".repeat(64)}))
        .collect();
    let c = compiled(json!({"steps":nodes})).unwrap();
    assert_eq!(c.discovery.segments, 1024);
    nodes.push(json!({"id":"extra","title":"${{1}}"}));
    values::CHECKS.with(|n| n.set(0));
    let e = compiled(json!({"steps":nodes})).unwrap_err();
    assert_eq!(e.code, "spec.compile_limit");
    assert_eq!(e.path, "/spec/steps/16/title");
    values::CHECKS.with(|n| assert_eq!(n.get(), 0));
    let mut r = Resources::default();
    r.admit(65_536, 262_144, "/spec").unwrap();
    assert_eq!(r.admit(1, 0, "/types").unwrap_err().path, "/types");
    assert_eq!(r.type_slots, 65_536);
    assert_eq!(r.admit(0, 1, "/fields").unwrap_err().path, "/fields");
    assert_eq!(r.field_slots, 262_144);
}
#[test]
fn source_limits_do_not_hide_complete_scope_limits() {
    let nodes: Vec<_> = (0..257)
        .map(|i| json!({"id":format!("n{i}"),"title":"Task"}))
        .collect();
    let s = source(json!({"steps":nodes}));
    let ir = crate::normalize(&s).unwrap();
    assert_eq!(
        check_for_test(&ir, &s).unwrap_err().code,
        "expression.schema_limit"
    );
}
#[test]
fn output_availability_and_approval_shapes_are_explicit() {
    let c=compiled(json!({"steps":[{"id":"health","title":"Health","outputs":{"code":{"type":"integer","required":true}}},{"id":"approve","approval":{"prompt":"Approve?","options":["yes","no"],"approvers":{"teams":["ops"]}}}]})).unwrap();
    let env = &c.environments[0];
    let steps = root_fields(&c, "steps");
    assert!(!steps["health"].required);
    let health = record(env, steps["health"].ty);
    assert!(health["verdict"].nullable);
    assert!(health["verdict"].required);
    assert!(!health["outputs"].required);
    assert!(!record(env, health["outputs"].ty)["code"].required);
    let approval = record(env, record(env, steps["approve"].ty)["outputs"].ty);
    assert_eq!(
        approval.keys().map(String::as_str).collect::<Vec<_>>(),
        ["comment", "decided_by", "decision"]
    );
    let execution = root_fields(&c, "execution");
    assert!(!execution["started_at"].required);
    assert!(!execution["started_at"].nullable);
}
#[test]
fn alias_collisions_and_outer_visibility() {
    for alias in [
        "inputs",
        "project",
        "steps",
        "outputs",
        "index",
        "iteration",
    ] {
        let e=compiled(json!({"inputs":{"names":{"type":"list","items":{"type":"string"}}},"steps":[{"id":"f","foreach":{"items":"${{ inputs.names }}","as":alias,"max_items":1,"steps":[]}}]})).unwrap_err();
        assert_eq!(e.code, "spec.alias_collision");
    }
    let mut body = json!({"inputs":{"names":{"type":"list","items":{"type":"string"}}},"steps":[{"id":"outer","foreach":{"items":"${{ inputs.names }}","as":"region","max_items":1,"steps":[{"id":"inner","foreach":{"items":"${{ inputs.names }}","as":"service","max_items":1,"steps":[{"id":"child","title":"${{ region }} ${{ service }}"}]}}]}}]});
    compiled(body.clone()).unwrap();
    body["steps"][0]["foreach"]["steps"][0]["foreach"]["as"] = json!("region");
    assert_eq!(compiled(body).unwrap_err().code, "spec.alias_collision");
}
#[test]
fn report_audience_and_ancillary_sites_check() {
    let c=compiled(json!({"outputs":{"result":"ok"},"report":"${{ outputs.result }}","steps":[{"id":"a","title":"A","instructions":{"default":"${{ 'default' }}","human":"${{ 'human' }}","agent":"${{ 'agent' }}"},"checklist":["${{ 'label' }}"],"assignee":"${{ 'user:jon' }}"},{"id":"wait","wait":{"until":"${{ timestamp('2026-01-01T00:00:00Z') }}"}},{"id":"end","end":{"status":"succeeded","message":"${{ 'done' }}"}}]})).unwrap();
    assert_eq!(c.sites.len(), 8);
    for literal in ["2026-01-01T00:00:00Z", "2026-01-01T01:00:00+01:00"] {
        compiled(json!({"steps":[{"id":"wait","wait":{"until":literal}}]})).unwrap();
    }
    assert!(compiled(json!({"steps":[{"id":"wait","wait":{"until":"tomorrow"}}]})).is_err());
    for (title, code) in [
        ("${{ outputs.result }}", "expression.type_error"),
        ("${{ iteration }}", "expression.type_error"),
        ("${{ steps.a.outputs.missing }}", "expression.type_error"),
    ] {
        let e = compiled(json!({"steps":[{"id":"a","title":title}]})).unwrap_err();
        assert_eq!(e.code, code);
    }
}
#[test]
fn inference_numeric_spelling_and_schema_name_paths() {
    let vars: Value = serde_json::from_str(r#"{"a":1,"b":1.0,"c":1e2,"d":1e-400}"#).unwrap();
    let c = compiled(json!({"vars":vars,"steps":[]})).unwrap();
    let vars = root_fields(&c, "vars");
    let env = &c.environments[0];
    assert_eq!(
        env.handle.type_definition(vars["a"].ty),
        Some(&TypeDef::Int)
    );
    for n in ["b", "c", "d"] {
        assert_eq!(
            env.handle.type_definition(vars[n].ty),
            Some(&TypeDef::Double)
        );
    }
    let e = compiled(json!({"inputs":{"not-valid":{"type":"string"}},"steps":[]})).unwrap_err();
    assert_eq!(e.code, "expression.schema_error");
    assert_eq!(e.path, "/spec/inputs/not-valid");
    assert!(e.column > 0);
}

#[test]
fn nested_declaration_names_point_to_original_schema_property() {
    let e=compiled(json!({"inputs":{"record":{"type":"object","properties":{"bad-name":{"type":"string"}}}},"steps":[]})).unwrap_err();
    assert_eq!(e.path, "/spec/inputs/record/properties/bad-name");
}
#[test]
fn nullable_computed_null_keeps_known_destination() {
    compiled(json!({"vars":{"v":"${{ steps.a.verdict }}"},"steps":[{"id":"a","title":"A"},{"id":"assign","set":{"vars":{"v":"${{ null }}"}}}]})).unwrap();
}
#[test]
fn arbitrary_outputs_only_infer_when_report_needs_closed_record() {
    let body = json!({"outputs":{"empty":[],"nothing":null},"steps":[]});
    compiled(body.clone()).unwrap();
    let mut report = body;
    report["report"] = json!("Report");
    assert_eq!(compiled(report).unwrap_err().code, "spec.uninferable_type");
}

#[test]
fn distinct_environment_aggregate_limits_reject_real_scopes() {
    let fields = compiled(corpus::combined_fixture(78, 5, 0)).unwrap();
    assert!(fields.resources.field_slots <= 262_144);
    let e = compiled(corpus::combined_fixture(79, 5, 0)).unwrap_err();
    assert_eq!(e.code, "spec.compile_limit");
    assert!(e.path.ends_with("/foreach"));
    let types = compiled(corpus::combined_fixture(96, 1, 0)).unwrap();
    assert_eq!(types.environments.len(), 97);
    assert!(types.resources.type_slots <= 65_536);
    let e = compiled(corpus::combined_fixture(97, 1, 0)).unwrap_err();
    assert_eq!(e.code, "spec.compile_limit");
    assert_eq!(e.path, "/spec/steps/96/foreach");
}

thread_local! {pub(super) static DAG_BUILDS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[test]
fn oversized_variable_and_report_records_fail_before_bulk_work() {
    let vars: serde_json::Map<_, _> = (0..257)
        .map(|i| (format!("v{i}"), json!("${{ vars }}")))
        .collect();
    DAG_BUILDS.with(|v| v.set(0));
    values::CHECKS.with(|v| v.set(0));
    let e = compiled(json!({"vars":vars,"steps":[]})).unwrap_err();
    assert_eq!(e.code, "expression.schema_limit");
    assert_eq!(e.path, "/spec/vars");
    DAG_BUILDS.with(|v| assert_eq!(v.get(), 0));
    values::CHECKS.with(|v| assert_eq!(v.get(), 0));
    let outputs: serde_json::Map<_, _> = (0..257)
        .map(|i| (format!("v{i}"), json!("${{ missing }}")))
        .collect();
    let e = compiled(json!({"outputs":outputs,"report":"Report","steps":[]})).unwrap_err();
    assert_eq!(e.code, "expression.schema_limit");
    assert_eq!(e.path, "/spec/outputs");
    values::CHECKS.with(|v| assert_eq!(v.get(), 0));
}

thread_local! {static COMPATIBILITY_VISITS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};static COMPATIBILITY_CEILING:std::cell::Cell<usize>=const{std::cell::Cell::new(usize::MAX)};}
pub(super) fn compatibility_visit() {
    COMPATIBILITY_VISITS.with(|v| {
        v.set(v.get() + 1);
        COMPATIBILITY_CEILING.with(|cap| {
            assert!(
                v.get() <= cap.get(),
                "shared type pairs were expanded repeatedly"
            )
        });
    });
}
#[test]
fn compatibility_visits_shared_type_pairs_once() {
    let mut arena = Arena::default();
    let scalar = arena.intern(TypeDef::String, "/test").unwrap();
    let (mut actual, mut expected) = (scalar, scalar);
    for _ in 0..12 {
        actual = arena
            .record(
                (0..12)
                    .map(|i| (format!("p{i}"), field(actual, true)))
                    .collect(),
                "/test",
            )
            .unwrap();
        expected = arena
            .record(
                (0..12)
                    .map(|i| (format!("p{i}"), field(expected, false)))
                    .collect(),
                "/test",
            )
            .unwrap();
    }
    COMPATIBILITY_VISITS.with(|v| v.set(0));
    COMPATIBILITY_CEILING.with(|v| v.set(1024));
    assert!(arena.compatible(&field(actual, true), &field(expected, true)));
    COMPATIBILITY_VISITS.with(|v| assert_eq!(v.get(), 12));
    assert!(!arena.compatible(&field(expected, true), &field(actual, true)));
}

#[test]
fn expression_diagnostics_use_yaml_scalar_anchors() {
    let source=crate::parse("apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec:\n  steps:\n    - id: task\n      title: >-\n        Prefix\n        ${{ missing }}\n").unwrap();
    let expected = source.source_spans["/spec/steps/0/title"];
    let e = check_for_test(&crate::normalize(&source).unwrap(), &source).unwrap_err();
    assert_eq!(e.path, "/spec/steps/0/title");
    assert_eq!((e.line, e.column), (expected.line, expected.column));
}
#[test]
fn dynamic_field_constraints_remain_runtime_work() {
    compiled(json!({"fields":{"severity":{"type":"enum","options":["none","major"]}},"steps":[{"id":"assign","set":{"fields":{"severity":"${{ 'not_an_enum_member' }}"}}}]})).unwrap();
}
