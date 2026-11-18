use super::*;

fn admitted(body: &str) -> NormalizedRunbook {
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test}}\nspec:\n{body}"
    );
    crate::normalize(&crate::parse(&source).unwrap()).unwrap()
}
fn task() -> NormalizedRunbook {
    admitted("  steps: [{id: task, title: Task}]\n")
}

#[test]
fn flagship_typed_semantics_roundtrip() {
    let ir = crate::normalize(
        &crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap(),
    )
    .unwrap();
    assert_eq!(validate_ir(ir.clone()).unwrap(), ir);
}
#[test]
fn companion_control_kinds_roundtrip() {
    let ir = admitted(
        r#"  steps:
    - id: group
      steps: [{id: task, title: Task}]
      catch: [{id: recovery, title: Recovery}]
      finally: [{id: cleanup, title: Cleanup}]
    - id: parallel
      parallel: [{id: branch, title: Branch}]
    - id: each
      foreach: {items: '${{ inputs.items }}', max_items: 2, steps: [{id: each_task, title: Each}]}
    - id: loop
      loop: {until: '${{ true }}', max_iterations: 2, steps: [{id: loop_task, title: Loop}]}
    - id: switch
      switch:
        cases: [{when: '${{ true }}', steps: [{id: case_task, title: Case}]}]
        default: {steps: []}
    - id: wait
      wait: {for: 1ms}
    - id: approval
      approval: {prompt: Approve, options: [yes, no], approvers: {users: [alice], teams: [ops]}}
    - id: set
      set: {vars: {answer: 42}}
    - id: end
      end: {status: succeeded}
"#,
    );
    assert_eq!(validate_ir(ir.clone()).unwrap(), ir);
}
#[test]
fn saved_source_annotations_are_ignored() {
    let mut ir = task();
    ir.source_json = serde_json::json!({"spec":{"steps":[{"malicious":"source"}]}});
    ir.source_yaml = "invalid: [source".into();
    assert_eq!(validate_ir(ir.clone()).unwrap(), ir);
}
#[test]
fn template_checking_is_deferred_to_scopes() {
    let mut ir = task();
    ir.steps[0].guard = Some("${{ invalid( }}".into());
    assert!(validate_ir(ir).is_ok());
}
#[test]
fn tampered_control_scalar_invariants_are_rejected() {
    let mut ir = task();
    ir.steps.push(ir.steps[0].clone());
    assert!(validate_ir(ir).is_err());
    let mut ir = task();
    ir.steps[0].title = None;
    assert!(validate_ir(ir).is_err());
    let mut ir = task();
    if let NodeKind::Task(t) = &mut ir.steps[0].kind {
        t.retry.max_attempts = 0;
    }
    assert!(validate_ir(ir).is_err());
    let mut ir = task();
    ir.execution.cleanup_timeout.millis = 0;
    assert!(validate_ir(ir).is_err());
    let mut ir = task();
    ir.execution.concurrency = Some(Concurrency {
        key: "test".into(),
        limit: 0,
        on_conflict: ConcurrencyPolicy::Queue,
    });
    assert!(validate_ir(ir).is_err());
    let mut ir = admitted(
        "  steps: [{id: loop, loop: {until: '${{ true }}', max_iterations: 1, steps: []}}]\n",
    );
    if let NodeKind::Loop(l) = &mut ir.steps[0].kind {
        l.max_iterations = 0;
    }
    assert!(validate_ir(ir).is_err());
}
#[test]
fn malformed_typed_schemas_are_rejected() {
    let base = admitted("  inputs: {count: {type: integer, min: 1, max: 3}}\n  steps: []\n");
    let mutate = |f: fn(&mut DataSchema)| {
        let mut ir = base.clone();
        if let DataType::Object { properties } = &mut ir.inputs.kind {
            f(properties.get_mut("count").unwrap());
        }
        assert!(validate_ir(ir).is_err());
    };
    mutate(|s| s.default = DefaultValue::Value(text("bad")));
    mutate(|s| s.min = Some(4.into()));
    mutate(|s| s.kind = DataType::Boolean);
    mutate(|s| {
        s.extensions.insert("unknown".into(), true.into());
    });
    let mut ir = base.clone();
    ir.inputs.required = true;
    assert!(validate_ir(ir).is_err());
    let mut ir = base;
    ir.inputs.kind = DataType::String;
    assert!(validate_ir(ir).is_err());
}
#[test]
fn opaque_private_number_marker_stays_an_object() {
    let mut ir = task();
    let mut literal = object();
    literal.insert("$serde_json::private::Number".into(), text("42"));
    ir.variables
        .insert("literal".into(), Value::Object(literal));
    assert_eq!(validate_ir(ir.clone()).unwrap(), ir);
}

#[test]
fn forged_expression_addresses_are_rejected() {
    let mut ir = crate::normalize(&crate::parse(r#"{"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"steps":[{"id":"task","title":"Task"}]}}"#).unwrap()).unwrap();
    ir.steps[0].source_path = "/spec/vars/forged".into();
    assert_eq!(validate_ir(ir).unwrap_err()[0].code, "spec.invalid_ir");
}
#[test]
fn a_stored_loop_needs_exactly_one_condition() {
    for (until, r#while) in [(None, None), (Some("${{ true }}"), Some("${{ true }}"))] {
        let mut ir = admitted(
            "  steps: [{id: loop, loop: {until: '${{ true }}', max_iterations: 1, steps: []}}]\n",
        );
        let NodeKind::Loop(l) = &mut ir.steps[0].kind else {
            panic!("not a loop")
        };
        l.until = until.map(str::to_owned);
        l.r#while = r#while.map(str::to_owned);
        let errors = validate_ir(ir).unwrap_err();
        assert_eq!(errors[0].code, "spec.invalid_ir", "{errors:?}");
    }
}
