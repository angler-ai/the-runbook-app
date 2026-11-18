mod support;
use support::*;
#[test]
fn escalation_expires_then_pauses() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    directory.teams.insert(
        "leads".into(),
        "team_00000000000000000000000002".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test, owners: [team:leads]}\nspec: {execution: {timeout: 2h}, steps: [{id: gate, approval: {prompt: Proceed?, options: [rollback], approvers: {teams: [oncall]}, timeout: 30m, escalation_timeout: 30m}}, {id: rollback, title: Rollback}]}",
        &[
            Input::Tick { at_ms: 1_800_000 },
            Input::Tick { at_ms: 3_600_000 },
        ],
        directory,
    );
    assert_eq!(trace.count("ApprovalEscalated"), 1);
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::Expired
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Paused);
    assert_eq!(trace.state, trace.replay());
}
fn expired_gate() -> Trace {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    directory.teams.insert(
        "leads".into(),
        "team_00000000000000000000000002".parse().unwrap(),
    );
    run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test, owners: [team:leads]}\nspec: {execution: {timeout: 2h}, steps: [{id: gate, approval: {prompt: Proceed?, options: [rollback], approvers: {teams: [oncall]}, timeout: 30m, escalation_timeout: 30m}}, {id: rollback, title: Rollback}]}",
        &[Input::Tick { at_ms: 3_600_000 }],
        directory,
    )
}
fn late_vote(trace: &mut Trace, at_ms: i64) {
    let vote = runbook_engine::ResolvedVote {
        at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: runbook_model::ActorRef::Human {
            user_id: "usr_00000000000000000000000003".parse().unwrap(),
        },
        credential_ref: "pat_carol".into(),
        option: "rollback".into(),
        comment: Some("late review".into()),
        eligibility: runbook_engine::VoteEligibility {
            eligible: true,
            directory_revision: 2,
            matched_recipients: vec![runbook_model::RecipientRef::Team(
                "team_00000000000000000000000002".into(),
            )],
        },
    };
    match runbook_engine::apply_vote(&trace.state, &vote) {
        Ok(events) => {
            for event in events {
                trace.append(event, vote.actor.clone(), at_ms);
            }
        }
        Err(e) => trace.error_codes.push(e.code().into()),
    };
    trace.settle();
}
#[test]
fn manual_resume_cannot_bypass_gate() {
    let mut trace = expired_gate();
    trace.append(
        runbook_engine::Event::ManualPauseChanged { paused: false },
        runbook_model::ActorRef::System,
        3_600_001,
    );
    trace.settle();
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Paused);
    assert!(trace.ready.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn late_vote_before_deadline_can_resume() {
    let mut trace = expired_gate();
    late_vote(&mut trace, 5_400_000);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.ready, ["rollback"]);
    assert_eq!(trace.count("ApprovalDecided"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn deadline_wins_at_exact_boundary() {
    for drained in [false, true] {
        let mut trace = expired_gate();
        if drained {
            for command in runbook_engine::advance_time(&trace.state, 7_200_000).unwrap() {
                trace.commit_command(command, 7_200_000);
            }
            trace.settle();
        }
        late_vote(&mut trace, 7_200_000);
        assert_eq!(trace.error_codes, ["execution.deadline_exceeded"]);
        assert_eq!(trace.count("ApprovalDecided"), 0);
    }
}
#[test]
fn late_approval_does_not_remove_manual_pause() {
    let mut trace = expired_gate();
    trace.append(
        runbook_engine::Event::ManualPauseChanged { paused: true },
        runbook_model::ActorRef::System,
        3_600_001,
    );
    late_vote(&mut trace, 5_400_000);
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Paused);
    assert_eq!(trace.state.pause_reasons.len(), 1);
    assert!(
        trace
            .state
            .pause_reasons
            .contains(&runbook_engine::PauseReason::Manual)
    );
    assert!(trace.ready.is_empty());
    trace.append(
        runbook_engine::Event::ManualPauseChanged { paused: false },
        runbook_model::ActorRef::System,
        5_400_001,
    );
    trace.settle();
    assert_eq!(trace.ready, ["rollback"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn task_report_cannot_beat_its_undrained_deadline() {
    let mut trace = run_case("spec: {steps: [{id: work, title: Work, timeout: 1m}]}", &[]);
    trace.append(
        runbook_engine::Event::StepReported {
            step_run_id: trace.step("work").id.clone().unwrap(),
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
        60_000,
    );
    assert_eq!(trace.error_codes, ["step.deadline_exceeded"]);
}
#[test]
fn one_late_tick_queues_notifications_before_terminal_timeout() {
    let mut trace = expired_gate();
    // Replay up to the open gate to simulate a process that slept across all timers.
    trace.state = trace.initial.clone();
    let events = trace.events.clone();
    trace.events.clear();
    trace.commands.clear();
    for envelope in events.into_iter().take(2) {
        trace.append(envelope.event, envelope.actor, envelope.at_ms);
    }
    for _ in 0..20 {
        let commands = runbook_engine::advance_time(&trace.state, 7_200_000).unwrap();
        if commands.is_empty() {
            break;
        }
        for command in commands {
            trace.commit_command(command, 7_200_000);
        }
    }
    trace.settle();
    assert_eq!(trace.count("ApprovalNotificationQueued"), 2);
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
}
#[test]
fn execution_timeout_includes_manual_pause() {
    let mut trace = run_case(
        "spec: {execution: {timeout: 1m}, steps: [{id: work, title: Work}]}",
        &[],
    );
    trace.append(
        runbook_engine::Event::ManualPauseChanged { paused: true },
        runbook_model::ActorRef::System,
        1,
    );
    for command in runbook_engine::advance_time(&trace.state, 60_000).unwrap() {
        trace.commit_command(command, 60_000);
    }
    trace.settle();
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(trace.state.terminal_reason.as_deref(), Some("timeout"));
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn active_reports_and_signals_can_finish_during_manual_pause() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work}, {id: next, title: Next}]}",
        &[],
    );
    trace.append(
        runbook_engine::Event::ManualPauseChanged { paused: true },
        runbook_model::ActorRef::System,
        1,
    );
    trace.append(
        runbook_engine::Event::StepReported {
            step_run_id: trace.step("work").id.clone().unwrap(),
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
        runbook_model::StepStatus::Succeeded
    );
    assert!(trace.step("next").id.is_none());
    let mut signal = run_case(
        "spec: {steps: [{id: gate, wait: {signal: deploy}}, {id: next, title: Next}]}",
        &[],
    );
    signal.append(
        runbook_engine::Event::ManualPauseChanged { paused: true },
        runbook_model::ActorRef::System,
        1,
    );
    send_signal(&mut signal, "deploy", "sig", 2);
    assert_eq!(
        signal.step("gate").status,
        runbook_model::StepStatus::Succeeded
    );
    assert_eq!(signal.state.status, runbook_model::ExecutionStatus::Paused);
    assert!(signal.step("next").id.is_none());
}
#[test]
fn unresolved_handoff_keeps_its_original_task_deadline() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work, timeout: 1m, on_failure: handoff}]}",
        &[],
    );
    trace.append(
        runbook_engine::Event::StepReported {
            step_run_id: trace.step("work").id.clone().unwrap(),
            report: runbook_engine::WorkReport {
                appended_logs: false,
                files: vec![],
                status: runbook_model::ReportStatus::Blocked,
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
        30_000,
    );
    trace.settle();
    assert_eq!(
        trace.step("work").routing_error.as_deref(),
        Some("handoff.no_recipient")
    );
    for command in runbook_engine::advance_time(&trace.state, 60_000).unwrap() {
        trace.commit_command(command, 60_000);
    }
    trace.settle();
    assert_eq!(
        trace.step("work").status,
        runbook_model::StepStatus::TimedOut
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn on_expired_fail_routes_failure_without_a_pause() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test, owners: [user:alice]}\nspec: {steps: [{id: gate, approval: {prompt: Go?, options: [go], approvers: {users: [alice]}, timeout: 1m, escalation_timeout: 1m, on_expired: fail}}]}",
        &[Input::Tick { at_ms: 120_000 }],
        directory,
    );
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::Expired
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert!(trace.state.pause_reasons.is_empty());
    assert_eq!(trace.count("ApprovalNotificationQueued"), 2);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn active_checklist_can_be_completed_during_manual_pause() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work, checklist: [Done]}]}",
        &[],
    );
    trace.append(
        runbook_engine::Event::ManualPauseChanged { paused: true },
        runbook_model::ActorRef::System,
        1,
    );
    trace.append(
        runbook_engine::Event::ChecklistUpdated {
            step_run_id: trace.step("work").id.clone().unwrap(),
            ticks: vec![runbook_model::ChecklistTick {
                title: "Done".into(),
                completed: true,
            }],
        },
        runbook_model::ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        2,
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert!(trace.step("work").checklist[0].completed);
}
#[test]
fn unsupported_auto_policy_does_not_mask_an_earlier_or_equal_deadline() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {execution: {timeout: 1m}, steps: [{id: gate, approval: {prompt: Go?, options: [{id: go, auto: true}], approvers: {users: [alice]}, timeout: 1m, on_timeout: {decide: go}}}]}",
        &[],
        directory,
    );
    assert!(matches!(
        runbook_engine::advance_time(&trace.state, 60_000)
            .unwrap()
            .as_slice(),
        [runbook_engine::Command::ExpireExecution { .. }]
    ));
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: parallel, parallel: [{id: delay, wait: {for: 30s}}, {id: gate, approval: {prompt: Go?, options: [{id: go, auto: true}], approvers: {users: [alice]}, timeout: 1m, on_timeout: {decide: go}}}]}]}",
        &[],
        directory,
    );
    assert!(matches!(
        runbook_engine::advance_time(&trace.state, 60_000)
            .unwrap()
            .as_slice(),
        [runbook_engine::Command::FireWaitTimer { .. }]
    ));
}
