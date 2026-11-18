mod support;
use support::*;
const REGIONS: &str = "spec: {inputs: {regions: {type: list, items: {type: string}, default: [east, west, eu]}}, steps: [{id: regions, foreach: {items: '${{ inputs.regions }}', as: region, key: '${{ region }}', max_items: 3, concurrency: 2, steps: [{id: check, title: Check, check: true}]}}]}";
#[test]
fn foreach_respects_concurrency() {
    let trace = run_case(REGIONS, &[]);
    assert_eq!(trace.ready, ["regions[east].check", "regions[west].check"]);
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}
fn checked(step: &str) -> Input {
    let mut input = succeeded(step);
    let Input::Report { verdict, .. } = &mut input else {
        unreachable!()
    };
    *verdict = Some(runbook_model::Verdict::Pass);
    input
}
#[test]
fn foreach_releases_next_slot() {
    let trace = run_case(REGIONS, &[checked("regions[east].check")]);
    assert_eq!(trace.ready, ["regions[west].check", "regions[eu].check"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn foreach_rejects_duplicate_keys() {
    let trace = run_case(&REGIONS.replace("[east, west, eu]", "[east, east]"), &[]);
    assert_eq!(trace.error_codes, ["foreach.duplicate_key"]);
    assert_eq!(trace.count("StepReady"), 0);
    assert_eq!(trace.count("ForeachStarted"), 0);
    assert_eq!(trace.count("ExecutionFaulted"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn foreach_finishes_with_aggregate_findings_and_ordered_items() {
    let trace = run_case(
        REGIONS,
        &[
            checked("regions[west].check"),
            checked("regions[east].check"),
            checked("regions[eu].check"),
        ],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(
        trace.step("regions").verdict,
        Some(runbook_model::Verdict::Pass)
    );
    assert_eq!(trace.count("ForeachStarted"), 1);
    assert_eq!(trace.count("IterationAdmitted"), 3);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn empty_foreach_and_oversize_items_are_bounded() {
    let empty = run_case(&REGIONS.replace("[east, west, eu]", "[]"), &[]);
    assert_eq!(
        empty.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(empty.step("regions").verdict, None);
    assert_eq!(empty.count("IterationAdmitted"), 0);
    let oversized = run_case(
        &REGIONS.replace("[east, west, eu]", "[east, west, eu, extra]"),
        &[],
    );
    assert_eq!(oversized.error_codes, ["foreach.max_items"]);
    assert_eq!(oversized.count("ExecutionFaulted"), 1);
    assert_eq!(oversized.count("StepReady"), 0);
    assert_eq!(
        oversized.state.status,
        runbook_model::ExecutionStatus::Failed
    );
}
#[test]
fn item_alias_and_index_drive_guards_and_assignments() {
    let trace = run_case("spec: {inputs: {regions: {type: list, items: {type: string}, default: [east, west]}}, vars: {seen: none, ordinal: 0}, steps: [{id: regions, foreach: {items: '${{ inputs.regions }}', as: region, key: '${{ region }}', max_items: 2, concurrency: 2, steps: [{id: record, if: '${{ region == \'west\' }}', set: {vars: {seen: '${{ region }}', ordinal: '${{ index }}'}}}]}}], outputs: {result: '${{ vars.seen }}'}}".replace("'${{ region == 'west' }}'", "\"${{ region == 'west' }}\"").as_str(), &[]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.result, serde_json::json!("west"));
    assert_eq!(trace.state.vars["ordinal"], serde_json::json!(1));
    assert_eq!(
        trace.step("regions[east].record").status,
        runbook_model::StepStatus::Skipped
    );
    let read = trace
        .step("regions[west].record")
        .reads
        .iter()
        .find(|r| r.origin.path == "region")
        .unwrap();
    assert_eq!(read.origin.record.iteration_path[0].ordinal, 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn current_iteration_step_outputs_and_history_have_distinct_origins() {
    let spec = "spec:\n  inputs: {regions: {type: list, items: {type: string}, default: [east, west]}}\n  vars: {seen: none}\n  steps:\n    - id: regions\n      foreach:\n        items: '${{ inputs.regions }}'\n        as: region\n        key: '${{ region }}'\n        max_items: 2\n        concurrency: 2\n        steps:\n          - {id: inspect, title: Inspect, outputs: {label: {type: string, required: true}}}\n          - {id: save, set: {vars: {seen: '${{ steps.inspect.outputs.label }}'}}}\n  outputs: {result: '${{ steps.regions.items[0].inspect.outputs.label }}'}\n";
    let mut east = succeeded("regions[east].inspect");
    let Input::Report { outputs, .. } = &mut east else {
        unreachable!()
    };
    outputs.insert("label".into(), serde_json::json!("east-result"));
    let mut west = succeeded("regions[west].inspect");
    let Input::Report { outputs, .. } = &mut west else {
        unreachable!()
    };
    outputs.insert("label".into(), serde_json::json!("west-result"));
    let trace = run_case(spec, &[west, east]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.result, serde_json::json!("east-result"));
    for (key, ordinal) in [("east", 0), ("west", 1)] {
        let read = trace
            .step(&format!("regions[{key}].save"))
            .reads
            .iter()
            .find(|r| r.origin.path == "steps.inspect.outputs.label")
            .unwrap();
        assert_eq!(read.origin.record.iteration_path[0].ordinal, ordinal);
        assert_eq!(
            read.origin.record.step_run_id.as_deref(),
            trace
                .step(&format!("regions[{key}].inspect"))
                .id
                .as_ref()
                .map(|id| id.as_str())
        );
    }
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn nested_foreach_has_independent_instances_and_outer_aliases() {
    let spec = "spec:\n  inputs:\n    regions: {type: list, items: {type: string}, default: [east, west]}\n    zones: {type: list, items: {type: string}, default: [a, b]}\n  vars: {seen: none}\n  steps:\n    - id: regions\n      foreach:\n        items: '${{ inputs.regions }}'\n        as: region\n        key: '${{ region }}'\n        max_items: 2\n        concurrency: 2\n        steps:\n          - id: zones\n            foreach:\n              items: '${{ inputs.zones }}'\n              as: zone\n              key: '${{ zone }}'\n              max_items: 2\n              concurrency: 1\n              steps:\n                - {id: save, set: {vars: {seen: '${{ region }}-${{ zone }}'}}}\n  outputs: {result: '${{ vars.seen }}'}\n";
    let trace = run_case(spec, &[]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("ForeachStarted"), 3);
    assert_eq!(trace.count("DataChanged"), 4);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.step("regions[west].zones[b].save").scope.len(), 2);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn snapshot_facts_reject_wrong_closed_item_types() {
    use runbook_engine::{Event, ForeachSnapshot};
    use runbook_model::ActorRef;
    let mut trace = run_case(REGIONS, &[]);
    trace.events.truncate(1);
    trace.state = trace.replay();
    let before = trace.state.clone();
    trace.append(
        Event::ForeachStarted {
            step_id: "regions".into(),
            step_run_id: "sr_00000000000000000000000100".parse().unwrap(),
            snapshot: Box::new(ForeachSnapshot {
                base_sequence: trace.state.sequence,
                items: serde_json::json!([42]),
                keys: vec!["42".into()],
                reads: vec![],
                admitted: 0,
            }),
        },
        ActorRef::System,
        0,
    );
    assert_eq!(trace.error_codes, ["foreach.invalid_snapshot"]);
    assert_eq!(trace.state, before);
}
#[test]
fn snapshot_is_frozen_and_admission_cannot_duplicate_or_exceed_slots() {
    use runbook_engine::Event;
    use runbook_model::ActorRef;
    let mut trace = run_case(REGIONS, &[]);
    let parent = trace.step("regions").id.clone().unwrap();
    let before = trace.state.clone();
    for index in [0, 2] {
        trace.append(
            Event::IterationAdmitted {
                foreach_run_id: parent.clone(),
                index,
            },
            ActorRef::System,
            0,
        );
        assert_eq!(
            trace.error_codes.last().unwrap(),
            "foreach.invalid_admission"
        );
        assert_eq!(trace.state, before);
    }
    // Even if an upstream record is amended, admitted items remain the recorded snapshot.
    trace
        .state
        .inputs
        .insert("regions".into(), serde_json::json!(["changed"]));
    let snapshot = trace.step("regions").foreach.as_ref().unwrap();
    assert_eq!(snapshot.items, serde_json::json!(["east", "west", "eu"]));
    assert_eq!(snapshot.keys, ["east", "west", "eu"]);
}
#[test]
fn object_items_and_escaped_keys_preserve_distinct_paths() {
    let trace = run_case(
        "spec: {inputs: {items: {type: list, items: {type: object, properties: {name: {type: string, required: true}}}, default: [{name: 'a]'}, {name: 'a%5D'}]}}, vars: {seen: none}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', as: item, key: '${{ item.name }}', max_items: 2, concurrency: 2, steps: [{id: save, set: {vars: {seen: '${{ item.name }}'}}}]}}]}",
        &[],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(
        trace.step("each[a%5D].save").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_eq!(
        trace.step("each[a%255D].save").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_ne!(
        trace.step("each[a%5D].save").id,
        trace.step("each[a%255D].save").id
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn foreach_snapshot_storage_keeps_exact_json_and_literal_number_marker() {
    use runbook_engine::{Event, ForeachSnapshot};
    let items = serde_json::json!([{"$serde_json::private::Number": "1.20"}, 1.0]);
    let event = Event::ForeachStarted {
        step_id: "regions".into(),
        step_run_id: "sr_00000000000000000000000100".parse().unwrap(),
        snapshot: Box::new(ForeachSnapshot {
            base_sequence: 1,
            items,
            keys: vec!["a".into(), "b".into()],
            reads: vec![],
            admitted: 0,
        }),
    };
    let decoded: Event = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
    assert_eq!(event, decoded);
}
#[test]
fn deeply_nested_snapshot_is_rejected_before_copy() {
    use runbook_engine::{Event, ForeachSnapshot};
    use runbook_model::ActorRef;
    use serde_json::Value;
    let mut trace = run_case(REGIONS, &[]);
    trace.events.truncate(1);
    trace.state = trace.replay();
    let mut value = Value::Null;
    for _ in 0..20_000 {
        value = Value::Array(vec![value]);
    }
    let event = runbook_engine::EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 0,
        actor: ActorRef::System,
        event: Event::ForeachStarted {
            step_id: "regions".into(),
            step_run_id: "sr_00000000000000000000000100".parse().unwrap(),
            snapshot: Box::new(ForeachSnapshot {
                base_sequence: trace.state.sequence,
                items: value,
                keys: vec!["x".into()],
                reads: vec![],
                admitted: 0,
            }),
        },
    };
    assert_eq!(
        runbook_engine::apply(&trace.state, &event)
            .unwrap_err()
            .code(),
        "expression.binding_limit"
    );
    let Event::ForeachStarted { mut snapshot, .. } = event.event else {
        unreachable!()
    };
    let mut pending = vec![std::mem::take(&mut snapshot.items)];
    while let Some(value) = pending.pop() {
        if let Value::Array(values) = value {
            pending.extend(values);
        }
    }
}
#[test]
fn parallel_controls_are_scoped_to_each_iteration() {
    let spec = "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, west]}}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', as: item, key: '${{ item }}', max_items: 2, concurrency: 2, steps: [{id: both, parallel: [{id: a, title: A}, {id: b, title: B}]}]}}]}";
    let trace = run_case(
        spec,
        &[succeeded("each[east].a"), succeeded("each[east].b")],
    );
    assert_eq!(trace.ready, ["each[west].a", "each[west].b"]);
    assert_eq!(
        trace.step("each[east].both").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_eq!(
        trace.step("each[west].both").status,
        runbook_model::StepStatus::Ready
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn outside_references_use_last_input_iteration_not_last_report() {
    let spec = "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, west]}}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 2, concurrency: 2, steps: [{id: inspect, title: Inspect, outputs: {label: {type: string, required: true}}}]}}], outputs: {result: '${{ steps.inspect.outputs.label }}'}}";
    let mut east = succeeded("each[east].inspect");
    let Input::Report { outputs, .. } = &mut east else {
        unreachable!()
    };
    outputs.insert("label".into(), serde_json::json!("east"));
    let mut west = succeeded("each[west].inspect");
    let Input::Report { outputs, .. } = &mut west else {
        unreachable!()
    };
    outputs.insert("label".into(), serde_json::json!("west"));
    let trace = run_case(spec, &[west, east]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.result, serde_json::json!("west"));
    assert_eq!(
        trace.state.output_reads[0].origin.record.iteration_path[0].ordinal,
        1
    );
}
#[test]
fn pending_current_iteration_hides_prior_outputs() {
    let spec = "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, west]}}, vars: {seen: none}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 2, concurrency: 1, steps: [{id: copy, if: \"${{ item == 'west' }}\", set: {vars: {seen: '${{ steps.inspect.outputs.label }}'}}}, {id: inspect, title: Inspect, outputs: {label: {type: string, required: true}}}]}}]}";
    let mut east = succeeded("each[east].inspect");
    let Input::Report { outputs, .. } = &mut east else {
        unreachable!()
    };
    outputs.insert("label".into(), serde_json::json!("east"));
    let trace = run_case(spec, &[east]);
    assert_eq!(trace.error_codes, ["expression.missing_value"]);
    assert_eq!(trace.state.vars["seen"], serde_json::json!("none"));
    assert_eq!(trace.count("DataChanged"), 0);
}
#[test]
fn foreach_cannot_be_readied_without_its_snapshot() {
    use runbook_engine::Event;
    let mut trace = run_case(REGIONS, &[]);
    trace.events.truncate(1);
    trace.state = trace.replay();
    let before = trace.state.clone();
    trace.append(
        Event::StepReady {
            step_id: "regions".into(),
            step_run_id: "sr_00000000000000000000000100".parse().unwrap(),
            reads: vec![],
        },
        runbook_model::ActorRef::System,
        0,
    );
    assert_eq!(trace.error_codes, ["foreach.snapshot_required"]);
    assert_eq!(trace.state, before);
}
#[test]
fn protocol_failure_fails_foreach_after_admitted_work_finishes() {
    let spec = "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, west]}}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 2, concurrency: 2, steps: [{id: a, title: A}]}}]}";
    let mut failure = succeeded("each[east].a");
    let Input::Report { status, .. } = &mut failure else {
        unreachable!()
    };
    *status = runbook_model::ReportStatus::Failed;
    let trace = run_case(spec, &[failure.clone(), succeeded("each[west].a")]);
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(trace.step("each").status, runbook_model::StepStatus::Failed);
    assert_eq!(trace.step("each").verdict, None);
    assert_eq!(trace.state, trace.replay());
    let continued = run_case(
        &spec.replace(
            "id: each, foreach",
            "id: each, on_failure: continue, foreach",
        ),
        &[failure, succeeded("each[west].a")],
    );
    assert!(continued.error_codes.is_empty());
    assert_eq!(
        continued.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(
        continued.step("each").status,
        runbook_model::StepStatus::Failed
    );
}
#[test]
fn public_report_validator_accepts_a_scoped_step_instance() {
    let trace = run_case(REGIONS, &[]);
    let record = trace.step("regions[east].check");
    let request = runbook_model::ReportRequest {
        step_run_id: record.id.clone().unwrap(),
        claim: runbook_model::ClaimRef {
            id: "clm_00000000000000000000000001".parse().unwrap(),
            generation: 1.try_into().unwrap(),
        },
        status: runbook_model::ReportStatus::Succeeded,
        verdict: Some(runbook_model::Verdict::Pass),
        outputs: Default::default(),
        summary: None,
        evidence: vec![],
        logs: vec![],
    };
    let validated = runbook_engine::validate_report(
        trace.state.node(&record.step_id).unwrap(),
        &request,
        record,
    )
    .unwrap();
    let event = runbook_engine::EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 0,
        actor: runbook_model::ActorRef::System,
        event: validated.into_event(),
    };
    let next = runbook_engine::apply(&trace.state, &event).unwrap();
    assert_eq!(
        next.steps
            .iter()
            .find(|s| s.step_id == "regions[east].check")
            .unwrap()
            .status,
        runbook_model::StepStatus::Succeeded
    );
}
