//! Exact rejection codes for fenced facts whose original tests only asserted
//! `is_err()` (approvals.rs `vote_facts_are_fenced_to_actor_gate_sequence_and_option`,
//! handoff.rs `handoff_snapshot_and_notification_are_fenced`, waits.rs timer
//! fencing, presentation.rs checklist-after-completion). `is_err()` also
//! passes on an invariant failure or an unrelated earlier check, so each
//! mutation here must be refused for its own reason.
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, RecipientRef, ReportStatus};
use support::*;

/// An expected rejection code and the edit that should cause it.
type Mutation<T> = (&'static str, fn(&mut T));

fn code<T>(result: Result<T, EngineError>) -> &'static str {
    match result {
        Ok(_) => "accepted",
        Err(error) => error.code(),
    }
}

#[test]
fn each_vote_fact_mutation_has_its_own_code() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [keep, rollback], approvers: {teams: [oncall]}, min: 2}}, {id: next, title: Next}]}",
        &[],
        directory,
    );
    let good = ResolvedVote {
        at_ms: trace.state.at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "pat".into(),
        option: "keep".into(),
        comment: None,
        eligibility: VoteEligibility {
            directory_revision: 1,
            eligible: true,
            matched_recipients: vec![RecipientRef::Team("team_00000000000000000000000001".into())],
        },
    };
    let mutations: [Mutation<ResolvedVote>; 6] = [
        ("event.sequence_conflict", |v| v.base_sequence -= 1),
        ("approval.invalid_option", |v| v.option = "unknown".into()),
        ("approval.not_eligible", |v| {
            v.eligibility.directory_revision = 0
        }),
        ("approval.not_eligible", |v| {
            v.eligibility.matched_recipients.clear()
        }),
        ("approval.not_eligible", |v| {
            v.eligibility.matched_recipients =
                vec![RecipientRef::Team("team_00000000000000000000000099".into())]
        }),
        ("approval.missing_credential", |v| v.credential_ref.clear()),
    ];
    assert_eq!(code(apply_vote(&trace.state, &good)), "accepted");
    for (expected, mutate) in mutations {
        let mut input = good.clone();
        mutate(&mut input);
        assert_eq!(
            code(apply_vote(&trace.state, &input)),
            expected,
            "{input:?}"
        );
        // The stored fact is fenced the same way when it reaches the reducer.
        let envelope = EventEnvelope {
            execution_id: trace.state.id.clone(),
            seq: trace.state.sequence + 1,
            at_ms: input.at_ms,
            actor: input.actor.clone(),
            event: Event::ApprovalVoted {
                vote: Box::new(input),
            },
        };
        assert_eq!(code(apply(&trace.state, &envelope)), expected);
    }
}

#[test]
fn each_handoff_snapshot_mutation_has_its_own_code() {
    let mut context = runbook_spec::CompileContext::default();
    context.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let mut failed = succeeded("a");
    if let Input::Report { status, actor, .. } = &mut failed {
        *status = ReportStatus::Failed;
        *actor = ActorRef::Agent {
            principal_id: "service-1".into(),
            session_id: "session-1".into(),
            name: "test".into(),
            model: None,
        };
    }
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test, owners: [user:alice]}\nspec: {steps: [{id: a, title: A, on_failure: handoff}]}",
        &[failed],
        context,
    );
    let index = trace
        .events
        .iter()
        .position(|e| matches!(e.event, Event::StepHandedOff { .. }))
        .expect("handoff happened");
    let before = replay_stored(
        trace.state.compiled(),
        &trace.state.id,
        &trace.events[..index],
    );
    let genuine = trace.events[index].clone();
    assert_eq!(code(apply(&before, &genuine)), "accepted");
    let mutations: [Mutation<ResolvedHandoff>; 4] = [
        ("event.sequence_conflict", |h| h.request.base_sequence -= 1),
        ("handoff.not_pending", |h| h.request.attempt += 1),
        ("handoff.invalid_resolution", |h| h.directory_revision = 0),
        ("handoff.invalid_resolution", |h| {
            h.source = HandoffSource::Unresolved
        }),
    ];
    for (expected, mutate) in mutations {
        let mut bad = genuine.clone();
        let Event::StepHandedOff { handoff } = &mut bad.event else {
            unreachable!()
        };
        mutate(handoff);
        assert_eq!(code(apply(&before, &bad)), expected, "{bad:?}");
    }
}

#[test]
fn each_wait_timer_mutation_has_its_own_code() {
    let trace = run_case("spec: {steps: [{id: gate, wait: {for: 1m}}]}", &[]);
    let input = WaitInput::Timer {
        execution_id: trace.state.id.clone(),
        step_run_id: trace.step("gate").id.clone().unwrap(),
        generation: 1,
        at_ms: 60_000,
    };
    assert_eq!(code(advance_wait(&trace.state, &input)), "accepted");
    let mutations: [Mutation<WaitInput>; 3] = [
        ("event.wrong_execution", |w| {
            if let WaitInput::Timer { execution_id, .. } = w {
                *execution_id = "exe_00000000000000000000000002".parse().unwrap()
            }
        }),
        ("timer.stale_generation", |w| {
            if let WaitInput::Timer { generation, .. } = w {
                *generation = 2
            }
        }),
        ("timer.not_due", |w| {
            if let WaitInput::Timer { at_ms, .. } = w {
                *at_ms = 59_999
            }
        }),
    ];
    for (expected, mutate) in mutations {
        let mut bad = input.clone();
        mutate(&mut bad);
        assert_eq!(code(advance_wait(&trace.state, &bad)), expected, "{bad:?}");
    }
}

#[test]
fn checklist_cannot_change_after_completion() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work, checklist: [One, Two]}]}",
        &[],
    );
    let id = trace.step("work").id.clone().unwrap();
    let tick = update_checklist(&trace.state, &id, &[true, true]).unwrap();
    trace.append(tick, ActorRef::System, 1);
    trace.append(
        Event::StepReported {
            step_run_id: id.clone(),
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Succeeded,
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
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(
        code(update_checklist(&trace.state, &id, &[false, false])),
        "execution.not_running"
    );
}
