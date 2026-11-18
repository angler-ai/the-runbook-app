//! LANG9: a signal wait may declare `outputs`, the schema its payload must
//! match. A valid payload becomes the step's outputs; an invalid one is
//! refused with field errors and the step keeps waiting. Waits without a
//! schema behave as before.
mod support;
use runbook_engine::*;
use runbook_model::StepStatus;
use serde_json::json;
use support::*;

const SPEC: &str = "spec: {steps: [{id: connect, wait: {signal: shopify_connected, outputs: {store_id: {type: string, required: true}, plan: {type: string}}}}, {id: next, title: Next, instructions: \"Use ${{ steps.connect.outputs.store_id }}\"}]}";

fn signal(trace: &Trace, payload: serde_json::Value) -> WaitInput {
    WaitInput::Signal {
        execution_id: trace.state.id.clone(),
        signal_id: "sig1".into(),
        name: "shopify_connected".into(),
        payload: serde_json::from_value(payload).unwrap(),
        at_ms: 5,
    }
}

#[test]
fn an_invalid_payload_is_refused_with_field_errors_and_the_step_keeps_waiting() {
    let trace = run_case(SPEC, &[]);
    let error = advance_wait(&trace.state, &signal(&trace, json!({"plan": "pro"}))).unwrap_err();
    assert_eq!(error.code(), "signal.invalid_payload");
    let paths: Vec<_> = error.errors().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["payload.store_id"]);
    assert_eq!(trace.step("connect").status, StepStatus::Waiting);

    let wrong_type =
        advance_wait(&trace.state, &signal(&trace, json!({"store_id": 7}))).unwrap_err();
    assert_eq!(wrong_type.code(), "signal.invalid_payload");
}

#[test]
fn a_valid_payload_becomes_the_steps_declared_outputs() {
    let mut trace = run_case(SPEC, &[]);
    let commands = advance_wait(
        &trace.state,
        &signal(
            &trace,
            json!({"store_id": "shop_42", "plan": "pro", "extra": true}),
        ),
    )
    .unwrap();
    for command in commands {
        let Command::ConsumeSignal { signal } = command else {
            unreachable!()
        };
        trace.append(
            Event::SignalReceived { signal },
            runbook_model::ActorRef::System,
            5,
        );
    }
    trace.settle();
    let connect = trace.step("connect");
    assert_eq!(connect.status, StepStatus::Succeeded);
    // Only the declared fields; `extra` is dropped.
    assert_eq!(
        serde_json::to_value(&connect.outputs).unwrap(),
        json!({"store_id": "shop_42", "plan": "pro"})
    );
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_wait_without_a_schema_accepts_any_payload_and_sets_no_outputs() {
    let mut trace = run_case(
        "spec: {steps: [{id: gate, wait: {signal: deploy}}, {id: next, title: Next}]}",
        &[],
    );
    let input = WaitInput::Signal {
        execution_id: trace.state.id.clone(),
        signal_id: "s".into(),
        name: "deploy".into(),
        payload: serde_json::from_value(json!({"anything": [1, 2]})).unwrap(),
        at_ms: 1,
    };
    for command in advance_wait(&trace.state, &input).unwrap() {
        let Command::ConsumeSignal { signal } = command else {
            unreachable!()
        };
        trace.append(
            Event::SignalReceived { signal },
            runbook_model::ActorRef::System,
            1,
        );
    }
    trace.settle();
    assert_eq!(trace.step("gate").status, StepStatus::Succeeded);
    assert!(trace.step("gate").outputs.is_empty());
}

#[test]
fn a_forged_delivery_with_an_invalid_payload_is_refused_on_replay() {
    let mut trace = run_case(SPEC, &[]);
    // Built from a valid payload, then tampered with before it is recorded.
    let command = advance_wait(
        &trace.state,
        &signal(&trace, json!({"store_id": "shop_42"})),
    )
    .unwrap()
    .remove(0);
    let Command::ConsumeSignal { mut signal } = command else {
        unreachable!()
    };
    signal.payload = serde_json::from_value(json!({"plan": "pro"})).unwrap();
    trace.append(
        Event::SignalReceived { signal },
        runbook_model::ActorRef::System,
        5,
    );
    assert_eq!(trace.error_codes, ["signal.invalid_payload"]);
    assert_eq!(trace.step("connect").status, StepStatus::Waiting);
}
