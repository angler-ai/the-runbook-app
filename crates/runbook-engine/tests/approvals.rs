mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, RecipientRef};
use support::*;
fn gate() -> Trace {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [keep, rollback], approvers: {teams: [oncall]}, min: 2}}, {id: next, title: Next}]}",
        &[],
        directory,
    )
}
fn vote(trace: &Trace, user: u32, token: &str, option: &str) -> ResolvedVote {
    ResolvedVote {
        at_ms: trace.state.at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: format!("usr_{user:026}").parse().unwrap(),
        },
        credential_ref: token.into(),
        option: option.into(),
        comment: Some("reviewed".into()),
        eligibility: VoteEligibility {
            directory_revision: 1,
            eligible: true,
            matched_recipients: vec![RecipientRef::Team("team_00000000000000000000000001".into())],
        },
    }
}
fn submit(trace: &mut Trace, vote: ResolvedVote) {
    match apply_vote(&trace.state, &vote) {
        Ok(events) => {
            for event in events {
                trace.append(event, vote.actor.clone(), trace.state.at_ms);
            }
        }
        Err(error) => trace.error_codes.push(error.code().into()),
    }
    trace.settle();
}
#[test]
fn two_tokens_do_not_make_two_voters() {
    let mut trace = gate();
    let first = vote(&trace, 1, "pat_first", "keep");
    submit(&mut trace, first);
    let duplicate = vote(&trace, 1, "pat_second", "keep");
    submit(&mut trace, duplicate);
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(
        trace.step("gate").approval.as_ref().unwrap().distinct_votes,
        1
    );
    assert_eq!(trace.count("ApprovalVoted"), 1);
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn two_distinct_matching_votes_decide() {
    let mut trace = gate();
    let first = vote(&trace, 1, "pat_alice", "keep");
    submit(&mut trace, first);
    let second = vote(&trace, 2, "pat_bob", "keep");
    submit(&mut trace, second);
    assert_eq!(trace.count("ApprovalDecided"), 1);
    assert_eq!(
        trace.step("gate").outputs["decision"],
        serde_json::json!("keep")
    );
    assert_eq!(
        trace.step("gate").outputs["comment"],
        serde_json::json!("reviewed")
    );
    assert_eq!(
        trace.step("gate").outputs["decided_by"],
        serde_json::json!("usr_00000000000000000000000002")
    );
    assert_eq!(trace.ready, ["next"]);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn owners_are_default_escalation_target() {
    let owners = [RecipientRef::Team("oncall".into())];
    assert_eq!(
        runbook_spec::resolve_escalation(&[], &owners).unwrap(),
        owners
    );
    assert_eq!(
        runbook_spec::resolve_escalation(&[], &[]).unwrap_err().code,
        "approval.no_escalation_target"
    );
}
#[test]
fn ineligible_vote_is_not_a_signature() {
    let mut trace = gate();
    let mut denied = vote(&trace, 3, "pat_unrelated", "keep");
    denied.eligibility.eligible = false;
    submit(&mut trace, denied);
    assert_eq!(trace.error_codes, ["approval.not_eligible"]);
    assert_eq!(trace.count("ApprovalVoted"), 0);
    assert_eq!(trace.count("ApprovalDecided"), 0);
}
#[test]
fn service_principal_vote_is_not_a_signature() {
    let mut trace = gate();
    let mut denied = vote(&trace, 1, "svc_token", "keep");
    denied.actor = ActorRef::Agent {
        principal_id: "svc_runner".into(),
        session_id: "session".into(),
        name: "agent".into(),
        model: None,
    };
    submit(&mut trace, denied);
    assert_eq!(trace.error_codes, ["approval.not_eligible"]);
    assert_eq!(trace.count("ApprovalVoted"), 0);
}
#[test]
fn different_options_do_not_combine_quorum() {
    let mut trace = gate();
    for (user, option) in [(1, "keep"), (2, "rollback")] {
        let input = vote(&trace, user, "pat", option);
        submit(&mut trace, input);
    }
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(
        trace.step("gate").approval.as_ref().unwrap().distinct_votes,
        2
    );
    let input = vote(&trace, 3, "pat", "rollback");
    submit(&mut trace, input);
    assert_eq!(trace.count("ApprovalDecided"), 1);
    assert_eq!(trace.step("gate").outputs["decision"], "rollback");
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn removal_prevents_new_votes_but_keeps_prior_signatures() {
    let mut trace = gate();
    let input = vote(&trace, 1, "pat_a", "keep");
    submit(&mut trace, input);
    let mut denied = vote(&trace, 1, "pat_b", "keep");
    denied.eligibility.directory_revision = 2;
    denied.eligibility.eligible = false;
    submit(&mut trace, denied);
    assert_eq!(trace.error_codes, ["approval.not_eligible"]);
    let mut input = vote(&trace, 2, "pat_c", "keep");
    input.eligibility.directory_revision = 3;
    submit(&mut trace, input);
    let record = trace.step("gate").approval.as_ref().unwrap();
    assert_eq!(record.votes.len(), 2);
    assert_eq!(record.votes[0].vote.eligibility.directory_revision, 1);
    assert_eq!(record.votes[1].vote.eligibility.directory_revision, 3);
    assert_eq!(trace.count("ApprovalDecided"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn duplicate_team_matches_still_count_one_human() {
    let mut trace = gate();
    let mut input = vote(&trace, 1, "pat_a", "keep");
    input
        .eligibility
        .matched_recipients
        .push(input.eligibility.matched_recipients[0].clone());
    submit(&mut trace, input);
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(
        trace.step("gate").approval.as_ref().unwrap().distinct_votes,
        1
    );
}
#[test]
fn vote_facts_are_fenced_to_actor_gate_sequence_and_option() {
    let trace = gate();
    for mutation in 0..6 {
        let mut input = vote(&trace, 1, "pat", "keep");
        match mutation {
            0 => input.base_sequence -= 1,
            1 => input.option = "unknown".into(),
            2 => input.eligibility.directory_revision = 0,
            3 => input.eligibility.matched_recipients.clear(),
            4 => {
                input.eligibility.matched_recipients =
                    vec![RecipientRef::Team("team_00000000000000000000000099".into())]
            }
            _ => input.credential_ref.clear(),
        }
        assert!(apply_vote(&trace.state, &input).is_err());
        let envelope = EventEnvelope {
            execution_id: trace.state.id.clone(),
            seq: trace.state.sequence + 1,
            at_ms: 5,
            actor: input.actor.clone(),
            event: Event::ApprovalVoted {
                vote: Box::new(input),
            },
        };
        assert!(apply(&trace.state, &envelope).is_err());
    }
    let input = vote(&trace, 1, "pat", "keep");
    let envelope = EventEnvelope {
        execution_id: trace.state.id.clone(),
        seq: trace.state.sequence + 1,
        at_ms: 5,
        actor: ActorRef::System,
        event: Event::ApprovalVoted {
            vote: Box::new(input),
        },
    };
    assert_eq!(
        apply(&trace.state, &envelope).unwrap_err().code(),
        "approval.actor_mismatch"
    );
}
#[test]
fn decision_requires_exact_recorded_quorum() {
    let mut trace = gate();
    for user in [1, 2] {
        let input = vote(&trace, user, "pat", "keep");
        submit(&mut trace, input);
    }
    let mut state = trace.initial.clone();
    for envelope in &trace.events {
        if matches!(envelope.event, Event::ApprovalDecided { .. }) {
            for mutation in 0..4 {
                let mut altered = envelope.clone();
                let Event::ApprovalDecided { decision } = &mut altered.event else {
                    unreachable!()
                };
                match mutation {
                    0 => decision.base_sequence -= 1,
                    1 => decision.option = "rollback".into(),
                    2 => decision.signers.pop().map(|_| ()).unwrap(),
                    _ => decision.decided_by = "usr_00000000000000000000000099".parse().unwrap(),
                }
                assert_eq!(
                    apply(&state, &altered).unwrap_err().code(),
                    "approval.invalid_decision"
                );
            }
        }
        state = apply(&state, envelope).unwrap();
    }
    let input = vote(&trace, 3, "pat_late", "rollback");
    assert_eq!(
        apply_vote(&trace.state, &input).unwrap_err().code(),
        "approval.not_open"
    );
}
#[test]
fn user_match_cannot_sign_as_another_user() {
    let mut context = runbook_spec::CompileContext::default();
    context.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [keep], approvers: {users: [alice]}}}]}",
        &[],
        context,
    );
    let mut denied = vote(&trace, 2, "pat_bob", "keep");
    denied.eligibility.matched_recipients = vec![RecipientRef::User(
        "usr_00000000000000000000000001".parse().unwrap(),
    )];
    submit(&mut trace, denied);
    assert_eq!(trace.error_codes, ["approval.not_eligible"]);
    let mut allowed = vote(&trace, 1, "pat_alice", "keep");
    allowed.eligibility.matched_recipients = vec![RecipientRef::User(
        "usr_00000000000000000000000001".parse().unwrap(),
    )];
    allowed.comment = None;
    submit(&mut trace, allowed);
    assert_eq!(trace.count("ApprovalDecided"), 1);
    assert_eq!(trace.step("gate").outputs["comment"], "");
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
}
#[test]
fn repeated_approval_instances_have_independent_quorum() {
    let mut context = runbook_spec::CompileContext::default();
    context.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: repeat, loop: {max_iterations: 2, until: '${{ iteration == 2 }}', steps: [{id: gate, approval: {prompt: Proceed?, options: [keep], approvers: {teams: [oncall]}}}]}}]}",
        &[],
        context,
    );
    for iteration in [1, 2] {
        let step = trace.step(&format!("repeat#{iteration}.gate"));
        assert!(step.approval.as_ref().unwrap().votes.is_empty());
        let input = ResolvedVote {
            at_ms: trace.state.at_ms,
            base_sequence: trace.state.sequence,
            step_run_id: step.id.clone().unwrap(),
            actor: ActorRef::Human {
                user_id: "usr_00000000000000000000000001".parse().unwrap(),
            },
            credential_ref: "pat_alice".into(),
            option: "keep".into(),
            comment: None,
            eligibility: VoteEligibility {
                eligible: true,
                directory_revision: 1,
                matched_recipients: vec![RecipientRef::Team(
                    "team_00000000000000000000000001".into(),
                )],
            },
        };
        submit(&mut trace, input);
    }
    assert_eq!(trace.count("ApprovalDecided"), 2);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn open_approval_waits_for_votes() {
    let trace = gate();
    assert_eq!(
        trace.step("gate").status,
        runbook_model::StepStatus::Waiting
    );
    assert!(trace.ready.is_empty());
}
