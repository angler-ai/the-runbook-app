mod support;
use runbook_model::StepStatus;
use support::*;

const TWO: &str = "spec:\n  steps:\n    - {id: a, title: A}\n    - {id: b, title: B}\n";
#[test]
fn only_first_sequential_task_is_ready() {
    let trace = run_case(TWO, &[]);
    assert_eq!(trace.ready, vec!["a"]);
    assert_eq!(trace.step("b").status, StepStatus::Pending);
    assert_eq!(trace.count("StepReady"), 1);
    assert!(runbook_engine::decide(&trace.state).unwrap().is_empty());
}
#[test]
fn successful_report_advances_once() {
    let trace = run_case(TWO, &[succeeded("a")]);
    assert_eq!(trace.ready, vec!["b"]);
    assert_eq!(trace.count("StepReported"), 1);
    assert_eq!(trace.step("a").status, StepStatus::Succeeded);
    assert_eq!(trace.count("StepReady"), 2);
    assert!(trace.error_codes.is_empty());
}
#[test]
fn replay_preserves_state() {
    let trace = run_case(TWO, &[succeeded("a"), succeeded("b")]);
    assert_eq!(trace.replay(), trace.state);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.count("ExecutionCompleted"), 1);
    assert!(runbook_engine::decide(&trace.state).unwrap().is_empty());
    // Persisted wire events, not just in-memory clones, replay identically.
    let json = serde_json::to_string(&trace.events).unwrap();
    let events: Vec<runbook_engine::EventEnvelope> = serde_json::from_str(&json).unwrap();
    assert_eq!(events, trace.events);
}
#[test]
fn memory_audit_events_do_not_change_control_flow() {
    use runbook_engine::{Command, Event, decide};
    use runbook_model::{ActorRef, Data, MemoryReadRef, ReportStatus};
    let mut plain = run_case(TWO, &[]);
    let mut memory = run_case(TWO, &[]);
    let refs = vec![MemoryReadRef {
        memory_id: "mem_00000000000000000000000001".parse().unwrap(),
        revision: 3,
    }];
    let step = memory.step("a").id.clone().unwrap();
    memory.append(
        Event::MemoryRead {
            step_run_id: step.clone(),
            refs: refs.clone(),
        },
        ActorRef::System,
        10,
    );
    memory.append(
        Event::MemoryProposed {
            step_run_id: step.clone(),
            memory_id: "mem_00000000000000000000000002".parse().unwrap(),
        },
        ActorRef::System,
        11,
    );
    // MEM6: a live save never changes control flow either.
    memory.append(
        Event::MemorySaved {
            step_run_id: step,
            memory_id: "mem_00000000000000000000000003".parse().unwrap(),
        },
        ActorRef::System,
        11,
    );
    assert_eq!(decide(&plain.state), decide(&memory.state));
    for trace in [&mut plain, &mut memory] {
        trace.append(
            Event::StepReported {
                step_run_id: trace.step("a").id.clone().unwrap(),
                report: runbook_engine::WorkReport {
                    appended_logs: false,
                    files: vec![],
                    status: ReportStatus::Succeeded,
                    verdict: None,
                    outputs: Data::new(),
                    summary: None,
                    evidence: vec![],
                    logs: vec![],
                },
            },
            ActorRef::System,
            12,
        );
    }
    assert_eq!(decide(&plain.state), decide(&memory.state));
    assert_eq!(
        decide(&memory.state).unwrap(),
        vec![Command::MakeStepReady {
            reads: vec![],
            step_id: "b".into()
        }]
    );
    memory.settle();
    assert_eq!(memory.ready, vec!["b"]);
    assert_eq!(memory.step("a").memory_read, refs);
    assert_eq!(memory.step("a").memory_proposed.len(), 1);
    assert_eq!(
        memory.step("a").memory_saved,
        ["mem_00000000000000000000000003".parse().unwrap()]
    );
    assert!(memory.error_codes.is_empty());
    assert_eq!(memory.replay(), memory.state);
}
#[test]
fn rejected_events_preserve_sequence_and_state() {
    use runbook_engine::{Event, EventEnvelope, apply};
    use runbook_model::{ActorRef, ExecutionStatus};
    let trace = run_case(TWO, &[]);
    let base = EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 500,
        actor: ActorRef::System,
        event: Event::ExecutionCompleted {
            status: ExecutionStatus::Succeeded,
            outputs: Default::default(),
        },
    };
    let mut cases = vec![(base.clone(), "execution.not_complete")];
    let mut e = base.clone();
    e.seq += 1;
    cases.push((e, "event.sequence_conflict"));
    let mut e = base.clone();
    e.seq -= 1;
    cases.push((e, "event.sequence_conflict"));
    let mut e = base.clone();
    e.execution_id = "exe_00000000000000000000000002".parse().unwrap();
    cases.push((e, "event.wrong_execution"));
    let mut e = base.clone();
    e.actor = runbook_model::ActorRef::Human {
        user_id: "usr_00000000000000000000000001".parse().unwrap(),
    };
    cases.push((e, "event.system_actor_required"));
    let mut e = base.clone();
    e.event = Event::StepReady {
        reads: vec![],
        step_id: "b".into(),
        step_run_id: "sr_00000000000000000000000002".parse().unwrap(),
    };
    cases.push((e, "step.not_ready"));
    let mut e = base.clone();
    e.event = Event::StepReady {
        reads: vec![],
        step_id: "a".into(),
        step_run_id: trace.step("a").id.clone().unwrap(),
    };
    cases.push((e, "step.not_ready"));
    let mut e = base;
    e.event = Event::MemoryRead {
        step_run_id: "sr_00000000000000000000000099".parse().unwrap(),
        refs: vec![],
    };
    cases.push((e, "engine.invariant"));
    let before = trace.state.clone();
    for (e, code) in cases {
        assert_eq!(apply(&trace.state, &e).unwrap_err().code(), code);
        assert_eq!(trace.state, before);
    }
}
#[test]
fn duplicate_report_never_advances_twice() {
    let trace = run_case(TWO, &[succeeded("a"), succeeded("a")]);
    assert_eq!(trace.error_codes, ["step.not_reportable"]);
    assert_eq!(trace.count("StepReported"), 1);
    assert_eq!(trace.count("StepReady"), 2);
    assert_eq!(trace.ready, ["b"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn empty_sequence_completes_and_finally_materializes() {
    let empty = run_case("spec: {steps: []}", &[]);
    assert_eq!(
        empty.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert!(empty.ready.is_empty());
    for spec in [
        "spec: {steps: [{id: group, steps: [], finally: [{id: clean, title: Clean}]}]}",
        "spec: {steps: [], finally: [{id: clean, title: Clean}]}",
    ] {
        let trace = run_case(spec, &[]);
        assert!(trace.error_codes.is_empty());
        assert_eq!(trace.ready, ["clean"]);
    }
}
#[test]
fn replay_retains_supplied_time_actor_and_outputs() {
    use runbook_model::{ActorRef, Data, ReportStatus};
    let actor = ActorRef::Agent {
        principal_id: "service-one".into(),
        session_id: "session-one".into(),
        name: "test-agent".into(),
        model: Some("sol".into()),
        service_attribution: None,
    };
    let outputs: Data = [("message".into(), serde_json::json!("done"))].into();
    let trace = run_case(
        "spec: {steps: [{id: a, title: A, outputs: {message: {type: string}}}, {id: b, title: B}]}",
        &[Input::Report {
            step: "a".into(),
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs: outputs.clone(),
            actor: actor.clone(),
            at_ms: Some(123456),
        }],
    );
    let reported = trace
        .events
        .iter()
        .find(|e| e.event.name() == "StepReported")
        .unwrap();
    assert_eq!(reported.actor, actor);
    assert_eq!(reported.at_ms, 123456);
    assert_eq!(trace.state.at_ms, 123456);
    assert_eq!(trace.step("a").outputs, outputs);
    assert_eq!(trace.state, trace.replay());
    let encoded = serde_json::to_string(&trace.events).unwrap();
    let events: Vec<runbook_engine::EventEnvelope> = serde_json::from_str(&encoded).unwrap();
    assert_eq!(events, trace.events);
}
