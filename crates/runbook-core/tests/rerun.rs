//! B9 `rerun_validates_original_initial_data` (core part): the original run's
//! start fact is validated against the target version; failures are field errors.
use runbook_core::rerun::{RerunInvocation, RerunRejected, rerun_context};
use runbook_engine::{Event, ExecutionState, InitialData, StartContext, prepare_start};
use runbook_spec::{CompiledRunbook, compile, parse};
use serde_json::json;
use std::rc::Rc;

fn build(inputs: &str, fields: &str) -> CompiledRunbook {
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: rerun, title: Rerun}}\n\
         spec:\n  inputs: {{{inputs}}}\n  fields: {{{fields}}}\n  steps: [{{id: work, title: Work}}]\n"
    );
    compile(&parse(&source).unwrap(), &Default::default()).unwrap()
}
const V1_INPUTS: &str = "service: {type: string, required: true}, regions: {type: list, items: {type: string}, default: [us-west]}";
const V1_FIELDS: &str = "severity: {type: enum, options: [none, minor, major], default: none}";

fn start(compiled: CompiledRunbook, context: &StartContext) -> Result<InitialData, String> {
    let state = ExecutionState::new(
        "exe_00000000000000000000000001".parse().unwrap(),
        Rc::new(compiled),
    );
    match prepare_start(&state, context, 1_000) {
        Ok(Event::ExecutionStarted { data, .. }) => Ok(*data),
        Ok(other) => Err(format!("unexpected {other:?}")),
        Err(e) => Err(e.code().into()),
    }
}
fn original() -> InitialData {
    let mut context = StartContext::default();
    context.inputs.insert("service".into(), json!("api"));
    context.url = "https://app.example/run/1".into();
    start(build(V1_INPUTS, V1_FIELDS), &context).unwrap()
}
fn invocation() -> RerunInvocation {
    RerunInvocation {
        url: "https://app.example/run/2".into(),
        created_by: "usr_00000000000000000000000002".into(),
        project_vars: Default::default(),
    }
}
fn errors(result: Result<StartContext, RerunRejected>) -> Vec<(String, String)> {
    result
        .unwrap_err()
        .errors
        .into_iter()
        .map(|e| (e.path, e.code))
        .collect()
}

#[test]
fn rerun_on_same_version_reuses_resolved_inputs_and_initial_fields() {
    let original = original();
    // The start fact already carries resolved defaults.
    assert_eq!(original.context.inputs["regions"], json!(["us-west"]));
    assert_eq!(original.context.fields["severity"], json!("none"));
    let context = rerun_context(&original, &build(V1_INPUTS, V1_FIELDS), invocation()).unwrap();
    assert_eq!(context.inputs, original.context.inputs);
    assert_eq!(context.fields, original.context.fields);
    assert_eq!(context.url, "https://app.example/run/2");
    assert_eq!(context.created_by, "usr_00000000000000000000000002");
    // The rebuilt context really starts the target version.
    assert!(start(build(V1_INPUTS, V1_FIELDS), &context).is_ok());
}

#[test]
fn rerun_validates_original_initial_data() {
    let original = original();

    // A newly required input rejects the rerun with a field error.
    let v2 = build(
        &format!("{V1_INPUTS}, region: {{type: string, required: true}}"),
        V1_FIELDS,
    );
    let found = errors(rerun_context(&original, &v2, invocation()));
    assert!(
        found.contains(&("inputs.region".into(), "data.required".into())),
        "{found:?}"
    );

    // A changed type is a type error on that input.
    let v3 = build(
        "service: {type: integer, required: true}, regions: {type: list, items: {type: string}, default: [us-west]}",
        V1_FIELDS,
    );
    let found = errors(rerun_context(&original, &v3, invocation()));
    assert!(
        found
            .iter()
            .any(|(path, code)| path == "inputs.service" && code.starts_with("data.")),
        "{found:?}"
    );

    // An input the target no longer declares is an error, not silently dropped.
    let v4 = build(
        "regions: {type: list, items: {type: string}, default: [us-west]}",
        V1_FIELDS,
    );
    let found = errors(rerun_context(&original, &v4, invocation()));
    assert!(
        found.iter().any(|(path, _)| path == "inputs.service"),
        "{found:?}"
    );

    // A changed field enum that excludes the original value is a field error.
    let v5 = build(
        V1_INPUTS,
        "severity: {type: enum, options: [low, high], default: low}",
    );
    let found = errors(rerun_context(&original, &v5, invocation()));
    assert!(
        found.iter().any(|(path, _)| path == "fields.severity"),
        "{found:?}"
    );
}

#[test]
fn new_optional_inputs_and_fields_take_their_defaults() {
    let original = original();
    let v2 = build(
        &format!("{V1_INPUTS}, retries: {{type: integer, default: 3}}"),
        &format!("{V1_FIELDS}, owner: {{type: string, default: oncall}}"),
    );
    let context = rerun_context(&original, &v2, invocation()).unwrap();
    assert_eq!(context.inputs["retries"], json!(3));
    assert_eq!(context.inputs["service"], json!("api"));
    assert_eq!(context.fields["owner"], json!("oncall"));
    assert!(start(v2, &context).is_ok());
}

// F22: a rerun never inherits the original's absolute hard deadline; the
// host derives a fresh one from the rerun's own start. Count limits carry.
#[test]
fn rerun_does_not_copy_the_absolute_hard_deadline() {
    let mut original = original();
    original.context.limits = runbook_engine::ExecutionLimits {
        max_step_runs: Some(50),
        hard_deadline_at_ms: Some(2_000_000),
    };
    let context = rerun_context(&original, &build(V1_INPUTS, V1_FIELDS), invocation()).unwrap();
    assert_eq!(context.limits.hard_deadline_at_ms, None);
    assert_eq!(context.limits.max_step_runs, Some(50));
}
