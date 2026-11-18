//! Regression tests for the 2026-11-11 independent review, group 1 (engine).
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, ExecutionStatus, StepStatus};
use support::*;

fn alice() -> runbook_spec::CompileContext {
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    directory
}
fn doc(spec: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test, owners: [user:alice]}}\n{spec}"
    )
}

// F1: a pause during cleanup must not survive suppression at the cleanup deadline.
#[test]
fn expired_finally_approval_still_closes_at_the_cleanup_deadline() {
    let trace = run_document(
        &doc(
            "spec: {execution: {timeout: 2h, cleanup_timeout: 30m}, steps: [{id: work, title: Work}], finally: [{id: gate, approval: {prompt: Go?, options: [go], approvers: {users: [alice]}, timeout: 1m, escalation_timeout: 1m}}]}",
        ),
        &[
            succeeded("work"),
            Input::Tick { at_ms: 120_000 },
            Input::Tick { at_ms: 1_800_000 },
        ],
        alice(),
    );
    assert_eq!(trace.step("gate").status, StepStatus::Expired);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(trace.count("CleanupSuppressed"), 1);
    assert_eq!(trace.count("ExecutionCompleted"), 1);
    assert!(trace.state.pause_reasons.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn manual_pause_during_cleanup_still_closes_at_the_cleanup_deadline() {
    let mut trace = run_case(
        "spec: {execution: {cleanup_timeout: 30m}, steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify}]}",
        &[succeeded("work")],
    );
    assert_eq!(trace.state.phase, ExecutionPhase::Cleanup);
    trace.append(
        Event::ManualPauseChanged { paused: true },
        ActorRef::System,
        1,
    );
    assert_eq!(trace.state.status, ExecutionStatus::Paused);
    for command in advance_time(&trace.state, 1_800_000).unwrap() {
        trace.commit_command(command, 1_800_000);
    }
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(trace.count("ExecutionCompleted"), 1);
    assert_eq!(trace.state, trace.replay());
}

fn alice_vote(trace: &Trace, option: &str, at_ms: i64) -> ResolvedVote {
    ResolvedVote {
        at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "pat_alice".into(),
        option: option.into(),
        comment: None,
        eligibility: VoteEligibility {
            eligible: true,
            directory_revision: 1,
            matched_recipients: vec![runbook_model::RecipientRef::User(
                "usr_00000000000000000000000001".parse().unwrap(),
            )],
        },
    }
}
fn submit(trace: &mut Trace, vote: ResolvedVote) {
    match apply_vote(&trace.state, &vote) {
        Ok(events) => {
            for event in events {
                trace.append(event, vote.actor.clone(), vote.at_ms);
            }
        }
        Err(error) => trace.error_codes.push(error.code().into()),
    }
    trace.settle();
}

// F2: once an automatic gate's deadline has passed, timeout handling wins
// even if the timer worker has not drained it yet (PRD §5).
#[test]
fn vote_after_an_automatic_gate_deadline_is_rejected() {
    let mut trace = run_document(
        &doc(
            "spec: {steps: [{id: gate, approval: {prompt: Roll back?, options: [keep, {id: rollback, auto: true}], approvers: {users: [alice]}, timeout: 30m, on_timeout: {decide: rollback}}}, {id: rollback, title: Rollback, destructive: true, allow_auto_decision: true}]}",
        ),
        &[],
        alice(),
    );
    let vote = alice_vote(&trace, "keep", 1_800_001);
    submit(&mut trace, vote);
    assert_eq!(trace.error_codes, ["approval.deadline_exceeded"]);
    assert_eq!(trace.count("ApprovalVoted"), 0);
    assert_eq!(trace.count("ApprovalDecided"), 0);
    // A vote before the deadline still decides.
    let mut trace = run_document(
        &doc(
            "spec: {steps: [{id: gate, approval: {prompt: Roll back?, options: [keep, {id: rollback, auto: true}], approvers: {users: [alice]}, timeout: 30m, on_timeout: {decide: rollback}}}, {id: rollback, title: Rollback, destructive: true, allow_auto_decision: true}]}",
        ),
        &[],
        alice(),
    );
    let vote = alice_vote(&trace, "keep", 1_799_999);
    submit(&mut trace, vote);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("gate").outputs["decision"], "keep");
    assert_eq!(trace.count("DecisionApplied"), 0);
}

// F3: a gate whose expiry failure was already consumed cannot be reopened.
#[test]
fn late_vote_cannot_reopen_a_failed_expired_gate() {
    let mut trace = run_document(
        &doc(
            "spec: {steps: [{id: gate, on_failure: continue, approval: {prompt: Go?, options: [go], approvers: {users: [alice]}, timeout: 1m, escalation_timeout: 1m, on_expired: fail}}, {id: after, title: After}]}",
        ),
        &[Input::Tick { at_ms: 120_000 }],
        alice(),
    );
    assert_eq!(trace.step("gate").status, StepStatus::Expired);
    assert_eq!(trace.ready, ["after"]);
    let vote = alice_vote(&trace, "go", 150_000);
    submit(&mut trace, vote);
    assert_eq!(trace.error_codes, ["approval.not_open"]);
    assert_eq!(trace.step("gate").status, StepStatus::Expired);
    assert!(trace.step("gate").outputs.is_empty());
}

// F95: every run records the engine semantics it started under, in its log.
#[test]
fn execution_started_records_the_engine_version() {
    let trace = run_case("spec: {steps: [{id: work, title: Work}]}", &[]);
    assert_eq!(trace.state.engine_version, ENGINE_VERSION);
    let started = serde_json::to_value(&trace.events[0].event).unwrap();
    assert_eq!(started["type"], "ExecutionStarted");
    assert_eq!(started["engine_version"], ENGINE_VERSION);

    // A log recorded before the marker existed is the legacy version.
    let mut legacy = started.clone();
    legacy.as_object_mut().unwrap().remove("engine_version");
    let mut envelope = trace.events[0].clone();
    envelope.event = serde_json::from_value(legacy).unwrap();
    let state = apply(&trace.initial, &envelope).unwrap();
    assert_eq!(state.engine_version, LEGACY_ENGINE_VERSION);

    // A log from a newer engine is refused rather than misread.
    let mut newer = started;
    newer["engine_version"] = (ENGINE_VERSION + 1).into();
    envelope.event = serde_json::from_value(newer).unwrap();
    assert_eq!(
        apply(&trace.initial, &envelope).unwrap_err().code(),
        "execution.unsupported_engine_version"
    );
}

fn tick(trace: &mut Trace, at_ms: i64) {
    for _ in 0..20 {
        let commands = advance_time(&trace.state, at_ms).unwrap();
        if commands.is_empty() {
            break;
        }
        for command in commands {
            trace.commit_command(command, at_ms);
        }
    }
    trace.settle();
}

// F4: PRD §6 says `failed` covers a timeout, so a task timeout follows
// `retry` and `on_failure` like any failure, and each new attempt gets a
// fresh task deadline.
#[test]
fn task_timeout_retries_with_a_fresh_deadline() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work, timeout: 10m, retry: {max_attempts: 2}}, {id: after, title: After}]}",
        &[],
    );
    tick(&mut trace, 600_000);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("RetryScheduled"), 1);
    let work = trace.step("work");
    assert_eq!((work.status, work.attempt), (StepStatus::Ready, 2));
    assert_eq!(work.deadline_at_ms, Some(1_200_000));
    assert!(work.attempt_history[0].timed_out);
    assert_eq!(trace.state.status, ExecutionStatus::Running);
    // The second attempt is the last: its timeout is final.
    tick(&mut trace, 1_200_000);
    assert_eq!(trace.step("work").status, StepStatus::TimedOut);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn task_timeout_hands_off_with_a_fresh_deadline() {
    let mut trace = run_document(
        &doc(
            "spec: {defaults: {on_failure: handoff}, steps: [{id: work, title: Work, actor: agent, timeout: 10m}]}",
        ),
        &[],
        alice(),
    );
    tick(&mut trace, 600_000);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("StepHandedOff"), 1);
    let work = trace.step("work");
    assert_eq!(work.status, StepStatus::Ready);
    assert!(work.human_continuation);
    assert_eq!(work.deadline_at_ms, Some(1_200_000));
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn failed_report_near_the_deadline_hands_off_with_a_fresh_deadline() {
    let agent = ActorRef::Agent {
        principal_id: "agt_ops".into(),
        session_id: "s1".into(),
        name: "ops".into(),
        model: None,
    };
    let trace = run_document(
        &doc(
            "spec: {defaults: {on_failure: handoff}, steps: [{id: work, title: Work, actor: agent, timeout: 10m}]}",
        ),
        &[Input::Report {
            step: "work".into(),
            status: runbook_model::ReportStatus::Failed,
            verdict: None,
            outputs: Default::default(),
            actor: agent,
            at_ms: Some(590_000),
        }],
        alice(),
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let work = trace.step("work");
    assert!(work.human_continuation);
    assert_eq!(work.deadline_at_ms, Some(1_190_000));
}
#[test]
fn legacy_runs_keep_final_task_timeouts() {
    let new = run_case(
        "spec: {steps: [{id: work, title: Work, timeout: 10m, retry: {max_attempts: 2}}]}",
        &[],
    );
    let mut events = new.events.clone();
    let Event::ExecutionStarted { engine_version, .. } = &mut events[0].event else {
        panic!("first event starts the run")
    };
    *engine_version = LEGACY_ENGINE_VERSION;
    let state = events
        .iter()
        .try_fold(new.initial.clone(), |state, event| apply(&state, event))
        .unwrap();
    let elapsed = EventEnvelope {
        execution_id: state.id.clone(),
        seq: state.sequence + 1,
        at_ms: 600_000,
        actor: ActorRef::System,
        event: Event::TaskDeadlineElapsed {
            step_run_id: new.step("work").id.clone().unwrap(),
            deadline_at_ms: 600_000,
        },
    };
    let state = apply(&state, &elapsed).unwrap();
    let work = state.steps.iter().find(|s| s.step_id == "work").unwrap();
    assert_eq!(work.status, StepStatus::TimedOut);
    assert!(work.attempt_history.is_empty());
}

// F17: a duration wait whose timeout equals its duration reaches its target.
#[test]
fn wait_whose_timeout_equals_its_duration_succeeds() {
    let mut trace = run_case(
        "spec: {steps: [{id: pause, wait: {for: 5m, timeout: 5m}}, {id: after, title: After}]}",
        &[],
    );
    tick(&mut trace, 300_000);
    assert_eq!(trace.step("pause").status, StepStatus::Succeeded);
    assert_eq!(trace.ready, ["after"]);
    // An earlier timeout still times the wait out.
    let mut trace = run_case(
        "spec: {steps: [{id: pause, wait: {for: 5m, timeout: 4m}}, {id: after, title: After}]}",
        &[],
    );
    tick(&mut trace, 300_000);
    assert_eq!(trace.step("pause").status, StepStatus::TimedOut);
}

// F13: cancel during cleanup is accepted. A success-path cleanup becomes a
// cancelled one (bounded by the cleanup deadline); a failed one stays failed.
#[test]
fn cancellation_during_cleanup_is_accepted() {
    let mut trace = run_case(
        "spec: {execution: {cleanup_timeout: 30m}, steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify}]}",
        &[succeeded("work")],
    );
    assert_eq!(trace.state.phase, ExecutionPhase::Cleanup);
    trace.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        ActorRef::System,
        1,
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(
        trace.state.pending_terminal_cause,
        Some(TerminalCause::Cancelled)
    );
    tick(&mut trace, 1_800_000);
    assert_eq!(trace.state.status, ExecutionStatus::Cancelled);
    assert_eq!(trace.state, trace.replay());

    let mut failed = run_case(
        "spec: {steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify}]}",
        &[Input::Report {
            step: "work".into(),
            status: runbook_model::ReportStatus::Failed,
            verdict: None,
            outputs: Default::default(),
            actor: ActorRef::System,
            at_ms: None,
        }],
    );
    assert_eq!(failed.state.phase, ExecutionPhase::Cleanup);
    failed.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        ActorRef::System,
        1,
    );
    assert!(failed.error_codes.is_empty(), "{:?}", failed.error_codes);
    assert_eq!(
        failed.state.pending_terminal_cause,
        Some(TerminalCause::Failed)
    );
}

// F15: only tasks are claimable; a ready control node is not.
#[test]
fn only_task_steps_can_be_claimed() {
    let trace = run_case(
        "spec: {steps: [{id: fan, parallel: [{id: a, title: A}, {id: b, title: B}]}]}",
        &[],
    );
    let fan = trace.step("fan").id.clone().unwrap();
    assert_eq!(trace.step("fan").status, StepStatus::Ready);
    let mut trace = trace;
    trace.append(
        Event::StepClaimed { step_run_id: fan },
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        1,
    );
    assert_eq!(trace.error_codes, ["step.not_claimable"]);
}

fn amend(trace: &mut Trace, step: &str, n: i64) {
    let id = trace.step(step).id.clone().unwrap();
    let to = runbook_model::StepRecord {
        status: StepStatus::Succeeded,
        verdict: None,
        outputs: [("n".to_string(), serde_json::json!(n))].into(),
        summary: None,
    };
    let amendment =
        validate_override(&trace.state, &id, &to, "correction", ActorRef::System).unwrap();
    trace.append(
        Event::StepOverridden {
            step_run_id: id,
            amendment: Box::new(amendment),
        },
        ActorRef::System,
        trace.state.at_ms + 1,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
}
fn reported(step: &str, n: i64) -> Input {
    Input::Report {
        step: step.into(),
        status: runbook_model::ReportStatus::Succeeded,
        verdict: None,
        outputs: [("n".to_string(), serde_json::json!(n))].into(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        at_ms: None,
    }
}

// F14: staleness follows reads through an already-stale `set` and
// through `fields`, not only `vars`.
#[test]
fn second_amendment_marks_readers_of_an_already_stale_set() {
    let spec = r#"spec:
  vars: {v: 0}
  steps:
    - {id: a, title: A, outputs: {n: {type: integer}}}
    - id: assign
      set: {vars: {v: "${{ steps.a.outputs.n }}"}}
    - {id: hold, title: Hold}
    - {id: c, title: C, if: "${{ vars.v >= 0 }}"}"#;
    let mut trace = run_case(spec, &[reported("a", 1)]);
    amend(&mut trace, "a", 2);
    assert!(trace.step("assign").stale);
    let mut trace = {
        // `c` reads `vars.v` only after the first amendment.
        let inputs = [succeeded("hold"), succeeded("c")];
        let mut t = trace;
        for input in inputs {
            let Input::Report { step, .. } = &input else {
                unreachable!()
            };
            let id = t.step(step).id.clone().unwrap();
            t.append(
                Event::StepReported {
                    step_run_id: id,
                    report: WorkReport {
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
                ActorRef::Human {
                    user_id: "usr_00000000000000000000000001".parse().unwrap(),
                },
                t.state.at_ms + 1,
            );
            t.settle();
        }
        t
    };
    assert!(!trace.step("c").stale);
    amend(&mut trace, "a", 3);
    assert!(trace.step("c").stale, "read through the already-stale set");
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn amendment_marks_readers_of_a_field_it_fed() {
    let spec = r#"spec:
  fields: {f: {type: integer, default: 0}}
  vars: {f: 0}
  steps:
    - {id: a, title: A, outputs: {n: {type: integer}}}
    - id: assign
      set: {fields: {f: "${{ steps.a.outputs.n }}"}}
    - {id: c, title: C, if: "${{ fields.f >= 0 }}"}
    - {id: d, title: D, if: "${{ vars.f >= 0 }}"}"#;
    let mut trace = run_case(spec, &[reported("a", 1), succeeded("c"), succeeded("d")]);
    amend(&mut trace, "a", 2);
    assert!(trace.step("assign").stale);
    assert!(trace.step("c").stale, "field reader");
    assert!(
        !trace.step("d").stale,
        "a var of the same name is unrelated"
    );
}
#[test]
fn amendment_marks_nested_foreach_aggregates() {
    let spec = "spec:\n  inputs:\n    regions: {type: list, items: {type: string}, default: [east]}\n    zones: {type: list, items: {type: string}, default: [a]}\n  steps:\n    - id: regions\n      foreach:\n        items: '${{ inputs.regions }}'\n        as: region\n        key: '${{ region }}'\n        max_items: 1\n        steps:\n          - id: zones\n            foreach:\n              items: '${{ inputs.zones }}'\n              as: zone\n              key: '${{ zone }}'\n              max_items: 1\n              steps:\n                - {id: check, title: Check, outputs: {n: {type: integer}}}\n";
    let mut trace = run_case(spec, &[reported("regions[east].zones[a].check", 1)]);
    assert_eq!(
        trace.state.status,
        ExecutionStatus::Succeeded,
        "{:?}",
        trace.error_codes
    );
    amend(&mut trace, "regions[east].zones[a].check", 2);
    assert!(trace.step("regions[east].zones").stale, "inner aggregate");
    assert!(trace.step("regions").stale, "outer aggregate");
}

// F25: test gaps named by the review.
#[test]
fn a_claimed_step_cannot_be_claimed_again() {
    let mut trace = run_case("spec: {steps: [{id: work, title: Work}]}", &[]);
    let id = trace.step("work").id.clone().unwrap();
    let human = |n: u8| ActorRef::Human {
        user_id: format!("usr_{n:026}").parse().unwrap(),
    };
    trace.append(
        Event::StepClaimed {
            step_run_id: id.clone(),
        },
        human(1),
        1,
    );
    trace.append(Event::StepClaimed { step_run_id: id }, human(2), 2);
    assert_eq!(trace.error_codes, ["step.not_claimable"]);
    assert_eq!(trace.step("work").actor, Some(human(1)));
}
#[test]
fn a_decided_approval_cannot_be_amended() {
    let mut trace = run_document(
        &doc(
            "spec: {steps: [{id: gate, approval: {prompt: Go?, options: [go, stop], approvers: {users: [alice]}}}, {id: after, title: After}]}",
        ),
        &[],
        alice(),
    );
    let vote = alice_vote(&trace, "go", 1);
    submit(&mut trace, vote);
    assert_eq!(trace.step("gate").status, StepStatus::Succeeded);
    let gate = trace.step("gate").id.clone().unwrap();
    let to = runbook_model::StepRecord {
        status: StepStatus::Succeeded,
        verdict: None,
        outputs: [("decision".to_string(), serde_json::json!("stop"))].into(),
        summary: None,
    };
    assert_eq!(
        validate_override(&trace.state, &gate, &to, "wrong call", ActorRef::System)
            .unwrap_err()
            .code(),
        "amendment.approval_forbidden"
    );
}
#[test]
fn an_amendment_cannot_change_a_step_status() {
    let trace = run_case(
        "spec: {steps: [{id: a, title: A}, {id: b, title: B}]}",
        &[succeeded("a")],
    );
    let a = trace.step("a").id.clone().unwrap();
    let to = runbook_model::StepRecord {
        status: StepStatus::Failed,
        verdict: None,
        outputs: Default::default(),
        summary: None,
    };
    assert_eq!(
        validate_override(&trace.state, &a, &to, "it failed", ActorRef::System)
            .unwrap_err()
            .code(),
        "amendment.status_change"
    );
}

// R49: a guard reading a field fed by an amended step is stale.
#[test]
fn guard_on_a_field_fed_by_an_amended_step_is_stale() {
    let spec = r#"spec:
  fields: {sev: {type: integer, default: 0}}
  steps:
    - {id: a, title: A, outputs: {n: {type: integer}}}
    - id: mark
      set: {fields: {sev: "${{ steps.a.outputs.n }}"}}
    - {id: guarded, title: Guarded, if: "${{ fields.sev == 1 }}"}"#;
    let mut trace = run_case(spec, &[reported("a", 1), succeeded("guarded")]);
    assert_eq!(trace.step("guarded").status, StepStatus::Succeeded);
    amend(&mut trace, "a", 2);
    assert!(trace.step("mark").stale, "mark");
    assert!(trace.step("guarded").stale, "guarded");
    assert_eq!(trace.state, trace.replay());
}

// R51: an override's recorded author must be the actor that submits it,
// as votes require (approval.actor_mismatch).
#[test]
fn override_author_must_match_the_submitting_actor() {
    let mut trace = run_case(
        "spec: {steps: [{id: a, title: A, outputs: {n: {type: integer}}}, {id: b, title: B}]}",
        &[reported("a", 1)],
    );
    let human = |n: u8| ActorRef::Human {
        user_id: format!("usr_{n:026}").parse().unwrap(),
    };
    let id = trace.step("a").id.clone().unwrap();
    let to = runbook_model::StepRecord {
        status: StepStatus::Succeeded,
        verdict: None,
        outputs: [("n".to_string(), serde_json::json!(2))].into(),
        summary: None,
    };
    let amendment = validate_override(&trace.state, &id, &to, "fix", human(1)).unwrap();
    let event = Event::StepOverridden {
        step_run_id: id,
        amendment: Box::new(amendment),
    };
    trace.append(event.clone(), human(2), trace.state.at_ms + 1);
    assert_eq!(trace.error_codes, ["amendment.actor_mismatch"]);
    assert!(!trace.step("a").amended);
    trace.append(event, human(1), trace.state.at_ms + 1);
    assert_eq!(trace.error_codes, ["amendment.actor_mismatch"]);
    assert!(trace.step("a").amended);
}

// R50: PRD §6, `waiting` means the run waits only on people or timers.
fn shown(trace: &Trace) -> ExecutionStatus {
    reported_status(&trace.state)
}
#[test]
fn a_run_waiting_only_on_people_or_timers_reports_waiting() {
    // Agent-ready work: running.
    let mut trace = run_document(
        &doc(
            "spec: {steps: [{id: auto, title: Auto, actor: agent}, {id: person, title: Person, actor: human}, {id: pause, wait: {for: 5m}}, {id: gate, approval: {prompt: Go?, options: [go], approvers: {users: [alice]}}}]}",
        ),
        &[],
        alice(),
    );
    assert_eq!(trace.state.status, ExecutionStatus::Running);
    assert_eq!(shown(&trace), ExecutionStatus::Running, "agent work");
    // Agent finishes; a human task is the only open work: waiting.
    let agent = ActorRef::Agent {
        principal_id: "svc_ops".into(),
        session_id: "s1".into(),
        name: "ops".into(),
        model: None,
    };
    let id = trace.step("auto").id.clone().unwrap();
    trace.append(
        Event::StepReported {
            step_run_id: id,
            report: WorkReport {
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
        agent,
        1,
    );
    trace.settle();
    assert_eq!(shown(&trace), ExecutionStatus::Waiting, "human task");
    // A human claims it: still waiting on a person.
    let person = trace.step("person").id.clone().unwrap();
    let alice = ActorRef::Human {
        user_id: "usr_00000000000000000000000001".parse().unwrap(),
    };
    trace.append(
        Event::StepClaimed {
            step_run_id: person.clone(),
        },
        alice.clone(),
        2,
    );
    assert_eq!(
        shown(&trace),
        ExecutionStatus::Waiting,
        "claimed by a person"
    );
    trace.append(
        Event::StepReported {
            step_run_id: person,
            report: WorkReport {
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
        alice.clone(),
        3,
    );
    trace.settle();
    assert_eq!(trace.step("pause").status, StepStatus::Waiting);
    assert_eq!(shown(&trace), ExecutionStatus::Waiting, "timer");
    // A manual pause wins over waiting.
    trace.append(
        Event::ManualPauseChanged { paused: true },
        ActorRef::System,
        4,
    );
    assert_eq!(shown(&trace), ExecutionStatus::Paused);
    trace.append(
        Event::ManualPauseChanged { paused: false },
        ActorRef::System,
        5,
    );
    tick(&mut trace, 300_003);
    assert_eq!(trace.step("gate").status, StepStatus::Waiting);
    assert_eq!(shown(&trace), ExecutionStatus::Waiting, "human approval");
    let vote = alice_vote(&trace, "go", 300_004);
    submit(&mut trace, vote);
    assert_eq!(shown(&trace), ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn agent_decidable_approvals_and_agent_claims_are_running() {
    let mut trace = run_document(
        &doc(
            "spec: {steps: [{id: gate, approval: {prompt: Go?, options: [go], approvers: {users: [alice]}, allow_agents: true, min: 1}}]}",
        ),
        &[],
        alice(),
    );
    assert_eq!(
        shown(&trace),
        ExecutionStatus::Running,
        "an agent may decide"
    );
    let mut claimed = run_case("spec: {steps: [{id: work, title: Work}]}", &[]);
    let id = claimed.step("work").id.clone().unwrap();
    claimed.append(
        Event::StepClaimed { step_run_id: id },
        ActorRef::Agent {
            principal_id: "svc_ops".into(),
            session_id: "s1".into(),
            name: "ops".into(),
            model: None,
        },
        1,
    );
    assert_eq!(shown(&claimed), ExecutionStatus::Running, "agent claim");
    let _ = &mut trace;
}
#[test]
fn runs_before_engine_version_3_never_report_waiting() {
    let new = run_case(
        "spec: {steps: [{id: person, title: Person, actor: human}]}",
        &[],
    );
    assert_eq!(shown(&new), ExecutionStatus::Waiting);
    let mut events = new.events.clone();
    let Event::ExecutionStarted { engine_version, .. } = &mut events[0].event else {
        panic!("first event starts the run")
    };
    *engine_version = 2;
    let state = events
        .iter()
        .try_fold(new.initial.clone(), |state, event| apply(&state, event))
        .unwrap();
    assert_eq!(reported_status(&state), ExecutionStatus::Running);
}
