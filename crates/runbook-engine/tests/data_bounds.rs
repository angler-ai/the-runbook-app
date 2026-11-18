//! Exact boundaries of the engine's data admission (`data.rs`: `guard_copy`
//! and `CopyAdmission`), plus forged data facts.
//!
//! Each ceiling is checked at the limit and one past it. Start data under an
//! undeclared key is admitted for size before the schema rejects the key, so
//! `execution.invalid_data` means "within the bound" and
//! `expression.binding_limit` / `data.depth_limit` mean "over it".
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, Data};
use serde_json::{Value, json};
use std::rc::Rc;
use support::*;

const WITHIN: &str = "execution.invalid_data";
const OVER: &str = "expression.binding_limit";

fn fresh() -> ExecutionState {
    fresh_for(&run_case("spec: {steps: [{id: a, title: Task}]}", &[]))
}
fn fresh_for(trace: &Trace) -> ExecutionState {
    ExecutionState::new(
        trace.state.id.clone(),
        Rc::new(runbook_spec::restore(trace.state.compiled().snapshot_bytes()).unwrap()),
    )
}
fn start_with(key: &str, value: Value) -> &'static str {
    let mut context = StartContext::default();
    context.inputs.insert(key.into(), value);
    match prepare_start(&fresh(), &context, 0) {
        Ok(_) => "ok",
        Err(error) => error.code(),
    }
}
/// `levels` arrays wrapped around a scalar.
fn nested(levels: usize) -> Value {
    let mut value = json!(0);
    for _ in 0..levels {
        value = Value::Array(vec![value]);
    }
    value
}

#[test]
fn string_and_key_length_ceiling_is_65_536_bytes() {
    assert_eq!(start_with("u", json!("x".repeat(65_536))), WITHIN);
    assert_eq!(start_with("u", json!("x".repeat(65_537))), OVER);
    // Multi-byte text is measured in bytes, not characters.
    assert_eq!(start_with("u", json!("é".repeat(32_768))), WITHIN);
    assert_eq!(
        start_with("u", json!(format!("{}x", "é".repeat(32_768)))),
        OVER
    );
    assert_eq!(start_with(&"k".repeat(65_536), json!(0)), WITHIN);
    assert_eq!(start_with(&"k".repeat(65_537), json!(0)), OVER);
}

#[test]
fn array_length_ceiling_is_100_000_items() {
    assert_eq!(
        start_with("u", Value::Array(vec![Value::Null; 100_000])),
        WITHIN
    );
    assert_eq!(
        start_with("u", Value::Array(vec![Value::Null; 100_001])),
        OVER
    );
}

#[test]
fn binding_depth_ceiling_is_32_levels_and_copy_depth_is_128() {
    // The top-level value is level 1, so 31 wrapping arrays put the scalar at 32.
    assert_eq!(start_with("u", nested(31)), WITHIN);
    assert_eq!(start_with("u", nested(32)), OVER);
    // The recursive-copy guard runs first and has its own, deeper ceiling.
    assert_eq!(start_with("u", nested(127)), OVER);
    assert_eq!(start_with("u", nested(128)), "data.depth_limit");
}

#[test]
fn node_count_ceiling_is_262_144_values() {
    // One outer array, three inner arrays and their elements.
    let build = |elements: usize| {
        let mut inner = vec![];
        let mut left = elements;
        for _ in 0..3 {
            let n = left.min(100_000);
            inner.push(Value::Array(vec![Value::Null; n]));
            left -= n;
        }
        Value::Array(inner)
    };
    assert_eq!(start_with("u", build(262_140)), WITHIN);
    assert_eq!(start_with("u", build(262_141)), OVER);
}

#[test]
fn total_text_ceiling_is_8_mib_including_keys() {
    // key "u" (1 byte) + 127 full strings + one string one byte short.
    let build = |last: usize| {
        let mut items = vec![json!("x".repeat(65_536)); 127];
        items.push(json!("x".repeat(last)));
        Value::Array(items)
    };
    assert_eq!(1 + 127 * 65_536 + 65_535, 8_388_608);
    assert_eq!(start_with("u", build(65_535)), WITHIN);
    assert_eq!(start_with("u", build(65_536)), OVER);
}

#[test]
fn tag_count_ceiling_is_100_000() {
    let start = |count: usize| {
        let context = StartContext {
            tags: vec![String::new(); count],
            ..Default::default()
        };
        prepare_start(&fresh(), &context, 0).map(|_| ())
    };
    assert!(start(100_000).is_ok());
    assert_eq!(start(100_001).unwrap_err().code(), OVER);
}

fn envelope(trace: &Trace, event: Event) -> EventEnvelope {
    EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: trace.state.at_ms,
        actor: ActorRef::System,
        event,
    }
}

/// Stored start facts are not re-evaluated on replay, so their shape is the
/// only guard: every initial variable must be present with its checked type.
#[test]
fn forged_start_vars_must_match_the_checked_record() {
    let trace = run_case(
        "spec: {vars: {count: 0, label: x}, steps: [{id: a, title: Task}]}",
        &[],
    );
    let started = trace.events[0].clone();
    let Event::ExecutionStarted {
        data,
        engine_version,
    } = &started.event
    else {
        panic!("first event starts the run")
    };
    let forge = |edit: &dyn Fn(&mut Data)| {
        let mut forged = data.clone();
        edit(&mut forged.vars);
        let envelope = EventEnvelope {
            event: Event::ExecutionStarted {
                data: forged,
                engine_version: *engine_version,
            },
            ..started.clone()
        };
        apply(&fresh_for(&trace), &envelope)
            .map(|_| ())
            .map_err(|e| e.code())
    };
    assert_eq!(forge(&|_| {}), Ok(()));
    let edits: [fn(&mut Data); 3] = [
        |v| {
            v.insert("count".into(), json!("seven"));
        },
        |v| {
            v.remove("label");
        },
        |v| {
            v.insert("ghost".into(), json!(1));
        },
    ];
    for edit in edits {
        assert_eq!(forge(&edit), Err("expression.binding_error"));
    }
}

/// A recorded assignment is consumed without rerunning its RHS, so a value
/// of the wrong type must be refused rather than installed.
#[test]
fn forged_assignment_values_must_match_the_variable_type() {
    let mut open = run_case(
        "spec: {vars: {count: 0}, steps: [{id: gate, title: Gate}, {id: change, set: {vars: {count: 1}}}]}",
        &[],
    );
    open.append(
        Event::StepReported {
            step_run_id: open.step("gate").id.clone().unwrap(),
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status: runbook_model::ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::new(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        ActorRef::System,
        1,
    );
    open.append(
        Event::StepReady {
            reads: vec![],
            step_id: "change".into(),
            step_run_id: "sr_00000000000000000000000099".parse().unwrap(),
        },
        ActorRef::System,
        1,
    );
    assert!(open.error_codes.is_empty(), "{:?}", open.error_codes);
    let Command::ApplyData { change } = decide(&open.state).unwrap().remove(0) else {
        panic!("assignment is next")
    };
    for bad in [json!("seven"), json!(1.5), Value::Null, json!([1])] {
        let mut forged = change.clone();
        forged.vars.insert("count".into(), bad.clone());
        let result = apply(
            &open.state,
            &envelope(&open, Event::DataChanged { change: forged }),
        );
        assert_eq!(
            result.map(|_| ()).unwrap_err().code(),
            "expression.binding_error",
            "{bad}"
        );
    }
    // The genuine fact still applies afterwards.
    let state = apply(&open.state, &envelope(&open, Event::DataChanged { change })).unwrap();
    assert_eq!(state.vars["count"], json!(1));
}

#[test]
fn metadata_edits_are_validated_and_bounded() {
    let trace = run_case(
        "spec: {fields: {sev: {type: enum, options: [a, b], default: a}}, steps: [{id: gate, title: Gate}]}",
        &[],
    );
    let edit = |fields: Value, title: Option<String>| {
        let event = Event::ExecutionMetadataUpdated {
            title,
            tags: None,
            fields: fields
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        apply(&trace.state, &envelope(&trace, event))
            .map(|s| (s.fields["sev"].clone(), s.title.len()))
            .map_err(|e| e.code())
    };
    assert_eq!(edit(json!({"sev": "b"}), None), Ok((json!("b"), 4)));
    assert_eq!(edit(json!({"sev": "z"}), None), Err("data.invalid_fields"));
    assert_eq!(edit(json!({"nope": 1}), None), Err("data.invalid_fields"));
    assert_eq!(
        edit(json!({}), Some("t".repeat(65_536))),
        Ok((json!("a"), 65_536))
    );
    assert_eq!(edit(json!({}), Some("t".repeat(65_537))), Err(OVER));

    let done = run_case(
        "spec: {fields: {sev: {type: enum, options: [a, b], default: a}}, steps: [{id: gate, title: Gate}]}",
        &[succeeded("gate")],
    );
    let event = Event::ExecutionMetadataUpdated {
        title: None,
        tags: None,
        fields: [("sev".into(), json!("b"))].into(),
    };
    assert_eq!(
        apply(&done.state, &envelope(&done, event.clone()))
            .unwrap_err()
            .code(),
        "execution.closed"
    );
    assert_eq!(
        apply(&fresh(), &envelope(&done, event).with_seq(1))
            .unwrap_err()
            .code(),
        "execution.not_started"
    );
}

trait WithSeq {
    fn with_seq(self, seq: u64) -> Self;
}
impl WithSeq for EventEnvelope {
    fn with_seq(mut self, seq: u64) -> Self {
        self.seq = seq;
        self
    }
}
