mod support;
use runbook_model::{ActorRef, ReportStatus, StepStatus};
use support::*;
fn agent_report(step: &str, status: ReportStatus) -> Input {
    let mut input = succeeded(step);
    let Input::Report {
        status: recorded,
        actor,
        ..
    } = &mut input
    else {
        unreachable!()
    };
    *recorded = status;
    *actor = ActorRef::Agent {
        principal_id: "service-1".into(),
        session_id: "session-1".into(),
        name: "test".into(),
        model: None,
        service_attribution: None,
    };
    input
}
const RETRY: &str = "spec: {defaults: {actor: agent, on_failure: handoff, retry: {max_attempts: 2, backoff: 1m}}, steps: [{id: a, title: A}]}";
#[test]
fn retry_precedes_handoff() {
    let trace = run_case(RETRY, &[agent_report("a", ReportStatus::Failed)]);
    assert_eq!(trace.count("RetryScheduled"), 1);
    assert_eq!(trace.count("StepHandedOff"), 0);
    assert_eq!(trace.step("a").status, StepStatus::Waiting);
    assert_eq!(trace.state, trace.replay());
}
fn addressed(spec: &str, inputs: &[Input]) -> Trace {
    let mut context = runbook_spec::CompileContext::default();
    context.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    run_document(
        &format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test, owners: [user:alice]}}\n{spec}"
        ),
        inputs,
        context,
    )
}
fn human_failure(step: &str) -> Input {
    let mut input = succeeded(step);
    let Input::Report { status, .. } = &mut input else {
        unreachable!()
    };
    *status = ReportStatus::Failed;
    input
}
#[test]
fn human_failure_does_not_handoff_again() {
    let trace = addressed(
        RETRY,
        &[
            agent_report("a", ReportStatus::Failed),
            Input::Tick { at_ms: 60_000 },
            agent_report("a", ReportStatus::Failed),
            human_failure("a"),
        ],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("StepHandedOff"), 1);
    assert_eq!(trace.count("RetryScheduled"), 1);
    assert_eq!(
        trace.state.pending_terminal_cause,
        Some(runbook_engine::TerminalCause::Failed)
    );
    assert_eq!(trace.step("a").attempt_history.len(), 3);
    assert_eq!(trace.step("a").attempt, 3);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn blocked_hands_off_without_retry() {
    let spec = "spec: {steps: [{id: a, title: A, retry: {max_attempts: 3}}]}";
    let mut trace = addressed(spec, &[]);
    trace.append(
        runbook_engine::Event::StepReported {
            step_run_id: trace.step("a").id.clone().unwrap(),
            report: runbook_engine::WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Blocked,
                verdict: None,
                outputs: Default::default(),
                summary: Some("needs_access".into()),
                evidence: vec![],
                logs: vec![],
            },
        },
        ActorRef::Agent {
            principal_id: "service-1".into(),
            session_id: "session-1".into(),
            name: "test".into(),
            model: None,
            service_attribution: None,
        },
        123,
    );
    trace.settle();
    assert_eq!(trace.count("StepHandedOff"), 1);
    assert_eq!(trace.count("RetryScheduled"), 0);
    assert_eq!(trace.count("StepBlockedNotificationQueued"), 1);
    assert_eq!(
        trace.step("a").attempt_history[0].report.summary.as_deref(),
        Some("needs_access")
    );
    assert!(trace.commands.iter().any(|c| matches!(c, runbook_engine::Command::NotifyStepBlocked {notification} if notification.event_type() == "step.blocked" && notification.recipients.len() == 1)));
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn unresolved_handoff_stays_blocked_and_notifies_admins_without_eligibility() {
    let trace = run_case(
        "spec: {steps: [{id: a, title: A}]}",
        &[agent_report("a", ReportStatus::Blocked), succeeded("a")],
    );
    assert_eq!(trace.error_codes, ["step.not_reportable"]);
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Running);
    assert_eq!(trace.step("a").status, StepStatus::Waiting);
    assert_eq!(
        trace.step("a").routing_error.as_deref(),
        Some("handoff.no_recipient")
    );
    let handoff = trace.step("a").handoff.as_ref().unwrap();
    assert!(handoff.eligible_recipients.is_empty());
    assert_eq!(handoff.source, runbook_engine::HandoffSource::Unresolved);
    assert_eq!(handoff.notification_recipients.len(), 1);
    assert_eq!(trace.count("StepBlockedNotificationQueued"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn human_continuation_rejects_agents_and_success_preserves_failed_attempt() {
    let trace = addressed(
        "spec: {steps: [{id: a, title: A, on_failure: handoff}]}",
        &[
            agent_report("a", ReportStatus::Failed),
            agent_report("a", ReportStatus::Succeeded),
            succeeded("a"),
        ],
    );
    assert_eq!(trace.error_codes, ["step.human_required"]);
    assert_eq!(trace.step("a").attempt_history.len(), 2);
    assert_eq!(
        trace.step("a").attempt_history[0].report.status,
        ReportStatus::Failed
    );
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn human_failure_propagates_even_when_original_task_said_continue() {
    let trace = addressed(
        "spec: {steps: [{id: a, title: A, on_failure: continue}, {id: b, title: B}]}",
        &[agent_report("a", ReportStatus::Blocked), human_failure("a")],
    );
    assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
    assert!(trace.step("b").id.is_none());
}
#[test]
fn retry_timer_rejects_early_stale_and_duplicate_fires() {
    use runbook_engine::Event;
    let mut trace = run_case(RETRY, &[agent_report("a", ReportStatus::Failed)]);
    let event = Event::TimerFired {
        step_run_id: trace.step("a").id.clone().unwrap(),
        generation: 1,
    };
    trace.append(event.clone(), ActorRef::System, 59_999);
    assert_eq!(trace.error_codes, ["timer.not_due"]);
    trace.append(
        Event::TimerFired {
            step_run_id: trace.step("a").id.clone().unwrap(),
            generation: 2,
        },
        ActorRef::System,
        60_000,
    );
    assert_eq!(trace.error_codes.last().unwrap(), "timer.stale_generation");
    trace.append(event.clone(), ActorRef::System, 60_000);
    assert_eq!(trace.step("a").attempt, 2);
    trace.append(event, ActorRef::System, 60_001);
    assert_eq!(trace.error_codes.last().unwrap(), "timer.stale_generation");
    assert_eq!(trace.step("a").attempt_history.len(), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn retry_without_delay_reuses_run_id_and_continues_after_exhaustion() {
    let trace = run_case(
        "spec: {steps: [{id: a, title: A, retry: {max_attempts: 2}, on_failure: continue}, {id: b, title: B}]}",
        &[
            agent_report("a", ReportStatus::Failed),
            agent_report("a", ReportStatus::Failed),
            succeeded("b"),
        ],
    );
    assert_eq!(trace.count("RetryScheduled"), 1);
    assert_eq!(trace.count("StepReady"), 2);
    assert_eq!(trace.step("a").attempt_history.len(), 2);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn dynamic_assignee_falls_back_to_owners_and_persists_reads() {
    let trace = addressed(
        "spec: {vars: {target: 'user:missing'}, steps: [{id: a, title: A, assignee: '${{ vars.target }}', on_failure: handoff}]}",
        &[agent_report("a", ReportStatus::Failed)],
    );
    let handoff = trace.step("a").handoff.as_ref().unwrap();
    assert_eq!(handoff.source, runbook_engine::HandoffSource::Owners);
    assert_eq!(
        handoff.request.assignee,
        Some(runbook_spec::ir::Recipient::User("missing".into()))
    );
    assert!(
        handoff
            .request
            .reads
            .iter()
            .any(|r| r.origin.path == "vars.target")
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn static_assignee_is_preferred_to_owners() {
    let trace = addressed(
        "spec: {steps: [{id: a, title: A, assignee: user:alice, on_failure: handoff}]}",
        &[agent_report("a", ReportStatus::Failed)],
    );
    assert_eq!(
        trace.step("a").handoff.as_ref().unwrap().source,
        runbook_engine::HandoffSource::Assignee
    );
}
#[test]
fn handoff_snapshot_and_notification_are_fenced() {
    use runbook_engine::{Event, apply};
    let trace = addressed(
        "spec: {steps: [{id: a, title: A, on_failure: handoff}]}",
        &[agent_report("a", ReportStatus::Failed)],
    );
    let mut state = trace.initial.clone();
    let mut found = false;
    for envelope in &trace.events {
        if let Event::StepHandedOff { handoff } = &envelope.event {
            found = true;
            for mutation in 0..4 {
                let mut bad = envelope.clone();
                let Event::StepHandedOff { handoff: changed } = &mut bad.event else {
                    unreachable!()
                };
                match mutation {
                    0 => changed.request.base_sequence -= 1,
                    1 => changed.request.attempt += 1,
                    2 => changed.directory_revision = 0,
                    _ => changed.source = runbook_engine::HandoffSource::Unresolved,
                }
                assert!(apply(&state, &bad).is_err());
            }
            assert!(!handoff.eligible_recipients.is_empty());
        }
        state = apply(&state, envelope).unwrap();
    }
    assert!(found);
    let mut duplicate = trace
        .events
        .iter()
        .find(|e| matches!(e.event, Event::StepBlockedNotificationQueued { .. }))
        .unwrap()
        .clone();
    duplicate.seq = state.sequence + 1;
    assert_eq!(
        apply(&state, &duplicate).unwrap_err().code(),
        "notification.not_pending"
    );
}
#[test]
fn failed_check_attempt_has_no_fabricated_finding() {
    let trace = addressed(
        "spec: {steps: [{id: a, title: Check, check: true, on_failure: handoff}]}",
        &[agent_report("a", ReportStatus::Failed)],
    );
    assert_eq!(trace.step("a").verdict, None);
    assert_eq!(trace.step("a").attempt_history[0].report.verdict, None);
}
