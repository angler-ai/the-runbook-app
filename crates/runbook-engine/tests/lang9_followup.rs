//! LANG9: `remind_after` / `remind_every` remind a step's people while it is
//! left open (ready, claimed or waiting), and `escalate_after` /
//! `escalate_to` escalate it once. Both count from when the step opened,
//! never fire while the run is paused, restart when the step opens again,
//! and replay exactly.
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, ExecutionStatus, RecipientRef};
use support::*;

const ALICE: &str = "usr_00000000000000000000000001";
const CAROL: &str = "usr_00000000000000000000000003";

fn directory() -> runbook_spec::CompileContext {
    let mut context = runbook_spec::CompileContext::default();
    for (name, id) in [("alice", 1), ("bob", 2), ("carol", 3)] {
        context
            .users
            .insert(name.into(), format!("usr_{id:026}").parse().unwrap());
    }
    context
}

fn document(spec: &str) -> Trace {
    let trace = run_document(
        &format!("metadata: {{id: test, owners: [user:bob]}}\n{spec}"),
        &[],
        directory(),
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    trace
}

fn tick(trace: &mut Trace, at_ms: i64) {
    for _ in 0..50 {
        let commands = advance_time(&trace.state, at_ms).unwrap();
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

fn intents(trace: &Trace, kind: &str) -> Vec<NotificationIntent> {
    let mut state = trace.initial.clone();
    trace
        .events
        .iter()
        .flat_map(|event| {
            let before = state.clone();
            state = apply(&state, event).unwrap();
            notifications_for(event, &before, &state)
        })
        .filter(|i| i.kind == kind)
        .collect()
}

fn user(id: &str) -> RecipientRef {
    RecipientRef::User(id.parse().unwrap())
}

const REMINDING: &str = "spec: {steps: [{id: work, title: Work, assignee: user:alice, remind_after: 1h, remind_every: 30m}]}";

#[test]
fn a_reminder_is_due_after_remind_after_and_repeats_every() {
    let mut trace = document(REMINDING);
    assert_eq!(next_wakeup(&trace.state).unwrap(), Some(3_600_000));
    tick(&mut trace, 3_599_999);
    assert_eq!(trace.count("StepReminded"), 0);
    tick(&mut trace, 3_600_000);
    assert_eq!(trace.count("StepReminded"), 1);
    assert_eq!(next_wakeup(&trace.state).unwrap(), Some(5_400_000));
    tick(&mut trace, 5_400_000);
    assert_eq!(trace.count("StepReminded"), 2);
    let reminders = intents(&trace, "step.reminder");
    assert_eq!(reminders.len(), 2);
    assert_eq!(reminders[0].recipients, [user(ALICE)]);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn overdue_reminders_send_once() {
    let mut trace = document(REMINDING);
    // Nothing ran for a day: one reminder, and the next is in the future.
    tick(&mut trace, 86_400_000);
    assert_eq!(trace.count("StepReminded"), 1);
    assert!(next_wakeup(&trace.state).unwrap().unwrap() > 86_400_000);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_single_reminder_without_remind_every() {
    let mut trace = document("spec: {steps: [{id: work, title: Work, remind_after: 1h}]}");
    tick(&mut trace, 3_600_000);
    tick(&mut trace, 86_400_000);
    assert_eq!(trace.count("StepReminded"), 1);
    // No assignee and nobody else: the owners are reminded.
    assert_eq!(
        intents(&trace, "step.reminder")[0].recipients,
        [user("usr_00000000000000000000000002")]
    );
    assert_eq!(next_wakeup(&trace.state).unwrap(), None);
}

#[test]
fn a_paused_run_does_not_remind_or_escalate() {
    let mut trace = document(
        "spec: {steps: [{id: work, title: Work, remind_after: 1h, escalate_after: 2h, escalate_to: [user:carol]}]}",
    );
    let alice = ActorRef::Human {
        user_id: ALICE.parse().unwrap(),
    };
    trace.append(Event::ManualPauseChanged { paused: true }, alice, 0);
    trace.settle();
    assert_eq!(trace.state.status, ExecutionStatus::Paused);
    tick(&mut trace, 86_400_000);
    assert_eq!(trace.count("StepReminded"), 0);
    assert_eq!(trace.count("StepEscalated"), 0);
}

#[test]
fn escalation_records_its_recipients_once_and_notifies_them() {
    let mut trace = document(
        "spec: {steps: [{id: work, title: Work, assignee: user:alice, escalate_after: 4h, escalate_to: [user:carol]}]}",
    );
    assert_eq!(next_wakeup(&trace.state).unwrap(), Some(14_400_000));
    tick(&mut trace, 14_400_000);
    assert_eq!(trace.count("StepEscalated"), 1);
    let escalation = trace.step("work").followup_escalation.clone().unwrap();
    assert_eq!(escalation.recipients, [user(CAROL)]);
    tick(&mut trace, 86_400_000);
    assert_eq!(trace.count("StepEscalated"), 1, "escalates once");
    assert_eq!(
        intents(&trace, "step.escalated")[0].recipients,
        [user(CAROL)]
    );
    // Later reminders include the escalation.
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn an_escalation_that_is_not_due_or_does_not_match_is_refused() {
    let trace = document(
        "spec: {steps: [{id: work, title: Work, escalate_after: 4h, escalate_to: [user:carol]}]}",
    );
    let step = trace.step("work");
    let request = EscalationRequest {
        base_sequence: trace.state.sequence,
        step_run_id: step.id.clone().unwrap(),
        deadline_at_ms: 14_400_000,
        targets: vec![user(CAROL)],
    };
    let event = |recipients: Vec<RecipientRef>, deadline_at_ms: i64| Event::StepEscalated {
        snapshot: EscalationSnapshot {
            request: EscalationRequest {
                deadline_at_ms,
                ..request.clone()
            },
            recipients,
            directory_revision: 1,
        },
    };
    let refused = |event: Event, at_ms: i64| {
        let envelope = EventEnvelope {
            execution_id: trace.state.id.clone(),
            seq: trace.state.sequence + 1,
            at_ms,
            actor: ActorRef::System,
            event,
        };
        apply(&trace.state, &envelope)
            .unwrap_err()
            .code()
            .to_owned()
    };
    for (event, at_ms) in [
        (event(vec![user(CAROL)], 14_400_000), 1_000),
        (event(vec![user(CAROL)], 60_000), 14_400_000),
        (event(vec![user(ALICE)], 14_400_000), 14_400_000),
    ] {
        assert_eq!(refused(event, at_ms), "step.invalid_escalation");
    }
    let reminder = Event::StepReminded {
        step_run_id: step.id.clone().unwrap(),
        due_at_ms: 0,
    };
    assert_eq!(refused(reminder, 0), "step.invalid_reminder");
}

#[test]
fn a_signal_wait_reminds_and_escalates_while_waiting() {
    let mut trace = document(
        "spec: {steps: [{id: shop, wait: {signal: shopify_connected}, remind_after: 1d, escalate_after: 3d, escalate_to: [user:carol]}, {id: next, title: Next}]}",
    );
    tick(&mut trace, 86_400_000);
    assert_eq!(trace.count("StepReminded"), 1);
    tick(&mut trace, 3 * 86_400_000);
    assert_eq!(trace.count("StepEscalated"), 1);
    send_signal(&mut trace, "shopify_connected", "sig-1", 3 * 86_400_000 + 1);
    // Done waiting: nothing more is due for it.
    assert!(
        trace.step("shop").followup_escalation.is_some(),
        "the record stays"
    );
    let before = trace.count("StepReminded");
    tick(&mut trace, 30 * 86_400_000);
    assert_eq!(trace.count("StepReminded"), before);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn reopening_a_step_restarts_its_follow_up() {
    let mut trace = document(
        "spec: {steps: [{id: work, title: Work, actor: agent, remind_after: 1h, retry: {max_attempts: 2, backoff: 1m}}]}",
    );
    tick(&mut trace, 3_600_000);
    assert_eq!(trace.count("StepReminded"), 1);
    // The attempt fails; the retry reopens the step later.
    let id = trace.step("work").id.clone().unwrap();
    trace.append(
        Event::StepReported {
            step_run_id: id,
            report: WorkReport {
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
        ActorRef::Agent {
            principal_id: "service-1".into(),
            session_id: "session-1".into(),
            name: "test".into(),
            model: None,
        },
        3_700_000,
    );
    trace.settle();
    tick(&mut trace, 3_760_000);
    let step = trace.step("work");
    assert_eq!(step.attempt, 2);
    assert_eq!(step.opened_at_ms, Some(3_760_000));
    assert_eq!(step.reminders_sent, 0);
    assert_eq!(
        next_wakeup(&trace.state).unwrap(),
        Some(3_760_000 + 3_600_000)
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_step_without_follow_up_records_nothing_new() {
    let mut trace = document("spec: {steps: [{id: work, title: Work}]}");
    tick(&mut trace, 86_400_000);
    assert_eq!(trace.count("StepReminded"), 0);
    assert_eq!(next_wakeup(&trace.state).unwrap(), None);
}
