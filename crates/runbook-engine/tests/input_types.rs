//! LANG5: input types (`multi_enum`, `date`, `datetime`, `duration`, `url`,
//! `email`, `user`) and `pattern` validation at execution start.
//!
//! Pinned as a pure-engine conformance trace (`conformance/execution/input-types.json`).
mod support;
use runbook_engine::*;
use runbook_model::{Data, ExecutionStatus};
use serde_json::json;
use std::rc::Rc;
use support::*;

const SPEC: &str = r#"spec:
  inputs:
    tags:
      type: multi_enum
      options: [web, api, worker, db]
    deploy_date:
      type: date
    scheduled_at:
      type: datetime
    timeout:
      type: duration
    docs_url:
      type: url
    contact:
      type: email
    owner:
      type: user
    code:
      type: string
      pattern: "^[A-Z]{3}-\\d{3}$"
  steps:
    - id: verify
      title: Verify inputs
  outputs:
    tags: "${{ inputs.tags }}"
    deploy_date: "${{ inputs.deploy_date }}"
    scheduled_at: "${{ inputs.scheduled_at }}"
    timeout: "${{ inputs.timeout }}"
    docs_url: "${{ inputs.docs_url }}"
    contact: "${{ inputs.contact }}"
    owner: "${{ inputs.owner }}"
    code: "${{ inputs.code }}"
"#;

fn valid_inputs() -> Data {
    [
        ("tags".into(), json!(["web", "worker"])),
        ("deploy_date".into(), json!("2026-11-12")),
        ("scheduled_at".into(), json!("2026-11-12T19:00:00Z")),
        ("timeout".into(), json!("15m")),
        ("docs_url".into(), json!("https://runbook.example.com/docs")),
        ("contact".into(), json!("ops@runbook.example.com")),
        ("owner".into(), json!("user:usr_00000000000000000000000001")),
        ("code".into(), json!("RUN-123")),
    ]
    .into_iter()
    .collect()
}

fn run() -> Trace {
    let context = StartContext {
        inputs: valid_inputs(),
        ..Default::default()
    };
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: input-types, title: Input Types}}\n{SPEC}"
    );
    run_document_with_start(
        &source,
        &[succeeded("verify")],
        runbook_spec::CompileContext::default(),
        context,
    )
}

#[test]
fn input_types_and_pattern_run_successfully_with_valid_inputs() {
    let trace = run();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());

    // Verify output bindings
    assert_eq!(trace.state.outputs["tags"], json!(["web", "worker"]));
    assert_eq!(trace.state.outputs["deploy_date"], json!("2026-11-12"));
    assert_eq!(
        trace.state.outputs["scheduled_at"],
        json!("2026-11-12T19:00:00Z")
    );
    assert_eq!(trace.state.outputs["timeout"], json!("15m"));
    assert_eq!(
        trace.state.outputs["docs_url"],
        json!("https://runbook.example.com/docs")
    );
    assert_eq!(
        trace.state.outputs["contact"],
        json!("ops@runbook.example.com")
    );
    assert_eq!(
        trace.state.outputs["owner"],
        json!("user:usr_00000000000000000000000001")
    );
    assert_eq!(trace.state.outputs["code"], json!("RUN-123"));
}

fn check_invalid(key: &str, val: serde_json::Value) -> Vec<String> {
    let mut inputs = valid_inputs();
    inputs.insert(key.into(), val);
    let context = StartContext {
        inputs,
        ..Default::default()
    };
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: input-types, title: Input Types}}\n{SPEC}"
    );
    let definition = Rc::new(
        runbook_spec::compile(
            &runbook_spec::parse(&source).unwrap(),
            &runbook_spec::CompileContext::default(),
        )
        .unwrap(),
    );
    let state = ExecutionState::new(
        "exe_00000000000000000000000001".parse().unwrap(),
        definition,
    );
    match prepare_start(&state, &context, 0) {
        Ok(_) => panic!("expected invalid data error for key {key}"),
        Err(err) => {
            assert_eq!(err.code(), "execution.invalid_data");
            err.errors().iter().map(|e| e.code.to_string()).collect()
        }
    }
}

#[test]
fn input_validation_rejects_invalid_pattern() {
    assert_eq!(check_invalid("code", json!("invalid")), ["data.pattern"]);
    assert_eq!(check_invalid("code", json!("RUN-1234")), ["data.pattern"]);
    assert_eq!(check_invalid("code", json!("run-123")), ["data.pattern"]);
    assert_eq!(check_invalid("code", json!("")), ["data.pattern"]);
}

#[test]
fn input_validation_rejects_invalid_multi_enum() {
    assert_eq!(
        check_invalid("tags", json!(["web", "unrecognized"])),
        ["data.enum"]
    );
}

#[test]
fn input_validation_rejects_invalid_date_and_datetime() {
    assert_eq!(
        check_invalid("deploy_date", json!("not-a-date")),
        ["data.format"]
    );
    assert_eq!(
        check_invalid("deploy_date", json!("2026-13-45")),
        ["data.format"]
    );
    assert_eq!(
        check_invalid("scheduled_at", json!("not-a-datetime")),
        ["data.format"]
    );
    assert_eq!(
        check_invalid("scheduled_at", json!("2026-11-12")),
        ["data.format"]
    );
}

#[test]
fn input_validation_rejects_invalid_duration() {
    assert_eq!(
        check_invalid("timeout", json!("not-a-duration")),
        ["data.format"]
    );
    assert_eq!(check_invalid("timeout", json!("100xyz")), ["data.format"]);
}

#[test]
fn input_validation_rejects_invalid_url_email_and_user() {
    assert_eq!(
        check_invalid("docs_url", json!("not a url")),
        ["data.format"]
    );
    assert_eq!(
        check_invalid("contact", json!("not-an-email")),
        ["data.format"]
    );
    assert_eq!(check_invalid("owner", json!("not a user")), ["data.format"]);
}

#[test]
fn input_types_trace_matches_checked_in_contract() {
    let trace = run();
    let fixture = json!({
        "format": "pure-engine-trace-v1",
        "runbook": SPEC,
        "events": trace.events,
        "commands": trace.commands,
        "status": trace.state.status,
        "outputs": trace.state.outputs,
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/execution/input-types.json");
    if std::env::var_os("RUNBOOK_UPDATE_FIXTURES").is_some() {
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
        )
        .unwrap();
    }
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, fixture, "{}", path.display());
}
