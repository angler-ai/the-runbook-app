//! LANG1 (later-plans rule 1): `loop.while`. Like `until` it is tested after
//! each iteration's children (PRD §5 "runs children, then checks `until` (or
//! `while`)"), so the body always runs at least once; the loop stops,
//! `satisfied`, when the condition is false, and is `exhausted` (a `fail`
//! finding) when it is still true after `max_iterations`. The check is
//! recorded as a `LoopChecked` fact, so replay never re-evaluates it. Pinned
//! as a conformance trace (`conformance/execution/loop-while.json`).
mod support;
use runbook_model::{ExecutionStatus, StepStatus, Verdict};
use support::*;

const SPEC: &str = "spec:\n  steps:\n    - id: poll\n      loop:\n        while: \"${{ !steps.probe.outputs.healthy }}\"\n        max_iterations: 3\n        delay: 1m\n        steps:\n          - id: probe\n            title: Probe the service\n            check: true\n            outputs:\n              healthy: {type: boolean, required: true}\n  outputs:\n    satisfied: \"${{ steps.poll.satisfied }}\"\n    exhausted: \"${{ steps.poll.exhausted }}\"\n";

fn probe(iteration: u32, healthy: bool, at_ms: i64) -> Input {
    let mut report = succeeded(&format!("poll#{iteration}.probe"));
    let Input::Report {
        outputs,
        verdict,
        at_ms: at,
        ..
    } = &mut report
    else {
        unreachable!()
    };
    outputs.insert("healthy".into(), serde_json::json!(healthy));
    *verdict = Some(if healthy {
        Verdict::Pass
    } else {
        Verdict::Fail
    });
    *at = Some(at_ms);
    report
}

/// Unhealthy, then healthy after the delay: two iterations, satisfied.
fn recovered() -> Trace {
    run_case(
        SPEC,
        &[
            probe(1, false, 10),
            Input::Tick { at_ms: 60_010 },
            probe(2, true, 60_020),
        ],
    )
}

#[test]
fn while_repeats_until_its_condition_is_false() {
    let started = run_case(SPEC, &[]);
    assert_eq!(started.ready, ["poll#1.probe"]);
    // The first check holds (still unhealthy): the delay timer waits.
    let waiting = run_case(SPEC, &[probe(1, false, 10)]);
    assert!(waiting.ready.is_empty());
    assert_eq!(waiting.step("poll").status, StepStatus::Waiting);
    let trace = recovered();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("LoopIterationStarted"), 2);
    assert_eq!(trace.count("LoopChecked"), 2);
    let poll = trace.step("poll");
    assert!(poll.satisfied);
    assert!(!poll.exhausted);
    assert_eq!(poll.status, StepStatus::Succeeded);
    // The first iteration's finding stays in the aggregate (PRD §5).
    assert_eq!(poll.verdict, Some(Verdict::Fail));
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_false_condition_after_the_first_iteration_stops_at_once() {
    let trace = run_case(SPEC, &[probe(1, true, 10)]);
    assert_eq!(trace.count("LoopIterationStarted"), 1);
    assert!(trace.step("poll").satisfied);
    assert_eq!(trace.step("poll").verdict, Some(Verdict::Pass));
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_condition_still_true_at_the_cap_exhausts_the_loop() {
    let trace = run_case(
        SPEC,
        &[
            probe(1, false, 10),
            Input::Tick { at_ms: 60_010 },
            probe(2, false, 60_020),
            Input::Tick { at_ms: 120_020 },
            probe(3, false, 120_030),
        ],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("LoopIterationStarted"), 3);
    let poll = trace.step("poll");
    assert!(poll.exhausted);
    assert!(!poll.satisfied);
    // Exhaustion is a finding, not an execution failure (PRD §5).
    assert_eq!(poll.status, StepStatus::Succeeded);
    assert_eq!(poll.verdict, Some(Verdict::Fail));
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(
        trace.state.outputs.get("exhausted"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn the_last_allowed_check_takes_precedence_over_exhaustion() {
    let trace = run_case(
        SPEC,
        &[
            probe(1, false, 10),
            Input::Tick { at_ms: 60_010 },
            probe(2, false, 60_020),
            Input::Tick { at_ms: 120_020 },
            probe(3, true, 120_030),
        ],
    );
    assert!(trace.step("poll").satisfied);
    assert!(!trace.step("poll").exhausted);
    assert_eq!(trace.state, trace.replay());
}

/// `LoopChecked` records the termination outcome (`satisfied`), not the raw
/// `while` value, so replay and readers never need to know the polarity.
#[test]
fn the_recorded_check_stores_satisfied_not_the_raw_condition() {
    let trace = run_case(SPEC, &[probe(1, true, 10)]);
    let check = trace
        .events
        .iter()
        .find(|e| e.event.name() == "LoopChecked")
        .unwrap();
    let json = serde_json::to_value(&check.event).unwrap();
    assert_eq!(
        json["check"]["satisfied"],
        serde_json::json!(true),
        "{json}"
    );
}

#[test]
fn loop_while_trace_matches_checked_in_contract() {
    use serde_json::json;
    let trace = recovered();
    let fixture = json!({
        "format": "pure-engine-trace-v1",
        "runbook": SPEC,
        "events": trace.events,
        "commands": trace.commands,
        "status": trace.state.status,
        "outputs": trace.state.outputs,
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/execution/loop-while.json");
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
