mod support;
use support::*;
#[test]
fn set_updates_vars_and_fields_together() {
    let trace = run_case(
        "spec:\n  fields: {severity: {type: string, default: minor}}\n  vars: {result: healthy}\n  steps:\n    - id: change\n      set: {fields: {severity: major}, vars: {result: rolled_back}}\n    - {id: a, title: Next}\n",
        &[],
    );
    assert_eq!(trace.state.fields["severity"], serde_json::json!("major"));
    assert_eq!(trace.state.vars["result"], serde_json::json!("rolled_back"));
    assert_eq!(trace.count("DataChanged"), 1);
    assert_eq!(trace.ready, ["a"]);
}
#[test]
fn rollback_result_is_success() {
    let trace = run_case(
        "spec:\n  vars: {result: healthy}\n  steps: [{id: result, set: {vars: {result: rolled_back}}}]\n  outputs: {result: '${{ vars.result }}'}",
        &[],
    );
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.state.result, serde_json::json!("rolled_back"));
    assert_eq!(trace.state, trace.replay());
    assert!(trace.error_codes.is_empty());
}
#[test]
fn initializer_dependencies_run_in_dependency_order() {
    let trace = run_case(
        "spec:\n  inputs: {seed: {type: string, default: initial}}\n  vars: {z: '${{ vars.a }}', a: '${{ inputs.seed }}'}\n  steps: []\n  outputs: {result: '${{ vars.z }}'}",
        &[],
    );
    assert_eq!(trace.state.vars["a"], serde_json::json!("initial"));
    assert_eq!(trace.state.vars["z"], serde_json::json!("initial"));
    assert_eq!(trace.state.result, serde_json::json!("initial"));
    assert_eq!(trace.state, trace.replay());
    assert!(trace.error_codes.is_empty());
}
#[test]
fn assignments_read_one_pre_change_snapshot() {
    let trace = run_case(
        "spec:\n  fields: {severity: {type: string, default: minor}}\n  vars: {result: initial}\n  steps: [{id: change, set: {fields: {severity: major}, vars: {result: '${{ fields.severity }}'}}}]",
        &[],
    );
    assert_eq!(trace.state.fields["severity"], serde_json::json!("major"));
    assert_eq!(trace.state.vars["result"], serde_json::json!("minor"));
    assert_eq!(trace.count("DataChanged"), 1);
    let reads = &trace.step("change").reads;
    assert!(
        reads
            .iter()
            .any(|r| r.origin.path == "fields.severity" && r.origin.record.revision == 1)
    );
}
#[test]
fn failed_assignment_has_no_partial_writes() {
    let trace = run_case(
        "spec:\n  inputs: {missing: {type: string}}\n  fields: {severity: {type: string, default: minor}}\n  vars: {result: initial}\n  steps: [{id: change, set: {fields: {severity: major}, vars: {result: '${{ inputs.missing }}'}}}]",
        &[],
    );
    assert_eq!(trace.error_codes, ["expression.missing_value"]);
    assert_eq!(trace.count("DataChanged"), 0);
    assert_eq!(trace.state.fields["severity"], serde_json::json!("minor"));
    assert_eq!(trace.state.vars["result"], serde_json::json!("initial"));
    assert_eq!(trace.state.revisions["fields"].revision, 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn unrelated_field_updates_preserve_value_revisions() {
    let trace = run_case(
        "spec:\n  fields: {a: {type: integer, default: 1}, b: {type: integer, default: 1}}\n  vars: {result: 0}\n  steps:\n    - {id: change, set: {fields: {a: 2}}}\n    - {id: read, set: {vars: {result: '${{ fields.b }}'}}}\n",
        &[],
    );
    let read = trace
        .step("read")
        .reads
        .iter()
        .find(|r| r.origin.path == "fields.b")
        .unwrap();
    assert_eq!(read.origin.record.revision, 1);
    assert_eq!(read.origin.record.execution_id, trace.state.id.as_str());
    assert_eq!(read.origin.record.step_run_id, None);
    assert!(read.origin.record.iteration_path.is_empty());
}
#[test]
fn runtime_reads_keep_record_identity() {
    let mut input = succeeded("a");
    let Input::Report { outputs, at_ms, .. } = &mut input else {
        unreachable!()
    };
    *outputs = [("code".into(), serde_json::json!(503))].into();
    *at_ms = Some(12_345);
    let trace = run_case(
        "spec:\n  vars: {code: 0}\n  steps:\n    - {id: a, title: Check, outputs: {code: {type: integer, required: true}}}\n    - {id: capture, set: {vars: {code: '${{ steps.a.outputs.code }}'}}}\n  outputs: {result: '${{ vars.code }}', observed: '${{ now() }}', text: 'Code ${{ vars.code }}'}",
        &[input],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let read = trace
        .step("capture")
        .reads
        .iter()
        .find(|r| r.origin.path == "steps.a.outputs.code")
        .unwrap();
    assert_eq!(
        read.origin.record.step_run_id,
        trace.step("a").id.as_ref().map(ToString::to_string)
    );
    assert_eq!(read.origin.record.execution_id, trace.state.id.as_str());
    assert_eq!(
        read.origin.record.revision,
        trace
            .events
            .iter()
            .find(|e| e.event.name() == "StepReported")
            .unwrap()
            .seq
    );
    assert_eq!(trace.state.result, serde_json::json!(503));
    assert_eq!(trace.state.outputs["text"], serde_json::json!("Code 503"));
    let observed: runbook_model::Timestamp = trace.state.outputs["observed"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(observed.timestamp_millis(), 12_345);
    assert_eq!(trace.state, trace.replay());
    let events: Vec<runbook_engine::EventEnvelope> =
        serde_json::from_slice(&serde_json::to_vec(&trace.events).unwrap()).unwrap();
    assert_eq!(events, trace.events);
}
#[test]
fn runtime_whole_values_keep_types_and_optional_absence() {
    let trace = run_case(
        "spec:\n  inputs:\n    count: {type: integer, default: 1.0}\n    rate: {type: number, default: 1}\n    enabled: {type: boolean, default: true}\n    record: {type: object, default: {name: ok}, properties: {name: {type: string, required: true}}}\n  vars: {result: '${{ inputs.record }}'}\n  steps: []\n  outputs: {count: '${{ inputs.count }}', rate: '${{ inputs.rate }}', enabled: '${{ inputs.enabled }}', result: '${{ vars.result }}'}",
        &[],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.outputs["count"].as_i64(), Some(1));
    assert_eq!(trace.state.outputs["rate"].to_string(), "1.0");
    assert_eq!(trace.state.outputs["enabled"], serde_json::json!(true));
    assert_eq!(trace.state.result, serde_json::json!({"name":"ok"}));
    let missing = run_case(
        "spec: {inputs: {optional: {type: string}}, vars: {bad: '${{ inputs.optional }}'}, steps: []}",
        &[],
    );
    assert_eq!(missing.error_codes, ["expression.missing_value"]);
    assert_eq!(missing.count("ExecutionStarted"), 0);
}
#[test]
fn data_replay_consumes_facts_and_rejects_stale_or_partial_batches() {
    use runbook_engine::{Command, Event, EventEnvelope, apply};
    use runbook_model::ActorRef;
    let mut trace = run_case(
        "spec: {vars: {value: initial}, steps: [{id: a, title: Gate}, {id: change, set: {vars: {value: authored}}}], outputs: {result: '${{ vars.value }}'}}",
        &[],
    );
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("a").id.clone().unwrap(),
            report: runbook_engine::WorkReport {
                appended_logs: false,
                files: vec![],
                status: runbook_model::ReportStatus::Succeeded,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        ActorRef::System,
        1,
    );
    trace.append(
        Event::StepReady {
            reads: vec![],
            step_id: "change".into(),
            step_run_id: "sr_00000000000000000000000099".parse().unwrap(),
        },
        ActorRef::System,
        1,
    );
    let Command::ApplyData { change } = runbook_engine::decide(&trace.state).unwrap().remove(0)
    else {
        panic!("assignment");
    };
    let mut stale = change.clone();
    stale.base_sequence -= 1;
    let envelope = EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 1,
        actor: ActorRef::System,
        event: Event::DataChanged { change: stale },
    };
    assert_eq!(
        apply(&trace.state, &envelope).unwrap_err().code(),
        "event.sequence_conflict"
    );
    let mut partial = change.clone();
    partial.vars.clear();
    let envelope = EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 1,
        actor: ActorRef::System,
        event: Event::DataChanged { change: partial },
    };
    assert_eq!(
        apply(&trace.state, &envelope).unwrap_err().code(),
        "step.invalid_assignment"
    );
    // This is an authenticated stored fact, intentionally distinct from the
    // authored RHS. Replay validates its shape but never executes that RHS again.
    let mut recorded = change;
    recorded
        .vars
        .insert("value".into(), serde_json::json!("recorded"));
    trace.append(Event::DataChanged { change: recorded }, ActorRef::System, 1);
    trace.settle();
    assert_eq!(trace.state.result, serde_json::json!("recorded"));
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn memory_audit_does_not_change_data_provenance() {
    let mut trace = run_case(
        "spec: {vars: {result: stable}, steps: [{id: a, title: Task}], outputs: {result: '${{ vars.result }}'}}",
        &[],
    );
    let before = runbook_engine::evaluate_outputs(&trace.state).unwrap();
    let revision = trace.state.data_revision;
    trace.append(
        runbook_engine::Event::MemoryRead {
            step_run_id: trace.step("a").id.clone().unwrap(),
            refs: vec![runbook_model::MemoryReadRef {
                memory_id: "mem_00000000000000000000000001".parse().unwrap(),
                revision: 5,
            }],
        },
        runbook_model::ActorRef::System,
        trace.state.at_ms,
    );
    assert_eq!(trace.state.data_revision, revision);
    assert_eq!(
        runbook_engine::evaluate_outputs(&trace.state).unwrap(),
        before
    );
}
#[test]
fn nullable_findings_are_preserved_as_null() {
    let trace = run_case(
        "spec: {steps: [{id: a, title: Task}], outputs: {result: '${{ steps.a.verdict }}'}}",
        &[succeeded("a")],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert!(trace.state.outputs.contains_key("result"));
    assert_eq!(trace.state.result, serde_json::Value::Null);
}
#[test]
fn start_admission_rejects_deep_values_before_copying() {
    let trace = run_case("spec: {steps: [{id: a, title: Task}]}", &[]);
    let initial = runbook_engine::ExecutionState::new(
        trace.state.id.clone(),
        std::rc::Rc::new(runbook_spec::restore(trace.state.compiled().snapshot_bytes()).unwrap()),
    );
    let mut context = runbook_engine::StartContext::default();
    let mut value = serde_json::json!(0);
    for _ in 0..20_000 {
        value = serde_json::Value::Array(vec![value]);
    }
    context.inputs.insert("unknown".into(), value);
    let result = runbook_engine::prepare_start(&initial, &context, 0);
    let mut value = context.inputs.remove("unknown").unwrap();
    while let serde_json::Value::Array(mut children) = value {
        value = children.pop().unwrap();
    }
    assert_eq!(result.unwrap_err().code(), "data.depth_limit");
}
#[test]
fn start_and_runtime_admit_binding_sizes_before_copies() {
    use runbook_engine::{ExecutionState, StartContext, evaluate_outputs, prepare_start};
    let trace = run_case(
        "spec: {inputs: {value: {type: string, default: ok}}, steps: [{id: a, title: Task}], outputs: {result: '${{ inputs.value }}'}}",
        &[],
    );
    let initial = ExecutionState::new(
        trace.state.id.clone(),
        std::rc::Rc::new(runbook_spec::restore(trace.state.compiled().snapshot_bytes()).unwrap()),
    );
    let mut context = StartContext::default();
    context
        .inputs
        .insert("value".into(), serde_json::json!("x".repeat(65_537)));
    assert_eq!(
        prepare_start(&initial, &context, 0).unwrap_err().code(),
        "expression.binding_limit"
    );
    context.inputs.clear();
    context.inputs.insert(
        "unknown".into(),
        serde_json::Value::Array(vec![serde_json::Value::Null; 100_001]),
    );
    assert_eq!(
        prepare_start(&initial, &context, 0).unwrap_err().code(),
        "expression.binding_limit"
    );
    let mut state = trace.state;
    state
        .inputs
        .insert("value".into(), serde_json::json!("x".repeat(65_537)));
    assert_eq!(
        evaluate_outputs(&state).unwrap_err().code(),
        "expression.binding_limit"
    );
}
#[test]
fn start_ignores_unselected_queued_data_and_bindings_borrow_provenance() {
    use runbook_engine::{ExecutionState, StartContext, evaluate_outputs, prepare_start};
    let trace = run_case(
        "spec: {vars: {value: stable}, steps: [{id: a, title: Task}], outputs: {result: '${{ vars.value }}'}}",
        &[],
    );
    let mut initial = ExecutionState::new(
        trace.state.id.clone(),
        std::rc::Rc::new(runbook_spec::restore(trace.state.compiled().snapshot_bytes()).unwrap()),
    );
    let mut value = serde_json::json!(0);
    for _ in 0..20_000 {
        value = serde_json::Value::Array(vec![value]);
    }
    initial.inputs.insert("ignored".into(), value);
    let event = prepare_start(&initial, &StartContext::default(), 0);
    let mut value = initial.inputs.remove("ignored").unwrap();
    while let serde_json::Value::Array(mut children) = value {
        value = children.pop().unwrap();
    }
    assert!(event.is_ok());
    let mut state = trace.state;
    state.revisions.get_mut("vars.value").unwrap().execution_id = "x".repeat(129);
    assert_eq!(
        evaluate_outputs(&state).unwrap_err().code(),
        "expression.binding_limit"
    );
    // An unused provenance entry isn't visited or copied into a binding snapshot.
    state.revisions.remove("vars.value");
    let mut unused = state.revisions["vars"].clone();
    unused.execution_id = "x".repeat(1_000_000);
    state.revisions.insert("not.a.runtime.path".into(), unused);
    assert_eq!(
        evaluate_outputs(&state).unwrap().values["result"],
        serde_json::json!("stable")
    );
}

#[test]
fn expressions_expose_the_run_page_and_api_address() {
    let url = "https://runs.example.test/v1/executions/exe_00000000000000000000000001";
    let trace = run_document_with_start(
        r#"
metadata: {id: run-links}
spec:
  steps: []
  outputs:
    api: '${{ execution.url }}'
    page: '${{ execution.web_url }}'
"#,
        &[],
        runbook_spec::CompileContext::default(),
        runbook_engine::StartContext {
            url: url.into(),
            ..Default::default()
        },
    );
    assert_eq!(trace.state.outputs["api"], url);
    assert_eq!(
        trace.state.outputs["page"],
        "https://runs.example.test/x/exe_00000000000000000000000001"
    );
    assert_eq!(trace.state, trace.replay());
}
