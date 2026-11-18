mod support;
use support::*;
#[test]
fn auto_timeout_is_never_human_signature() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Roll back?, options: [{id: rollback, auto: true}], approvers: {users: [alice]}, timeout: 30m, on_timeout: {decide: rollback}}}, {id: rollback, title: Rollback, destructive: true, allow_auto_decision: true}]}",
        &[Input::Tick { at_ms: 1_800_000 }],
        directory,
    );
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(trace.step("gate").outputs["decided_by"], "system");
    assert!(
        trace
            .event_names()
            .windows(2)
            .any(|w| w == ["ApprovalExpired", "DecisionApplied"])
    );
    assert_eq!(trace.ready, ["rollback"]);
    assert_eq!(trace.state, trace.replay());
}
fn automatic_fixture(opt_in: bool) -> Trace {
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    run_document(
        &format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test}}\nspec: {{execution: {{timeout: 2h}}, steps: [{{id: gate, approval: {{prompt: Roll back?, options: [{{id: rollback, auto: true}}], approvers: {{users: [alice]}}, timeout: 30m, on_timeout: {{decide: rollback}}}}}}, {{id: rollback, title: Rollback, destructive: true, allow_auto_decision: {opt_in}}}]}}"
        ),
        &[],
        directory,
    )
}
fn policy(trace: &Trace) -> runbook_engine::TimeoutPolicy {
    runbook_engine::TimeoutPolicy {
        step_run_id: trace.step("gate").id.clone().unwrap(),
        deadline_at_ms: 1_800_000,
        at_ms: 1_800_000,
    }
}
#[test]
fn draft_cannot_bypass_automatic_destructive_policy() {
    let mut trace = automatic_fixture(false);
    // F5: the policy is checked when the decision is made, and a rejected
    // policy withholds the decision instead of wedging the run.
    assert!(matches!(
        runbook_engine::apply_automatic_decision(&trace.state, &policy(&trace))
            .unwrap()
            .as_slice(),
        [runbook_engine::Event::AutomaticDecisionWithheld { reason, .. }]
            if reason == "approval.invalid_automatic_policy"
    ));
    for at_ms in [1_800_000, 7_200_000] {
        for _ in 0..10 {
            let commands = runbook_engine::advance_time(&trace.state, at_ms).unwrap();
            if commands.is_empty() {
                break;
            }
            for command in commands {
                trace.commit_command(command, at_ms);
            }
        }
        trace.settle();
        assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    }
    assert_eq!(trace.count("DecisionApplied"), 0);
    assert_eq!(trace.count("AutomaticDecisionWithheld"), 1);
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::Expired
    );
    assert!(trace.step("rollback").id.is_none());
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(trace.state.terminal_reason.as_deref(), Some("timeout"));
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn recorded_automatic_decision_replays_under_a_stricter_policy() {
    // F5: replay never re-runs today's policy code. Events recorded while
    // the policy allowed the decision fold over a definition it now rejects.
    let mut recorded = automatic_fixture(true);
    for command in runbook_engine::advance_time(&recorded.state, 1_800_000).unwrap() {
        recorded.commit_command(command, 1_800_000);
    }
    recorded.settle();
    assert_eq!(recorded.count("DecisionApplied"), 1);
    let stricter = automatic_fixture(false).initial;
    let replayed = recorded
        .events
        .iter()
        .try_fold(stricter, |state, event| {
            runbook_engine::apply(&state, event)
        })
        .unwrap();
    assert_eq!(replayed.steps, recorded.state.steps);
    assert_eq!(replayed.status, recorded.state.status);
}
#[test]
fn automatic_batch_is_system_only_and_rejects_interleaving_or_changed_option() {
    use runbook_engine::*;
    use runbook_model::ActorRef;
    let mut trace = automatic_fixture(true);
    let events = apply_automatic_decision(&trace.state, &policy(&trace)).unwrap();
    assert_eq!(events.len(), 2);
    trace.append(
        events[0].clone(),
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        1_800_000,
    );
    assert_eq!(
        trace.error_codes.last().unwrap(),
        "event.system_actor_required"
    );
    trace.append(events[0].clone(), ActorRef::System, 1_800_000);
    trace.append(
        Event::ManualPauseChanged { paused: true },
        ActorRef::System,
        1_800_000,
    );
    assert_eq!(
        trace.error_codes.last().unwrap(),
        "approval.automatic_pending"
    );
    let mut changed = events[1].clone();
    let Event::DecisionApplied {
        decision: AppliedDecision::Automatic(decision),
    } = &mut changed
    else {
        unreachable!()
    };
    decision.option = "keep".into();
    trace.append(changed, ActorRef::System, 1_800_000);
    assert_eq!(
        trace.error_codes.last().unwrap(),
        "approval.invalid_automatic_decision"
    );
    trace.append(events[1].clone(), ActorRef::System, 1_800_000);
    trace.settle();
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(trace.count("DecisionApplied"), 1);
    assert!(
        trace
            .step("gate")
            .approval
            .as_ref()
            .unwrap()
            .votes
            .is_empty()
    );
    assert_eq!(trace.step("gate").actor, Some(ActorRef::System));
    let value = serde_json::to_value(&events[1]).unwrap();
    assert_eq!(value["decision"]["by"], "system");
    assert_eq!(value["decision"]["policy"], "on_timeout");
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn automatic_decision_does_not_remove_manual_pause() {
    let mut trace = automatic_fixture(true);
    trace.append(
        runbook_engine::Event::ManualPauseChanged { paused: true },
        runbook_model::ActorRef::System,
        1,
    );
    for command in runbook_engine::advance_time(&trace.state, 1_800_000).unwrap() {
        trace.commit_command(command, 1_800_000);
    }
    trace.settle();
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Paused);
    assert!(trace.step("rollback").id.is_none());
    assert_eq!(trace.count("DecisionApplied"), 1);
    assert_eq!(trace.count("ApprovalNotificationQueued"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn late_timer_drain_records_policy_before_timeout_without_starting_remediation() {
    let mut trace = automatic_fixture(true);
    for _ in 0..10 {
        let commands = runbook_engine::advance_time(&trace.state, 7_200_000).unwrap();
        if commands.is_empty() {
            break;
        }
        for command in commands {
            trace.commit_command(command, 7_200_000);
        }
    }
    trace.settle();
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(trace.state.terminal_reason.as_deref(), Some("timeout"));
    assert!(trace.step("rollback").id.is_none());
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(trace.count("DecisionApplied"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn interrupted_automatic_batch_resumes_only_its_system_decision() {
    let mut trace = automatic_fixture(true);
    let events = runbook_engine::apply_automatic_decision(&trace.state, &policy(&trace)).unwrap();
    trace.append(
        events[0].clone(),
        runbook_model::ActorRef::System,
        1_800_000,
    );
    let commands = runbook_engine::advance_time(&trace.state, 1_800_001).unwrap();
    assert_eq!(commands.len(), 1);
    trace.commit_command(commands[0].clone(), 1_800_001);
    trace.settle();
    assert_eq!(trace.count("ApprovalExpired"), 1);
    assert_eq!(trace.count("DecisionApplied"), 1);
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn cleanup_waits_for_dispatched_actor() {
    let spec = "spec: {execution: {timeout: 2h, cleanup_timeout: 30m}, steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify, timeout: 15m}]}";
    let during = run_case(spec, &[Input::Tick { at_ms: 7_200_000 }]);
    assert_eq!(during.state.phase, runbook_engine::ExecutionPhase::Cleanup);
    assert_eq!(during.ready, ["notify"]);
    let mut report = succeeded("notify");
    let Input::Report { at_ms, .. } = &mut report else {
        unreachable!()
    };
    *at_ms = Some(7_800_000);
    let after = run_case(spec, &[Input::Tick { at_ms: 7_200_000 }, report]);
    assert_eq!(after.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(after.state.terminal_reason.as_deref(), Some("timeout"));
    assert_eq!(after.state, after.replay());
}
#[test]
fn failure_runs_group_then_top_finally() {
    let mut failed = succeeded("work");
    let Input::Report { status, .. } = &mut failed else {
        unreachable!()
    };
    *status = runbook_model::ReportStatus::Failed;
    let trace = run_case(
        "spec: {steps: [{id: group, steps: [{id: work, title: Work}], finally: [{id: clean_a, title: Clean}]}], finally: [{id: notify, title: Notify}]}",
        &[failed, succeeded("clean_a"), succeeded("notify")],
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(
        trace.step("clean_a").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_eq!(
        trace.step("notify").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn cancel_retains_cancelled_outcome() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify}]}",
        &[],
    );
    trace.append(
        runbook_engine::Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        runbook_model::ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        1,
    );
    trace.settle();
    assert_eq!(trace.ready, ["notify"]);
    trace.append(
        runbook_engine::Event::StepReported {
            step_run_id: trace.step("notify").id.clone().unwrap(),
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
        runbook_model::ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        2,
    );
    trace.settle();
    assert_eq!(
        trace.step("work").status,
        runbook_model::StepStatus::Cancelled
    );
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Cancelled
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn hard_step_cap_suppresses_cleanup() {
    let tasks = (0..100)
        .map(|i| format!("{{id: work_{i}, title: Work}}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test}}\nspec: {{execution: {{timeout: 2h}}, steps: [{tasks}], finally: [{{id: notify, title: Notify}}]}}"
    );
    let mut inputs = (0..99)
        .map(|i| succeeded(&format!("work_{i}")))
        .collect::<Vec<_>>();
    inputs.push(Input::Tick { at_ms: 7_200_000 });
    let trace = run_document_with_start(
        &source,
        &inputs,
        Default::default(),
        runbook_engine::StartContext {
            limits: runbook_engine::ExecutionLimits {
                max_step_runs: Some(100),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(trace.count("CleanupSuppressed"), 1);
    assert_eq!(trace.state.materialized_steps, 100);
    assert!(trace.step("notify").id.is_none());
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn fail_fast_unwinds_cancelled_group_before_recovered_work() {
    let mut failed = succeeded("bad");
    if let Input::Report { status, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
    }
    let spec = "spec: {steps: [{id: outer, steps: [{id: fan, fail_fast: true, parallel: [{id: bad, title: Bad}, {id: sibling, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}]}], catch: [{id: recover, title: Recover}]}, {id: after, title: After}]}";
    let during = run_case(spec, &[failed.clone()]);
    assert_eq!(during.ready, ["clean"]);
    let trace = run_case(
        spec,
        &[
            failed,
            succeeded("clean"),
            succeeded("recover"),
            succeeded("after"),
        ],
    );
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(
        trace.step("sibling").status,
        runbook_model::StepStatus::Cancelled
    );
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn cleanup_deadline_and_task_deadline_use_the_earlier_bound() {
    for (task_timeout, tick, expected_suppression) in [("15m", 8_100_000, 0), ("1h", 9_000_000, 1)]
    {
        let spec = format!(
            "spec: {{execution: {{timeout: 2h, cleanup_timeout: 30m}}, steps: [{{id: work, title: Work}}], finally: [{{id: notify, title: Notify, timeout: {task_timeout}}}]}}"
        );
        let mut late = succeeded("notify");
        if let Input::Report { at_ms, .. } = &mut late {
            *at_ms = Some(tick);
        }
        let rejected = run_case(&spec, &[Input::Tick { at_ms: 7_200_000 }, late]);
        assert_eq!(rejected.error_codes.len(), 1);
        let trace = run_case(
            &spec,
            &[
                Input::Tick { at_ms: 7_200_000 },
                Input::Tick { at_ms: tick },
            ],
        );
        assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
        assert_eq!(trace.count("CleanupSuppressed"), expected_suppression);
        assert_eq!(trace.state, trace.replay());
    }
}
#[test]
fn cleanup_cannot_extend_the_runtime_hard_deadline() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify}]}";
    let trace = run_document_with_start(
        source,
        &[Input::Tick { at_ms: 100 }],
        Default::default(),
        runbook_engine::StartContext {
            limits: runbook_engine::ExecutionLimits {
                hard_deadline_at_ms: Some(100),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert!(trace.step("notify").id.is_none());
    assert_eq!(
        trace.state.cleanup.as_ref().unwrap().suppressed.as_deref(),
        Some("limit.execution_duration")
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn cleanup_rejects_forged_cause_and_early_suppression() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify}]}",
        &[],
    );
    let before = trace.state.clone();
    trace.append(
        runbook_engine::Event::CleanupStarted {
            cause: runbook_engine::TerminalCause::Succeeded,
            reason: None,
        },
        runbook_model::ActorRef::System,
        1,
    );
    assert_eq!(trace.state, before);
    assert_eq!(trace.error_codes, ["cleanup.invalid_cause"]);
    trace.append(
        runbook_engine::Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        runbook_model::ActorRef::System,
        1,
    );
    trace.settle();
    let before = trace.state.clone();
    for reason in [
        "cleanup.timeout",
        "limit.step_runs_exceeded",
        "limit.execution_duration",
        "arbitrary",
    ] {
        trace.append(
            runbook_engine::Event::CleanupSuppressed {
                reason: reason.into(),
            },
            runbook_model::ActorRef::System,
            2,
        );
        assert_eq!(trace.state, before);
        assert_eq!(
            trace.error_codes.last().unwrap(),
            "cleanup.invalid_suppression"
        );
    }
}
#[test]
fn parallel_materialization_cannot_overbook_the_cap() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: fan, parallel: [{id: skip, if: '${{ false }}', title: Skip}, {id: a, title: A}, {id: b, title: B}]}], finally: [{id: notify, title: Notify}]}";
    let trace = run_document_with_start(
        source,
        &[],
        Default::default(),
        runbook_engine::StartContext {
            limits: runbook_engine::ExecutionLimits {
                max_step_runs: Some(2),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(trace.state.materialized_steps, 2);
    assert_eq!(
        trace.step("skip").status,
        runbook_model::StepStatus::Skipped
    );
    assert!(trace.step("b").id.is_none());
    assert_eq!(trace.count("StepLimitExceeded"), 1);
    assert_eq!(trace.count("CleanupSuppressed"), 1);
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn failure_during_cleanup_does_not_skip_outer_finally() {
    let mut failed = succeeded("clean_a");
    if let Input::Report { status, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
    }
    let trace = run_case(
        "spec: {steps: [{id: group, steps: [{id: work, title: Work}], finally: [{id: clean_a, title: Clean}]}], finally: [{id: notify, title: Notify}]}",
        &[succeeded("work"), failed, succeeded("notify")],
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(
        trace.step("notify").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn cancellation_preserves_started_finally_and_unwinds_inner_before_outer() {
    let mut trace = run_case(
        "spec: {steps: [{id: outer, steps: [{id: inner, steps: [{id: work, title: Work}], finally: [{id: clean_inner, title: Clean}]}], finally: [{id: clean_outer, title: Clean}]}], finally: [{id: notify, title: Notify}]}",
        &[succeeded("work")],
    );
    let cleanup_id = trace.step("clean_inner").id.clone();
    trace.append(
        runbook_engine::Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        runbook_model::ActorRef::System,
        1,
    );
    trace.settle();
    assert_eq!(trace.ready, ["clean_inner"]);
    assert_eq!(trace.step("clean_inner").id, cleanup_id);
    for name in ["clean_inner", "clean_outer", "notify"] {
        report_ok(&mut trace, name, 2);
    }
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Cancelled
    );
    assert_eq!(trace.state, trace.replay());
}
fn report_ok(trace: &mut Trace, name: &str, at_ms: i64) {
    trace.append(
        runbook_engine::Event::StepReported {
            step_run_id: trace.step(name).id.clone().unwrap(),
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
        runbook_model::ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        at_ms,
    );
    trace.settle();
}
#[test]
fn fail_fast_preserves_already_started_sibling_finally() {
    let mut failed = succeeded("bad");
    if let Input::Report { status, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
    }
    let trace = run_case(
        "spec: {steps: [{id: fan, fail_fast: true, parallel: [{id: bad, title: Bad}, {id: sibling, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}]}], finally: [{id: notify, title: Notify}]}",
        &[
            succeeded("work"),
            failed,
            succeeded("clean"),
            succeeded("notify"),
        ],
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(
        trace.step("clean").status,
        runbook_model::StepStatus::Succeeded
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn fail_fast_inside_top_cleanup_unwinds_its_cancelled_group() {
    let mut failed = succeeded("bad");
    if let Input::Report { status, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
    }
    let trace = run_case(
        "spec: {steps: [], finally: [{id: fan, fail_fast: true, parallel: [{id: bad, title: Bad}, {id: sibling, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}]}]}",
        &[failed],
    );
    assert_eq!(trace.ready, ["clean"]);
}

#[test]
fn cancellation_with_nested_finally_failure_still_reaches_top_cleanup() {
    let mut trace = run_case(
        "spec: {steps: [{id: outer, steps: [{id: work, title: Work}], finally: [{id: fan, fail_fast: true, parallel: [{id: bad, title: Bad}, {id: sibling, steps: [{id: busy, title: Busy}], finally: [{id: clean, title: Clean}]}]}]}], finally: [{id: notify, title: Notify}]}",
        &[],
    );
    trace.append(
        runbook_engine::Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        runbook_model::ActorRef::System,
        1,
    );
    trace.settle();
    trace.append(
        runbook_engine::Event::StepReported {
            step_run_id: trace.step("bad").id.clone().unwrap(),
            report: runbook_engine::WorkReport {
                appended_logs: false,
                files: vec![],
                status: runbook_model::ReportStatus::Failed,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        runbook_model::ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        2,
    );
    trace.settle();
    assert_eq!(trace.ready, ["clean"]);
    report_ok(&mut trace, "clean", 3);
    report_ok(&mut trace, "notify", 4);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Cancelled
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn unhandled_group_failure_enters_bounded_cleanup_before_finally() {
    let mut failed = succeeded("work");
    if let Input::Report { status, at_ms, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
        *at_ms = Some(1);
    }
    for body in [
        "[{id: group, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}]",
        "[{id: outer, steps: [{id: group, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}], finally: [{id: outer_clean, title: Clean}]}]",
    ] {
        let spec =
            format!("spec: {{execution: {{timeout: 2h, cleanup_timeout: 30m}}, steps: {body}}}");
        let during = run_case(&spec, &[failed.clone()]);
        assert_eq!(during.state.phase, runbook_engine::ExecutionPhase::Cleanup);
        assert_eq!(during.ready, ["clean"]);
        let after = run_case(&spec, &[failed.clone(), Input::Tick { at_ms: 1_800_001 }]);
        assert_eq!(after.count("CleanupSuppressed"), 1);
        assert_eq!(after.state.status, runbook_model::ExecutionStatus::Failed);
        assert_eq!(after.state, after.replay());
    }
}

#[test]
fn handled_group_failure_keeps_ordinary_continuation_after_finally() {
    let mut failed = succeeded("work");
    if let Input::Report { status, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
    }
    for spec in [
        "spec: {steps: [{id: outer, steps: [{id: group, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}], catch: [{id: recover, title: Recover}]}]}",
        "spec: {steps: [{id: group, on_failure: continue, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}, {id: recover, title: Recover}]}",
    ] {
        let during = run_case(spec, &[failed.clone()]);
        assert_eq!(during.state.phase, runbook_engine::ExecutionPhase::Ordinary);
        assert_eq!(during.ready, ["clean"]);
        let after = run_case(
            spec,
            &[failed.clone(), succeeded("clean"), succeeded("recover")],
        );
        assert_eq!(
            after.state.status,
            runbook_model::ExecutionStatus::Succeeded
        );
        assert_eq!(after.state, after.replay());
    }
}
