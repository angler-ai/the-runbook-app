mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, RecipientRef};
use support::*;
fn directory() -> runbook_spec::CompileContext {
    let mut context = runbook_spec::CompileContext::default();
    for (name, id) in [("alice", 1), ("bob", 2), ("carol", 3)] {
        context
            .users
            .insert(name.into(), format!("usr_{id:026}").parse().unwrap());
    }
    context.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    context
}
fn document(spec: &str, inputs: &[Input]) -> Trace {
    run_document(
        &format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test, owners: [user:alice]}}\n{spec}"
        ),
        inputs,
        directory(),
    )
}
fn intents(trace: &Trace) -> Vec<NotificationIntent> {
    let mut state = trace.initial.clone();
    trace
        .events
        .iter()
        .flat_map(|event| {
            let before = state.clone();
            state = apply(&state, event).unwrap();
            notifications_for(event, &before, &state)
        })
        .collect()
}
#[test]
fn approval_request_targets_oncall() {
    let mut trace = document(
        "spec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [yes], approvers: {teams: [oncall]}}}]}",
        &[],
    );
    let at = trace
        .events
        .iter()
        .position(|e| matches!(e.event, Event::ApprovalRequested { .. }))
        .unwrap();
    let Event::ApprovalRequested { mut snapshot } = trace.events[at].event.clone() else {
        unreachable!()
    };
    trace.events.truncate(at);
    trace.state = trace.replay();
    snapshot.recipients = [1, 2]
        .map(|id| RecipientRef::User(format!("usr_{id:026}").parse().unwrap()))
        .to_vec();
    snapshot.directory_revision = 42;
    trace.append(Event::ApprovalRequested { snapshot }, ActorRef::System, 0);
    let intent = intents(&trace).pop().unwrap();
    assert_eq!(intent.kind, "approval.requested");
    assert_eq!(
        intent.recipients,
        [1, 2].map(|id| RecipientRef::User(format!("usr_{id:026}").parse().unwrap()))
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn escalation_and_handoff_are_not_agent_work() {
    let escalated = document(
        "spec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [yes], approvers: {users: [alice]}, timeout: 1m, escalate_to: [user:carol], escalation_timeout: 1m}}]}",
        &[Input::Tick { at_ms: 60_000 }],
    );
    let intent = intents(&escalated)
        .into_iter()
        .find(|i| i.kind == "approval.escalated")
        .unwrap();
    assert_eq!(
        intent.recipients,
        [RecipientRef::User(
            "usr_00000000000000000000000003".parse().unwrap()
        )]
    );
    let mut failed = succeeded("work");
    if let Input::Report { status, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
    }
    let handed = document(
        "spec: {defaults: {actor: agent, on_failure: handoff}, steps: [{id: work, title: Work}]}",
        &[failed],
    );
    let intent = intents(&handed)
        .into_iter()
        .find(|i| i.kind == "step.blocked")
        .unwrap();
    assert_eq!(
        intent.recipients,
        [RecipientRef::User(
            "usr_00000000000000000000000001".parse().unwrap()
        )]
    );
    assert_eq!(handed.count("AgentWorkAvailable"), 1);
}
#[test]
fn new_agent_work_has_stable_identity() {
    let mut trace = document(
        "spec: {steps: [{id: gate, approval: {prompt: Rollback?, options: [rollback], approvers: {users: [alice]}}}, {id: rollback, title: Rollback, actor: agent}]}",
        &[],
    );
    let vote = ResolvedVote {
        at_ms: 1,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "tok_public_id".into(),
        option: "rollback".into(),
        comment: None,
        eligibility: VoteEligibility {
            eligible: true,
            directory_revision: 1,
            matched_recipients: vec![RecipientRef::User(
                "usr_00000000000000000000000001".parse().unwrap(),
            )],
        },
    };
    for event in apply_vote(&trace.state, &vote).unwrap() {
        trace.append(event, vote.actor.clone(), 1);
    }
    trace.settle();
    let first = intents(&trace)
        .into_iter()
        .find(|i| i.kind == "agent.work_available")
        .unwrap();
    let replayed = intents(&trace)
        .into_iter()
        .find(|i| i.kind == "agent.work_available")
        .unwrap();
    assert_eq!(first.event_id, replayed.event_id);
    assert_eq!(first.reason.as_deref(), Some("approval_decided"));
    assert_eq!(first.ready_steps[0].path, "rollback");
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn agent_notice_is_once_per_attempt_and_params_stay_opaque() {
    let mut failed = succeeded("work");
    if let Input::Report { status, .. } = &mut failed {
        *status = runbook_model::ReportStatus::Failed;
    }
    let mut trace = document(
        "spec: {agents: {params: {repo: acme/api, literal: '${{ untouched }}'}}, defaults: {actor: agent, retry: {max_attempts: 2, backoff: 1m}}, steps: [{id: work, title: Work}]}",
        &[failed, Input::Tick { at_ms: 60_000 }],
    );
    let all = intents(&trace)
        .into_iter()
        .filter(|i| i.kind == "agent.work_available")
        .collect::<Vec<_>>();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].reason.as_deref(), Some("initial"));
    assert_eq!(all[1].reason.as_deref(), Some("timer"));
    assert_eq!(
        all[0].params,
        Some(serde_json::json!({"repo":"acme/api","literal":"${{ untouched }}"}))
    );
    assert_ne!(all[0].event_id, all[1].event_id);
    trace.settle();
    assert_eq!(trace.count("AgentWorkAvailable"), 2);
    let encoded = serde_json::to_string(&all).unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<NotificationIntent>>(&encoded).unwrap(),
        all
    );
    assert_eq!(trace.state, trace.replay());
}
fn last_work(trace: &Trace) -> NotificationIntent {
    intents(trace)
        .into_iter()
        .rfind(|i| i.kind == "agent.work_available")
        .unwrap()
}
#[test]
fn work_reasons_distinguish_auto_policy_timers_signals_and_cleanup() {
    let automatic = document(
        "spec: {steps: [{id: gate, approval: {prompt: Go?, options: [{id: go, auto: true}], approvers: {users: [alice]}, timeout: 1m, on_timeout: {decide: go}}}, {id: work, title: Work, actor: agent}]}",
        &[Input::Tick { at_ms: 60_000 }],
    );
    assert_eq!(
        last_work(&automatic).reason.as_deref(),
        Some("auto_decision")
    );
    let timer = document(
        "spec: {steps: [{id: wait, wait: {for: 1m}}, {id: work, title: Work, actor: agent}]}",
        &[Input::Tick { at_ms: 60_000 }],
    );
    assert_eq!(last_work(&timer).reason.as_deref(), Some("timer"));
    let mut signal = document(
        "spec: {steps: [{id: wait, wait: {signal: ready}}, {id: work, title: Work, actor: agent}]}",
        &[],
    );
    send_signal(&mut signal, "ready", "signal-1", 1);
    assert_eq!(last_work(&signal).reason.as_deref(), Some("signal"));
    let cleanup = document(
        "spec: {execution: {timeout: 1m}, steps: [{id: work, title: Work, actor: agent}], finally: [{id: clean, title: Clean, actor: agent}]}",
        &[Input::Tick { at_ms: 60_000 }],
    );
    assert_eq!(last_work(&cleanup).reason.as_deref(), Some("cleanup"));
    for trace in [&automatic, &timer, &signal, &cleanup] {
        assert_eq!(trace.state, trace.replay());
    }
}
#[test]
fn human_tasks_and_unrelated_events_do_not_make_agent_intents() {
    let trace = document(
        "spec: {steps: [{id: work, title: Work, actor: human}]}",
        &[],
    );
    assert!(
        intents(&trace)
            .iter()
            .all(|i| i.kind != "agent.work_available")
    );
    let mut event = trace.events.last().unwrap().clone();
    event.execution_id = "exe_00000000000000000000000002".parse().unwrap();
    assert!(notification_for(&event, &trace.state).is_none());
}
#[test]
fn availability_facts_reject_wrong_sequence_actors_and_duplicate_delivery() {
    let trace = document(
        "spec: {steps: [{id: work, title: Work, actor: agent}]}",
        &[],
    );
    let index = trace
        .events
        .iter()
        .position(|e| matches!(e.event, Event::AgentWorkAvailable { .. }))
        .unwrap();
    let before = trace.events[..index]
        .iter()
        .try_fold(trace.initial.clone(), |state, event| apply(&state, event))
        .unwrap();
    let event = trace.events[index].clone();
    let after = apply(&before, &event).unwrap();
    let mut duplicate = event.clone();
    duplicate.seq += 1;
    assert_eq!(
        apply(&after, &duplicate).unwrap_err().code(),
        "notification.invalid_agent_work"
    );
    let mut forged = event.clone();
    forged.actor = ActorRef::Human {
        user_id: "usr_00000000000000000000000001".parse().unwrap(),
    };
    assert_eq!(
        apply(&before, &forged).unwrap_err().code(),
        "event.system_actor_required"
    );
    let Event::AgentWorkAvailable { work } = &mut forged.event else {
        unreachable!()
    };
    work.base_sequence += 1;
    forged.actor = ActorRef::System;
    assert_eq!(
        apply(&before, &forged).unwrap_err().code(),
        "notification.invalid_agent_work"
    );
    assert!(notification_for(&event, &before).is_none());
}
#[test]
fn parallel_agent_readiness_is_emitted_once_for_each_active_task() {
    let mut trace = document(
        "spec: {defaults: {actor: agent}, steps: [{id: fan, parallel: [{id: a, title: A}, {id: b, title: B}]}]}",
        &[],
    );
    assert_eq!(trace.count("AgentWorkAvailable"), 2);
    trace.settle();
    assert_eq!(trace.count("AgentWorkAvailable"), 2);
    let paths = intents(&trace)
        .into_iter()
        .flat_map(|i| i.ready_steps.into_iter().map(|s| s.path))
        .collect::<Vec<_>>();
    assert_eq!(paths, ["a", "b"]);
}
#[test]
fn fail_fast_does_not_announce_work_that_is_about_to_be_cancelled() {
    let mut trace = document(
        "spec: {defaults: {actor: agent}, steps: [{id: fan, fail_fast: true, parallel: [{id: a, title: A}, {id: b, title: B}]}]}",
        &[],
    );
    let index = trace
        .events
        .iter()
        .position(|e| matches!(e.event, Event::AgentWorkAvailable { .. }))
        .unwrap();
    trace.events.truncate(index);
    trace.state = trace.replay();
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("a").id.clone().unwrap(),
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
        ActorRef::System,
        1,
    );
    trace.settle();
    assert_eq!(trace.count("AgentWorkAvailable"), 0);
    assert_eq!(trace.step("b").status, runbook_model::StepStatus::Cancelled);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn notification_snapshots_do_not_grant_approval_eligibility() {
    let trace = document(
        "spec: {steps: [{id: gate, approval: {prompt: Go?, options: [yes], approvers: {users: [alice]}}}]}",
        &[],
    );
    assert_eq!(trace.count("ApprovalRequested"), 1);
    let denied = ResolvedVote {
        at_ms: 1,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "tok_public_id".into(),
        option: "yes".into(),
        comment: None,
        eligibility: VoteEligibility {
            eligible: false,
            directory_revision: 2,
            matched_recipients: vec![],
        },
    };
    assert_eq!(
        apply_vote(&trace.state, &denied).unwrap_err().code(),
        "approval.not_eligible"
    );
}

#[test]
fn lease_expired_reason_matches_webhook_catalog() {
    assert_eq!(WorkReason::LeaseExpiry.as_str(), "lease_expired");
    assert_eq!(
        serde_json::to_value(WorkReason::LeaseExpiry).unwrap(),
        serde_json::json!("lease_expired")
    );
}

#[test]
fn lifecycle_decision_and_completion_events_join_the_catalog() {
    let mut trace = document(
        "spec: {steps: [{id: gate, approval: {prompt: Go?, options: [yes], approvers: {users: [alice]}}}, {id: work, title: Work, actor: agent}]}",
        &[],
    );
    let vote = ResolvedVote {
        at_ms: 1,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "tok_public_id".into(),
        option: "yes".into(),
        comment: None,
        eligibility: VoteEligibility {
            eligible: true,
            directory_revision: 1,
            matched_recipients: vec![RecipientRef::User(
                "usr_00000000000000000000000001".parse().unwrap(),
            )],
        },
    };
    for event in apply_vote(&trace.state, &vote).unwrap() {
        trace.append(event, vote.actor.clone(), 1);
    }
    trace.settle();
    let all = intents(&trace);
    let kinds = all.iter().map(|i| i.kind.as_str()).collect::<Vec<_>>();
    assert_eq!(kinds.first(), Some(&"execution.started"), "{kinds:?}");
    let decided = all.iter().find(|i| i.kind == "approval.decided").unwrap();
    assert_eq!(
        decided.step_run_ids,
        [trace.step("gate").id.clone().unwrap()]
    );
    assert_eq!(decided.detail.as_ref().unwrap()["option"], "yes");
    assert_eq!(
        decided.detail.as_ref().unwrap()["decided_by"],
        "usr_00000000000000000000000001"
    );
    let completed = all.iter().find(|i| i.kind == "step.completed").unwrap();
    assert_eq!(completed.detail.as_ref().unwrap()["path"], "gate");
    // Decision precedes the agent notice it unblocks.
    let decided_at = kinds.iter().position(|k| *k == "approval.decided").unwrap();
    let work_at = kinds
        .iter()
        .position(|k| *k == "agent.work_available")
        .unwrap();
    assert!(decided_at < work_at, "{kinds:?}");
    // Finishing the run announces completion with status and result.
    let mut finished = document(
        "spec: {steps: [{id: work, title: Work, actor: human}]}",
        &[succeeded("work")],
    );
    finished.settle();
    let done = intents(&finished)
        .into_iter()
        .find(|i| i.kind == "execution.completed")
        .unwrap();
    assert_eq!(done.detail.as_ref().unwrap()["status"], "succeeded");
    assert!(done.detail.as_ref().unwrap().get("result").is_some());
    let encoded = serde_json::to_string(&done).unwrap();
    assert_eq!(
        serde_json::from_str::<NotificationIntent>(&encoded).unwrap(),
        done
    );
    // Stored intents written before the catalog grew still decode.
    let mut legacy = serde_json::to_value(&done).unwrap();
    legacy.as_object_mut().unwrap().remove("detail");
    assert_eq!(
        serde_json::from_value::<NotificationIntent>(legacy)
            .unwrap()
            .detail,
        None
    );
}
#[test]
fn escalation_notice_may_expand_its_team_to_users_and_keeps_the_targets() {
    let mut trace = document(
        "spec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [yes], approvers: {users: [alice]}, timeout: 1m, escalate_to: [team:oncall], escalation_timeout: 1m}}]}",
        &[Input::Tick { at_ms: 60_000 }],
    );
    let at = trace
        .events
        .iter()
        .position(|e| matches!(e.event, Event::ApprovalNotificationQueued { .. }))
        .unwrap();
    let Event::ApprovalNotificationQueued { notice } = trace.events[at].event.clone() else {
        unreachable!()
    };
    let team = RecipientRef::Team("team_00000000000000000000000001".into());
    assert_eq!(notice.recipients, std::slice::from_ref(&team));
    trace.events.truncate(at);
    trace.state = trace.replay();
    let user = |n: u8| RecipientRef::User(format!("usr_{n:026}").parse().unwrap());
    // A notice naming another team, or the same user twice, is not this
    // escalation's notice.
    for recipients in [
        vec![RecipientRef::Team("team_00000000000000000000000009".into())],
        vec![user(2), user(2)],
    ] {
        let mut forged = notice.clone();
        forged.recipients = recipients;
        trace.append(
            Event::ApprovalNotificationQueued { notice: forged },
            ActorRef::System,
            60_000,
        );
    }
    assert_eq!(
        trace.error_codes,
        ["notification.not_pending", "notification.not_pending"]
    );
    let mut expanded = notice.clone();
    expanded.recipients = vec![user(2), user(3)];
    expanded.directory_revision = Some(7);
    trace.append(
        Event::ApprovalNotificationQueued { notice: expanded },
        ActorRef::System,
        60_000,
    );
    assert_eq!(trace.error_codes.len(), 2, "{:?}", trace.error_codes);
    let intent = intents(&trace).pop().unwrap();
    assert_eq!(intent.kind, "approval.escalated");
    assert_eq!(intent.recipients, [user(2), user(3)]);
    assert_eq!(intent.detail, Some(serde_json::json!({"targets": [team]})));
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn notice_without_a_team_cannot_add_users() {
    let mut trace = document(
        "spec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [yes], approvers: {users: [alice]}, timeout: 1m, escalate_to: [user:carol], escalation_timeout: 1m}}]}",
        &[Input::Tick { at_ms: 60_000 }],
    );
    let at = trace
        .events
        .iter()
        .position(|e| matches!(e.event, Event::ApprovalNotificationQueued { .. }))
        .unwrap();
    let Event::ApprovalNotificationQueued { mut notice } = trace.events[at].event.clone() else {
        unreachable!()
    };
    trace.events.truncate(at);
    trace.state = trace.replay();
    notice.recipients.push(RecipientRef::User(
        "usr_00000000000000000000000002".parse().unwrap(),
    ));
    trace.append(
        Event::ApprovalNotificationQueued { notice },
        ActorRef::System,
        60_000,
    );
    assert_eq!(trace.error_codes, ["notification.not_pending"]);
}
/// MEM3 (D40): a proposal notifies `memory.proposed` with identifiers only;
/// a memory read notifies nobody.
#[test]
fn memory_proposal_is_a_catalog_notice_and_reads_are_silent() {
    let mut trace = document("spec: {steps: [{id: a, title: A}, {id: b, title: B}]}", &[]);
    let step = trace.step("a").id.clone().unwrap();
    let before = intents(&trace).len();
    trace.append(
        Event::MemoryRead {
            step_run_id: step.clone(),
            refs: vec![],
        },
        ActorRef::System,
        5,
    );
    assert_eq!(intents(&trace).len(), before);
    trace.append(
        Event::MemoryProposed {
            step_run_id: step.clone(),
            memory_id: "mem_00000000000000000000000001".parse().unwrap(),
        },
        ActorRef::System,
        6,
    );
    let intent = intents(&trace).pop().unwrap();
    assert_eq!(intent.kind, "memory.proposed");
    assert_eq!(intent.step_run_ids, std::slice::from_ref(&step));
    assert_eq!(
        intent.detail,
        Some(serde_json::json!({"memory_id": "mem_00000000000000000000000001"}))
    );
    assert!(intent.recipients.is_empty());
    // MEM6: a live save is a `memory.saved` notice, identifiers only.
    trace.append(
        Event::MemorySaved {
            step_run_id: step.clone(),
            memory_id: "mem_00000000000000000000000002".parse().unwrap(),
        },
        ActorRef::System,
        7,
    );
    let saved = intents(&trace).pop().unwrap();
    assert_eq!(saved.kind, "memory.saved");
    // MEM6 review fix 3: a run's edit is a `memory.edited` notice.
    trace.append(
        Event::MemoryEdited {
            step_run_id: step.clone(),
            memory_id: "mem_00000000000000000000000002".parse().unwrap(),
        },
        ActorRef::System,
        8,
    );
    let edited = intents(&trace).pop().unwrap();
    assert_eq!(edited.kind, "memory.edited");
    assert_eq!(edited.detail, saved.detail);
    let intent = saved;
    assert_eq!(intent.step_run_ids, [step]);
    assert_eq!(
        intent.detail,
        Some(serde_json::json!({"memory_id": "mem_00000000000000000000000002"}))
    );
    assert!(intent.recipients.is_empty());
}
