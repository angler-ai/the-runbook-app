mod support;
use runbook_model::{ExecutionStatus, StepStatus, Verdict};
use support::*;
const LOOP: &str = "spec: {steps: [{id: stabilize, loop: {max_iterations: 2, until: '${{ steps.recheck.outputs.rate <= 0.01 }}', steps: [{id: recheck, title: Recheck, check: true, outputs: {rate: {type: number, required: true}}}]}}]}";
fn rate(iteration: u32, value: f64, finding: Verdict) -> Input {
    let mut report = succeeded(&format!("stabilize#{iteration}.recheck"));
    let Input::Report {
        outputs, verdict, ..
    } = &mut report
    else {
        unreachable!()
    };
    outputs.insert("rate".into(), serde_json::json!(value));
    *verdict = Some(finding);
    report
}
#[test]
fn recovery_keeps_history_but_satisfies_loop() {
    let started = run_case(LOOP, &[]);
    assert_eq!(started.ready, ["stabilize#1.recheck"]);
    let trace = run_case(
        LOOP,
        &[rate(1, 0.1, Verdict::Fail), rate(2, 0.0, Verdict::Pass)],
    );
    assert_eq!(trace.step("stabilize").verdict, Some(Verdict::Fail));
    assert!(trace.step("stabilize").satisfied);
    assert!(!trace.step("stabilize").exhausted);
    assert_eq!(trace.step("stabilize").status, StepStatus::Succeeded);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn exhaustion_is_a_finding() {
    let trace = run_case(
        LOOP,
        &[rate(1, 0.1, Verdict::Fail), rate(2, 0.1, Verdict::Fail)],
    );
    assert_eq!(trace.step("stabilize").status, StepStatus::Succeeded);
    assert!(trace.step("stabilize").exhausted);
    assert!(!trace.step("stabilize").satisfied);
    assert_eq!(trace.step("stabilize").verdict, Some(Verdict::Fail));
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn loop_delay_requires_timer() {
    let spec = LOOP.replace("max_iterations: 2", "max_iterations: 2, delay: 5m");
    let before = run_case(
        &spec,
        &[rate(1, 0.1, Verdict::Fail), Input::Tick { at_ms: 299_999 }],
    );
    assert!(before.ready.is_empty());
    let after = run_case(
        &spec,
        &[rate(1, 0.1, Verdict::Fail), Input::Tick { at_ms: 300_000 }],
    );
    assert_eq!(after.ready, ["stabilize#2.recheck"]);
    assert_eq!(after.state, after.replay());
}
#[test]
fn first_success_and_last_boundary_take_precedence_over_exhaustion() {
    let first = run_case(LOOP, &[rate(1, 0.0, Verdict::Pass)]);
    assert!(first.step("stabilize").satisfied);
    assert!(!first.step("stabilize").exhausted);
    assert_eq!(first.count("LoopIterationStarted"), 1);
    for until in ["iteration == 2", "false"] {
        let source = format!(
            "spec: {{steps: [{{id: repeat, loop: {{max_iterations: 2, until: '${{{{ {until} }}}}', steps: []}}}}], outputs: {{result: '${{{{ steps.repeat.satisfied }}}}', exhausted: '${{{{ steps.repeat.exhausted }}}}'}}}}"
        );
        let trace = run_case(&source, &[]);
        assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
        assert_eq!(trace.count("LoopChecked"), 2);
        assert_eq!(trace.step("repeat").satisfied, until != "false");
        assert_eq!(trace.step("repeat").exhausted, until == "false");
        assert_eq!(
            trace.step("repeat").verdict,
            if until == "false" {
                Some(Verdict::Fail)
            } else {
                None
            }
        );
        assert_eq!(trace.state, trace.replay());
    }
}
#[test]
fn stale_and_early_timer_firings_never_admit_an_iteration() {
    use runbook_engine::Event;
    use runbook_model::ActorRef;
    let spec = LOOP.replace("max_iterations: 2", "max_iterations: 3, delay: 5m");
    let mut trace = run_case(
        &spec,
        &[
            rate(1, 0.1, Verdict::Fail),
            Input::Tick { at_ms: 300_000 },
            rate(2, 0.1, Verdict::Fail),
        ],
    );
    let id = trace.step("stabilize").id.clone().unwrap();
    let before = trace.state.clone();
    trace.append(
        Event::TimerFired {
            step_run_id: id.clone(),
            generation: 1,
        },
        ActorRef::System,
        600_000,
    );
    assert_eq!(trace.error_codes.last().unwrap(), "timer.stale_generation");
    trace.append(
        Event::TimerFired {
            step_run_id: id.clone(),
            generation: 2,
        },
        ActorRef::System,
        599_999,
    );
    assert_eq!(trace.error_codes.last().unwrap(), "timer.not_due");
    assert_eq!(trace.state, before);
    trace.append(
        Event::TimerFired {
            step_run_id: id.clone(),
            generation: 2,
        },
        ActorRef::System,
        600_000,
    );
    trace.settle();
    assert_eq!(trace.ready, ["stabilize#3.recheck"]);
    trace.append(
        Event::TimerFired {
            step_run_id: id,
            generation: 2,
        },
        ActorRef::System,
        600_000,
    );
    assert_eq!(trace.error_codes.last().unwrap(), "timer.stale_generation");
    assert_eq!(trace.count("LoopIterationStarted"), 3);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn loop_and_foreach_scopes_keep_current_and_historical_outputs() {
    let source = "spec: {inputs: {regions: {type: list, items: {type: string}, default: [east, west]}}, vars: {seen: none, ordinal: 0}, steps: [{id: repeat, loop: {max_iterations: 2, until: '${{ iteration == 2 }}', steps: [{id: each, foreach: {items: '${{ inputs.regions }}', as: region, key: '${{ region }}', max_items: 2, concurrency: 2, steps: [{id: save, set: {vars: {seen: '${{ region }}', ordinal: '${{ iteration }}'}}}]}}]}}], outputs: {result: '${{ steps.repeat.iterations[0].each.items[0].save.status }}', last: '${{ steps.save.status }}'}}";
    let trace = run_case(source, &[]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("DataChanged"), 4);
    assert_eq!(trace.state.vars["ordinal"], serde_json::json!(2));
    assert_eq!(trace.state.result, serde_json::json!("succeeded"));
    assert_eq!(trace.step("repeat#2.each[west].save").scope.len(), 2);
    let last = trace
        .state
        .output_reads
        .iter()
        .find(|r| r.origin.path == "steps.save.status")
        .unwrap();
    assert_eq!(
        last.origin
            .record
            .iteration_path
            .iter()
            .map(|i| i.ordinal)
            .collect::<Vec<_>>(),
        [1, 1]
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn nested_loops_use_the_inner_iteration_number() {
    let source = "spec: {vars: {ordinal: 0}, steps: [{id: outer, loop: {max_iterations: 2, until: '${{ iteration == 2 }}', steps: [{id: inner, loop: {max_iterations: 2, until: '${{ iteration == 2 }}', steps: [{id: save, set: {vars: {ordinal: '${{ iteration }}'}}}]}}]}}]}";
    let trace = run_case(source, &[]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("DataChanged"), 4);
    assert_eq!(trace.count("LoopChecked"), 6);
    assert!(trace.step("outer").satisfied);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn protocol_failure_does_not_satisfy_or_exhaust_a_loop() {
    let mut failure = rate(1, 0.1, Verdict::Fail);
    let Input::Report { status, .. } = &mut failure else {
        unreachable!()
    };
    *status = runbook_model::ReportStatus::Failed;
    let trace = run_case(LOOP, &[failure]);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(trace.step("stabilize").status, StepStatus::Failed);
    assert!(!trace.step("stabilize").satisfied);
    assert!(!trace.step("stabilize").exhausted);
    assert_eq!(trace.count("LoopChecked"), 0);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn false_guard_keeps_loop_outcome_flags_false() {
    let trace = run_case(
        &LOOP.replace(
            "id: stabilize, loop",
            "id: stabilize, if: '${{ false }}', loop",
        ),
        &[],
    );
    assert_eq!(trace.step("stabilize").status, StepStatus::Skipped);
    assert!(!trace.step("stabilize").satisfied);
    assert!(!trace.step("stabilize").exhausted);
    assert_eq!(trace.count("LoopIterationStarted"), 0);
}
