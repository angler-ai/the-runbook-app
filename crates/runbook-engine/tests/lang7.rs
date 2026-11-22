//! LANG7: strict assignment resolves the assignee when the step becomes
//! ready (routing error when it names nobody, never broader), and a person
//! taking over an agent task is recorded after the claim.
mod support;
use runbook_engine::{Event, HandoffSource};
use runbook_model::{ActorKind, ActorRef, ReportStatus, StepStatus};
use support::*;

const ALICE: &str = "usr_00000000000000000000000001";

fn human() -> ActorRef {
    ActorRef::Human {
        user_id: ALICE.parse().unwrap(),
    }
}
fn agent() -> ActorRef {
    ActorRef::Agent {
        principal_id: "service-1".into(),
        session_id: "session-1".into(),
        name: "test".into(),
        model: None,
    }
}
fn addressed(spec: &str, inputs: &[Input]) -> Trace {
    let mut context = runbook_spec::CompileContext::default();
    context.users.insert("alice".into(), ALICE.parse().unwrap());
    context.users.insert(
        "owner".into(),
        "usr_00000000000000000000000003".parse().unwrap(),
    );
    run_document(
        &format!("metadata: {{id: test, owners: [user:owner]}}\n{spec}"),
        inputs,
        context,
    )
}

#[test]
fn a_strict_assignee_is_resolved_when_the_step_becomes_ready() {
    let trace = addressed(
        "spec: {steps: [{id: a, assignee: user:alice, assignment: strict}]}",
        &[],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("StepAssigneeResolved"), 1);
    let step = trace.step("a");
    assert_eq!(step.status, StepStatus::Ready);
    let resolved = step.strict_assignee.as_ref().unwrap();
    assert_eq!(
        resolved.eligible_recipients,
        [runbook_model::RecipientRef::User(ALICE.parse().unwrap())]
    );
    assert!(resolved.notification_recipients.is_empty());
    assert_eq!(step.routing_error, None);
    assert_eq!(trace.count("StepBlockedNotificationQueued"), 0);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_guidance_assignee_needs_no_resolution() {
    let trace = addressed("spec: {steps: [{id: a, assignee: user:alice}]}", &[]);
    assert_eq!(trace.count("StepAssigneeResolved"), 0);
    assert!(trace.step("a").strict_assignee.is_none());
}

#[test]
fn a_strict_assignee_naming_nobody_is_a_routing_error_for_admins() {
    let trace = addressed(
        "spec:\n  inputs: {owner: {type: string, default: 'user:nobody'}}\n  steps: [{id: a, assignee: '${{ inputs.owner }}', assignment: strict}]",
        &[],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let step = trace.step("a");
    assert_eq!(step.routing_error.as_deref(), Some("handoff.no_recipient"));
    let resolved = step.strict_assignee.as_ref().unwrap();
    assert!(resolved.unresolved());
    assert_eq!(resolved.notification_recipients.len(), 1);
    assert_eq!(trace.count("StepBlockedNotificationQueued"), 1);
    assert!(trace.commands.iter().any(|c| matches!(c,
        runbook_engine::Command::NotifyStepBlocked { notification }
            if notification.routing_error.as_deref() == Some("handoff.no_recipient"))));
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn an_expression_that_names_no_recipient_resolves_to_nobody() {
    let trace = addressed(
        "spec:\n  inputs: {owner: {type: string, default: 'alice'}}\n  steps: [{id: a, assignee: '${{ inputs.owner }}', assignment: strict}]",
        &[],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let resolved = trace.step("a").strict_assignee.as_ref().unwrap();
    assert_eq!(resolved.request.assignee, None);
    assert!(resolved.unresolved());
}

#[test]
fn each_attempt_resolves_its_assignee_again() {
    let mut failed = succeeded("a");
    let Input::Report { status, .. } = &mut failed else {
        unreachable!()
    };
    *status = ReportStatus::Failed;
    let trace = addressed(
        "spec: {steps: [{id: a, assignee: user:alice, assignment: strict, retry: {max_attempts: 2}}]}",
        &[failed],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.count("StepAssigneeResolved"), 2);
    assert_eq!(
        trace
            .step("a")
            .strict_assignee
            .as_ref()
            .unwrap()
            .request
            .attempt,
        2
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_strict_handoff_never_falls_back_to_the_owners() {
    let mut blocked = succeeded("a");
    let Input::Report { status, actor, .. } = &mut blocked else {
        unreachable!()
    };
    *status = ReportStatus::Blocked;
    *actor = agent();
    let strict = addressed(
        "spec:\n  inputs: {owner: {type: string, default: 'user:nobody'}}\n  steps: [{id: a, assignee: '${{ inputs.owner }}', assignment: strict}]",
        std::slice::from_ref(&blocked),
    );
    assert!(strict.error_codes.is_empty(), "{:?}", strict.error_codes);
    let handoff = strict.step("a").handoff.as_ref().unwrap();
    assert!(handoff.request.strict);
    assert_eq!(handoff.source, HandoffSource::Unresolved);
    assert_eq!(
        strict.step("a").routing_error.as_deref(),
        Some("handoff.no_recipient")
    );
    assert_eq!(strict.state, strict.replay());
    // The same runbook without `strict` hands off to the owners.
    let guidance = addressed(
        "spec:\n  inputs: {owner: {type: string, default: 'user:nobody'}}\n  steps: [{id: a, assignee: '${{ inputs.owner }}'}]",
        &[blocked],
    );
    let handoff = guidance.step("a").handoff.as_ref().unwrap();
    assert!(!handoff.request.strict);
    assert_eq!(handoff.source, HandoffSource::Owners);
}

#[test]
fn a_person_takes_over_a_ready_agent_task() {
    let mut trace = addressed("spec: {steps: [{id: a, actor: agent}]}", &[]);
    let id = trace.step("a").id.clone().unwrap();
    trace.append(
        Event::StepClaimed {
            step_run_id: id.clone(),
            agent_version_id: None,
            claim_id: None,
        },
        human(),
        10,
    );
    trace.append(
        Event::StepTakenOver {
            step_run_id: id.clone(),
            previous_actor_kind: ActorKind::Agent,
        },
        human(),
        10,
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let taken = trace.step("a").taken_over.clone().unwrap();
    assert_eq!(taken.by, human());
    assert_eq!(taken.at_ms, 10);
    assert_eq!(taken.previous_actor_kind, ActorKind::Agent);
    // Recorded once per claim.
    trace.append(
        Event::StepTakenOver {
            step_run_id: id.clone(),
            previous_actor_kind: ActorKind::Agent,
        },
        human(),
        11,
    );
    assert_eq!(trace.error_codes, ["step.not_taken_over"]);
    // The person completes the step; the record keeps the take-over.
    trace.append(
        Event::StepReported {
            step_run_id: id,
            report: runbook_engine::WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Default::default(),
                summary: Some("done by hand".into()),
                evidence: vec![],
                logs: vec![],
            },
        },
        human(),
        20,
    );
    trace.settle();
    assert_eq!(trace.error_codes, ["step.not_taken_over"]);
    assert_eq!(trace.step("a").status, StepStatus::Succeeded);
    assert!(trace.step("a").taken_over.is_some());
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_take_over_is_only_a_persons_claim_of_an_agent_task() {
    for (spec, claimant, taker, kind) in [
        // Not an agent task.
        (
            "spec: {steps: [{id: a, actor: any}]}",
            human(),
            human(),
            ActorKind::Agent,
        ),
        // Not a person.
        (
            "spec: {steps: [{id: a, actor: agent}]}",
            agent(),
            agent(),
            ActorKind::Agent,
        ),
        // Not the claimant.
        (
            "spec: {steps: [{id: a, actor: agent}]}",
            human(),
            ActorRef::Human {
                user_id: "usr_00000000000000000000000002".parse().unwrap(),
            },
            ActorKind::Agent,
        ),
        // Not from an agent.
        (
            "spec: {steps: [{id: a, actor: agent}]}",
            human(),
            human(),
            ActorKind::Human,
        ),
    ] {
        let mut trace = addressed(spec, &[]);
        let id = trace.step("a").id.clone().unwrap();
        trace.append(
            Event::StepClaimed {
                step_run_id: id.clone(),
                agent_version_id: None,
                claim_id: None,
            },
            claimant,
            10,
        );
        trace.append(
            Event::StepTakenOver {
                step_run_id: id,
                previous_actor_kind: kind,
            },
            taker,
            10,
        );
        assert_eq!(trace.error_codes, ["step.not_taken_over"], "{spec}");
    }
}

#[test]
fn a_released_take_over_is_cleared() {
    let mut trace = addressed("spec: {steps: [{id: a, actor: agent}]}", &[]);
    let id = trace.step("a").id.clone().unwrap();
    trace.append(
        Event::StepClaimed {
            step_run_id: id.clone(),
            agent_version_id: None,
            claim_id: None,
        },
        human(),
        10,
    );
    trace.append(
        Event::StepTakenOver {
            step_run_id: id.clone(),
            previous_actor_kind: ActorKind::Agent,
        },
        human(),
        10,
    );
    trace.append(
        Event::StepClaimReleased {
            step_run_id: id,
            reason: None,
        },
        human(),
        11,
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("a").status, StepStatus::Ready);
    assert!(trace.step("a").taken_over.is_none());
    assert_eq!(trace.state, trace.replay());
}

// LANG7 review round 1.

/// Strict `actor: any` work belongs to its assignee, a person: agents are
/// not told about it and the run waits on people.
#[test]
fn agents_are_not_offered_a_strict_task_open_to_anyone() {
    let trace = addressed(
        "spec: {steps: [{id: a, actor: any, assignee: user:alice, assignment: strict}]}",
        &[],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("a").status, StepStatus::Ready);
    assert_eq!(trace.count("AgentWorkAvailable"), 0);
    assert_eq!(
        runbook_engine::reported_status(&trace.state),
        runbook_model::ExecutionStatus::Waiting
    );
}

/// `actor` still governs agents on a strict agent task, and a guidance
/// assignee never limits agents.
#[test]
fn agents_are_still_offered_strict_agent_and_guidance_tasks() {
    for spec in [
        "spec: {steps: [{id: a, actor: agent, assignee: user:alice, assignment: strict}]}",
        "spec: {steps: [{id: a, actor: any, assignee: user:alice}]}",
    ] {
        let trace = addressed(spec, &[]);
        assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
        assert_eq!(trace.count("AgentWorkAvailable"), 1, "{spec}");
        assert_eq!(
            runbook_engine::reported_status(&trace.state),
            runbook_model::ExecutionStatus::Running,
            "{spec}"
        );
    }
}

fn reresolution(trace: &Trace, eligible: Vec<runbook_model::RecipientRef>) -> Event {
    let recorded = trace.step("a").strict_assignee.clone().unwrap();
    Event::StepAssigneeResolved {
        resolution: Box::new(runbook_engine::ResolvedAssignee {
            request: runbook_engine::AssigneeRequest {
                base_sequence: trace.state.sequence,
                ..recorded.request
            },
            eligible_recipients: eligible,
            notification_recipients: vec![],
            directory_revision: 2,
        }),
    }
}

/// An assignee that resolved to nobody may be resolved again for the same
/// attempt (the host does it when someone claims after the directory
/// changed); the new resolution clears the routing error. A resolved one
/// is never replaced, and a re-resolution must name somebody.
#[test]
fn an_unresolved_strict_assignee_may_be_resolved_again() {
    let mut trace = addressed(
        "spec:\n  inputs: {owner: {type: string, default: 'user:nobody'}}\n  steps: [{id: a, assignee: '${{ inputs.owner }}', assignment: strict}]",
        &[],
    );
    let step = trace.step("a");
    assert!(step.strict_assignee.as_ref().unwrap().unresolved());
    let empty = reresolution(&trace, vec![]);
    trace.append(empty, ActorRef::System, 10);
    assert_eq!(trace.error_codes, ["assignment.not_pending"]);
    let alice = runbook_model::RecipientRef::User(ALICE.parse().unwrap());
    let found = reresolution(&trace, vec![alice.clone()]);
    trace.append(found, ActorRef::System, 11);
    assert_eq!(trace.error_codes, ["assignment.not_pending"]);
    let step = trace.step("a");
    let resolved = step.strict_assignee.as_ref().unwrap();
    assert_eq!(resolved.eligible_recipients, std::slice::from_ref(&alice));
    assert_eq!(resolved.directory_revision, 2);
    assert_eq!(step.routing_error, None);
    let again = reresolution(&trace, vec![alice]);
    trace.append(again, ActorRef::System, 12);
    assert_eq!(
        trace.error_codes,
        ["assignment.not_pending", "assignment.not_pending"]
    );
    assert_eq!(trace.state, trace.replay());
}
