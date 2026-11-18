use super::*;
use serde_json::json;
fn fixture() -> CompiledRunbook {
    let spec=crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"vars":{"b":"${{ vars.a }}","a":"alpha"},"steps":[{"id":"task","title":"${{ vars.b }}"}]}}).to_string()).unwrap();
    compile(&spec, &CompileContext::default()).unwrap()
}
fn changed(f: impl FnOnce(&mut crate::snapshot::UnvalidatedSnapshot)) -> Vec<u8> {
    let compiled = fixture();
    let mut s = crate::snapshot::decode(compiled.snapshot_bytes(), limits()).unwrap();
    f(&mut s);
    crate::snapshot::encode(
        crate::snapshot::SnapshotRef {
            ir: &s.ir,
            bindings: &s.bindings,
            project_variables: &s.project_variables,
            hash: s.hash,
            worst_case_step_runs: s.worst_case_step_runs,
        },
        limits(),
    )
    .unwrap()
}
#[test]
fn restore_checks_hash_and_budget() {
    assert_eq!(
        restore(&changed(|s| s.hash[0] ^= 1)).unwrap_err()[0].code,
        "spec.snapshot_hash_mismatch"
    );
    assert_eq!(
        restore(&changed(|s| s.worst_case_step_runs += 1)).unwrap_err()[0].code,
        "spec.snapshot_budget_mismatch"
    );
}
#[test]
fn restore_rechecks_invariants_and_templates() {
    let error = restore(&changed(|s| s.ir.steps.push(s.ir.steps[0].clone()))).unwrap_err();
    assert_eq!(error[0].code, "spec.duplicate_id");
    let error = restore(&changed(|s| {
        s.ir.steps[0].source_path = "/spec/vars/a".into()
    }))
    .unwrap_err();
    assert_eq!(error[0].code, "spec.invalid_ir");
    let error = restore(&changed(|s| {
        s.ir.steps[0].title = Some("${{ unknown.member }}".into());
        s.hash = crate::semantic::hash(&s.ir, &pins(&s.bindings, &s.project_variables));
    }))
    .unwrap_err();
    assert!(error[0].code.starts_with("expression."));
    assert_eq!(error[0].path, "/spec/steps/0/title");
    let error = restore(&changed(|s| {
        s.project_variables.kind = crate::DataType::String
    }))
    .unwrap_err();
    assert_eq!(error[0].code, "spec.invalid_schema");
    let error = restore(&changed(|s| {
        s.bindings
            .users
            .insert(
                "unused".into(),
                "usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            )
            .map(|_| ())
            .unwrap_or(())
    }))
    .unwrap_err();
    assert_eq!(error[0].code, "spec.snapshot_pins_mismatch");
}
#[test]
fn restore_ignores_source_as_authority() {
    let original = fixture();
    let bytes = changed(|s| {
        s.ir.source_yaml = "not YAML: [ -- deliberately invalid".into();
        s.ir.source_json = json!({"steps":"attacker annotation"});
    });
    let restored = restore(&bytes).unwrap();
    assert_eq!(restored.hash(), original.hash());
    assert_eq!(restored.ir().steps, original.ir().steps);
    assert_eq!(restored.initialization_order(), ["a", "b"]);
    assert_eq!(
        restored.checked_sites().map(|s| s.path).collect::<Vec<_>>(),
        original.checked_sites().map(|s| s.path).collect::<Vec<_>>()
    );
    assert_eq!(
        restored.ir().source_yaml,
        "not YAML: [ -- deliberately invalid"
    );
}
#[test]
fn snapshot_limits_are_enforced_before_decode() {
    let bytes = vec![b'!'; MAX_SNAPSHOT_BYTES + 1];
    assert_eq!(restore(&bytes).unwrap_err()[0].code, "spec.snapshot_limit");
    assert_eq!(restore(b"!").unwrap_err()[0].code, "spec.invalid_snapshot");
}
#[test]
fn selected_context_count_matches_compact_json() {
    let mut project=crate::parse_declarations(&json!({"s":{"type":"string","description":"","default":"é\n\"\\","x-note":{"opaque":[null,true,false,1.0]}},"n":{"type":"number","min":0.5,"max":2.0},"e":{"type":"enum","options":["a","b"]},"o":{"type":"object","properties":{"list":{"type":"list","items":{"type":"integer"}}}}})).unwrap();
    let bindings = crate::bindings::Bindings {
        skills: [("skill://a\"é".into(), [3; 32])].into(),
        users: [(
            "a\n".into(),
            "usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        )]
        .into(),
        teams: BTreeMap::new(),
    };
    let json = json!({"skills":{"skill://a\"é":"03".repeat(32)},"users":{"a\n":"usr_01ARZ3NDEKTSV4RRFFQ69G5FAV"},"teams":{},"project_variables":crate::admission::project_source(&project).unwrap()});
    let padding = MAX_COMPILE_CONTEXT_BYTES - json.to_string().len();
    let crate::DataType::Object { properties } = &mut project.kind else {
        panic!()
    };
    properties.get_mut("s").unwrap().description = Some("x".repeat(padding));
    crate::context::admit(&bindings, &project).unwrap();
    let crate::DataType::Object { properties } = &mut project.kind else {
        panic!()
    };
    properties
        .get_mut("s")
        .unwrap()
        .description
        .as_mut()
        .unwrap()
        .push('x');
    assert_eq!(
        crate::context::admit(&bindings, &project).unwrap_err()[0].code,
        "spec.context_limit"
    );
}

// CF13b: the 256-output cap applies at publish and to a new run, not on
// restore. A version published before CF13 with more outputs and no
// `report` still restores (every run pinned to it loads); publishing one is
// refused (`tests/resource_limits.rs`), and a new run on it is refused.
#[test]
fn a_stored_version_past_the_output_cap_restores_and_refuses_new_runs() {
    let bytes = changed(|s| {
        for i in 0..300 {
            s.ir.outputs.insert(format!("o{i}"), json!("${{ vars.a }}"));
        }
        s.hash = crate::semantic::hash(&s.ir, &pins(&s.bindings, &s.project_variables));
    });
    let restored = restore(&bytes).expect("a stored version past the cap restores");
    assert_eq!(restored.ir().outputs.len(), 300);
    let refused = restored.admit_new_run().unwrap_err();
    assert_eq!(
        (refused[0].code.as_str(), refused[0].path.as_str()),
        ("expression.schema_limit", "/spec/outputs")
    );
    // At the cap a new run is admitted.
    let bytes = changed(|s| {
        for i in 0..256 {
            s.ir.outputs.insert(format!("o{i}"), json!("${{ vars.a }}"));
        }
        s.hash = crate::semantic::hash(&s.ir, &pins(&s.bindings, &s.project_variables));
    });
    restore(&bytes).unwrap().admit_new_run().unwrap();
}
