mod support;
use support::*;
#[test]
fn duration_wait_needs_its_deadline() {
    let spec = "spec: {steps: [{id: gate, wait: {for: 5m}}, {id: next, title: Next}]}";
    let before = run_case(spec, &[Input::Tick { at_ms: 299_999 }]);
    let after = run_case(spec, &[Input::Tick { at_ms: 300_000 }]);
    assert!(before.ready.is_empty());
    assert_eq!(after.ready, ["next"]);
    assert_eq!(after.state, after.replay());
}
#[test]
fn absolute_wait_uses_supplied_time() {
    let spec = "spec: {steps: [{id: gate, wait: {until: '1970-01-01T00:10:00Z'}}, {id: next, title: Next}]}";
    assert!(
        run_case(spec, &[Input::Tick { at_ms: 599_999 }])
            .ready
            .is_empty()
    );
    let trace = run_case(spec, &[Input::Tick { at_ms: 600_000 }]);
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn signal_wait_timeout_is_failure() {
    let trace = run_case(
        "spec: {steps: [{id: gate, wait: {signal: deploy, timeout: 5m}}]}",
        &[Input::Tick { at_ms: 300_000 }],
    );
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::TimedOut
    );
    assert_eq!(
        trace.state.pending_terminal_cause,
        Some(runbook_engine::TerminalCause::Failed)
    );
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn signal_name_must_match() {
    let mut trace = run_case(
        "spec: {steps: [{id: gate, wait: {signal: deploy}}, {id: next, title: Next}]}",
        &[],
    );
    send_signal(&mut trace, "unrelated", "sig_wrong", 1);
    assert_eq!(trace.error_codes, ["signal.not_waiting"]);
    assert!(trace.ready.is_empty());
    send_signal(&mut trace, "deploy", "sig_right", 2);
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn signals_are_not_buffered_or_reused_for_later_waits() {
    let mut trace = run_case(
        "spec: {steps: [{id: gate, wait: {signal: deploy}}, {id: later, wait: {signal: deploy}}, {id: next, title: Next}]}",
        &[],
    );
    send_signal(&mut trace, "unknown", "ignored", 0);
    assert_eq!(trace.error_codes, ["signal.not_waiting"]);
    assert!(trace.state.signals.is_empty());
    send_signal(&mut trace, "deploy", "one", 1);
    assert_eq!(
        trace.step("later").status,
        runbook_model::StepStatus::Waiting
    );
    send_signal(&mut trace, "deploy", "one", 2);
    // A redelivered signal ID is an idempotent no-op, not a rejection.
    assert_eq!(trace.error_codes, ["signal.not_waiting"]);
    assert_eq!(trace.count("SignalReceived"), 1);
    assert!(trace.ready.is_empty());
    send_signal(&mut trace, "deploy", "two", 3);
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn signal_at_deadline_cannot_beat_an_undrained_timer() {
    let mut trace = run_case(
        "spec: {steps: [{id: gate, wait: {signal: deploy, timeout: 5m}}]}",
        &[],
    );
    send_signal(&mut trace, "deploy", "late", 300_000);
    assert_eq!(trace.error_codes, ["signal.not_waiting"]);
    assert!(trace.state.signals.is_empty());
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::Waiting
    );
    for event in runbook_engine::due_timers(&trace.state, 300_000).unwrap() {
        trace.append(event, runbook_model::ActorRef::System, 300_000);
    }
    trace.settle();
    send_signal(&mut trace, "deploy", "later", 300_001);
    assert_eq!(
        trace.error_codes,
        ["signal.not_waiting", "signal.not_waiting"]
    );
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::TimedOut
    );
    assert_eq!(trace.count("SignalReceived"), 0);
}
#[test]
fn matching_parallel_waits_consume_one_signal_atomically() {
    let mut trace = run_case(
        "spec: {steps: [{id: both, parallel: [{id: a, wait: {signal: deploy}}, {id: b, wait: {signal: deploy}}]}, {id: next, title: Next}]}",
        &[],
    );
    send_signal(&mut trace, "deploy", "one", 1);
    assert_eq!(trace.count("SignalReceived"), 1);
    assert_eq!(trace.state.signals["one"].step_run_ids.len(), 2);
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn timeout_routes_through_continue_and_catch() {
    for spec in [
        "spec: {steps: [{id: gate, wait: {signal: deploy, timeout: 1m}, on_failure: continue}, {id: next, title: Next}]}",
        "spec: {steps: [{id: group, steps: [{id: gate, wait: {signal: deploy, timeout: 1m}}], catch: [{id: next, title: Next}]}]}",
    ] {
        let trace = run_case(spec, &[Input::Tick { at_ms: 60_000 }]);
        assert_eq!(
            trace.step("gate").status,
            runbook_model::StepStatus::TimedOut
        );
        assert_eq!(trace.ready, ["next"]);
        assert!(trace.error_codes.is_empty());
        assert_eq!(trace.state, trace.replay());
    }
}
#[test]
fn elapsed_absolute_wait_finishes_without_a_future_tick() {
    let trace = run_case(
        "spec: {steps: [{id: gate, wait: {until: '1969-12-31T23:59:59Z'}}, {id: next, title: Next}]}",
        &[],
    );
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.count("TimerFired"), 1);
}
#[test]
fn submillisecond_target_rounds_up_without_waking_early() {
    let spec = "spec: {steps: [{id: gate, wait: {until: '1970-01-01T00:00:00.000001Z'}}, {id: next, title: Next}]}";
    assert!(run_case(spec, &[]).ready.is_empty());
    assert_eq!(run_case(spec, &[Input::Tick { at_ms: 1 }]).ready, ["next"]);
}
#[test]
fn wait_timer_input_is_scoped_and_generation_fenced() {
    use runbook_engine::*;
    let trace = run_case("spec: {steps: [{id: gate, wait: {for: 1m}}]}", &[]);
    let input = WaitInput::Timer {
        execution_id: trace.state.id.clone(),
        step_run_id: trace.step("gate").id.clone().unwrap(),
        generation: 1,
        at_ms: 60_000,
    };
    assert_eq!(advance_wait(&trace.state, &input).unwrap().len(), 1);
    for mutation in 0..3 {
        let mut bad = input.clone();
        let WaitInput::Timer {
            execution_id,
            generation,
            at_ms,
            ..
        } = &mut bad
        else {
            unreachable!()
        };
        match mutation {
            0 => *execution_id = "exe_00000000000000000000000002".parse().unwrap(),
            1 => *generation = 2,
            _ => *at_ms = 59_999,
        };
        assert!(advance_wait(&trace.state, &bad).is_err());
    }
}
#[test]
fn signal_event_rejects_partial_targets_and_conflicting_identity() {
    use runbook_engine::*;
    let mut trace = run_case(
        "spec: {steps: [{id: both, parallel: [{id: a, wait: {signal: deploy}}, {id: b, wait: {signal: deploy}}]}, {id: next, title: Next}]}",
        &[],
    );
    let input = WaitInput::Signal {
        execution_id: trace.state.id.clone(),
        signal_id: "one".into(),
        name: "deploy".into(),
        payload: Default::default(),
        at_ms: 1,
    };
    let command = advance_wait(&trace.state, &input).unwrap().remove(0);
    let Command::ConsumeSignal { mut signal } = command else {
        unreachable!()
    };
    signal.step_run_ids.pop();
    trace.append(
        Event::SignalReceived { signal },
        runbook_model::ActorRef::System,
        1,
    );
    assert_eq!(trace.error_codes, ["signal.not_waiting"]);
    assert_eq!(trace.step("a").status, runbook_model::StepStatus::Waiting);
    send_signal(&mut trace, "deploy", "one", 2);
    send_signal(&mut trace, "other", "one", 3);
    assert_eq!(trace.error_codes.last().unwrap(), "signal.id_conflict");
}
#[test]
fn timestamp_expression_is_resolved_once_with_read_provenance() {
    let trace = run_case(
        "spec: {vars: {wake: \"${{ timestamp('1970-01-01T00:10:00Z') }}\"}, steps: [{id: gate, wait: {until: '${{ vars.wake }}'}}, {id: next, title: Next}]}",
        &[Input::Tick { at_ms: 600_000 }],
    );
    assert_eq!(trace.ready, ["next"]);
    assert!(
        trace
            .step("gate")
            .reads
            .iter()
            .any(|r| r.origin.path == "vars.wake")
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn time_waits_schedule_their_wakeup() {
    // The native drain only wakes an execution at `next_wakeup`, so a
    // duration or absolute wait must report its target time there.
    for (spec, due) in [
        (
            "spec: {steps: [{id: gate, wait: {for: 5m}}, {id: next, title: Next}]}",
            300_000,
        ),
        (
            "spec: {steps: [{id: gate, wait: {until: '1970-01-01T00:10:00Z'}}, {id: next, title: Next}]}",
            600_000,
        ),
        (
            "spec: {steps: [{id: gate, wait: {for: 5m, timeout: 1m}}, {id: next, title: Next}]}",
            60_000,
        ),
    ] {
        let trace = run_case(spec, &[]);
        assert_eq!(
            trace.step("gate").status,
            runbook_model::StepStatus::Waiting
        );
        assert_eq!(
            runbook_engine::next_wakeup(&trace.state).unwrap(),
            Some(due),
            "{spec}"
        );
    }
}

// PRD §17: a single wait parks a run at most 7 days. An `until` target is
// only known at runtime, so from engine version 4 a target more than 7 days
// after the wait starts fails the step and the run with
// `limit.wait_exceeded` (like the step-run limit), instead of waking early.
const SEVEN_DAYS_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
fn until(target: &str) -> String {
    format!(
        "spec: {{steps: [{{id: gate, wait: {{until: '{target}'}}}}, {{id: next, title: Next}}]}}"
    )
}
#[test]
fn until_wait_at_the_seven_day_limit_waits() {
    let trace = run_case(&until("1970-01-08T00:00:00Z"), &[]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::Waiting
    );
    let trace = run_case(
        &until("1970-01-08T00:00:00Z"),
        &[Input::Tick {
            at_ms: SEVEN_DAYS_MS,
        }],
    );
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn until_wait_past_the_seven_day_limit_fails_the_run() {
    let trace = run_case(&until("1970-01-08T00:00:00.001Z"), &[]);
    assert_eq!(trace.error_codes, ["limit.wait_exceeded"]);
    assert_eq!(trace.count("WaitStarted"), 0);
    assert_eq!(trace.step("gate").status, runbook_model::StepStatus::Failed);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Failed,
        "{:?}",
        trace.event_names()
    );
    assert_eq!(
        trace.state.terminal_reason.as_deref(),
        Some("limit.wait_exceeded")
    );
    assert!(trace.ready.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn runs_before_engine_version_4_keep_long_until_waits() {
    let new = run_case(&until("1970-01-09T00:00:00Z"), &[]);
    let fault = new
        .events
        .iter()
        .position(|e| e.event.name() == "ExecutionFaulted")
        .expect("version 4 faults the wait");
    let mut trace = new;
    trace.events.truncate(fault);
    let runbook_engine::Event::ExecutionStarted { engine_version, .. } = &mut trace.events[0].event
    else {
        panic!("first event starts the run")
    };
    *engine_version = 3;
    trace.state = trace
        .events
        .iter()
        .try_fold(trace.initial.clone(), |state, event| {
            runbook_engine::apply(&state, event)
        })
        .unwrap();
    assert_eq!(trace.error_codes, ["limit.wait_exceeded"]);
    trace.error_codes = Default::default();
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("WaitStarted"), 1);
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::Waiting
    );
    // The recorded log replays unchanged.
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn a_long_until_wait_event_is_rejected_under_version_4() {
    let legacy = {
        let mut trace = run_case(&until("1970-01-09T00:00:00Z"), &[]);
        let fault = trace
            .events
            .iter()
            .position(|e| e.event.name() == "ExecutionFaulted")
            .unwrap();
        assert_eq!(trace.error_codes, ["limit.wait_exceeded"]);
        trace.events.truncate(fault);
        trace
    };
    // A forged WaitStarted beyond the cap does not apply to a version 4 run.
    let state = legacy
        .events
        .iter()
        .try_fold(legacy.initial.clone(), |state, event| {
            runbook_engine::apply(&state, event)
        })
        .unwrap();
    let gate = state.steps.iter().find(|s| s.step_id == "gate").unwrap();
    let forged = runbook_engine::EventEnvelope {
        execution_id: state.id.clone(),
        seq: state.sequence + 1,
        at_ms: 0,
        actor: runbook_model::ActorRef::System,
        event: runbook_engine::Event::WaitStarted {
            start: runbook_engine::WaitStart {
                base_sequence: state.sequence,
                step_run_id: gate.id.clone().unwrap(),
                until_at_ms: Some(SEVEN_DAYS_MS + 1),
                reads: vec![],
            },
        },
    };
    assert_eq!(
        runbook_engine::apply(&state, &forged).unwrap_err().code(),
        "limit.wait_exceeded"
    );
}
