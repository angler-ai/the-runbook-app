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
    vote_at(trace, "gate", user, token, option)
}
fn reviewer(user: u32) -> ActorRef {
    ActorRef::Human {
        user_id: format!("usr_{user:026}").parse().unwrap(),
    }
}
fn vote_for(trace: &Trace, path: &str, user: u32) -> ResolvedVote {
    vote_at(trace, path, user, "pat_review", "keep")
}
fn vote_at(trace: &Trace, path: &str, user: u32, token: &str, option: &str) -> ResolvedVote {
    ResolvedVote {
        at_ms: trace.state.at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step(path).id.clone().unwrap(),
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
        service_attribution: None,
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

#[test]
fn a_task_completer_cannot_sign_even_as_a_team_approver() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(
        "metadata: {id: test}\nspec: {steps: [{id: work, actor: human}, {id: gate, approval: {prompt: Proceed?, options: [keep], approvers: {teams: [oncall]}, exclude_completers_of: [work]}}]}",
        &[],
        directory,
    );
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("work").id.clone().unwrap(),
            report: WorkReport {
                appended_logs: false,
                status: runbook_model::ReportStatus::Succeeded,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
                files: vec![],
            },
        },
        ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        0,
    );
    trace.settle();
    let completer = vote(&trace, 1, "pat_first", "keep");
    submit(&mut trace, completer);
    assert_eq!(trace.error_codes, ["approval.excluded_completer"]);
    assert_eq!(trace.count("ApprovalVoted"), 0);
    let independent = vote(&trace, 2, "pat_second", "keep");
    submit(&mut trace, independent);
    assert_eq!(trace.count("ApprovalDecided"), 1);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_review_after_a_loop_excludes_every_iteration_contributor() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let second = Input::Report {
        step: "collect#2.work".into(),
        status: runbook_model::ReportStatus::Succeeded,
        verdict: None,
        outputs: Default::default(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000002".parse().unwrap(),
        },
        at_ms: None,
    };
    let source = r#"
metadata: {id: independent}
spec:
  steps:
    - id: collect
      loop:
        until: "${{ iteration == 2 }}"
        max_iterations: 2
        steps:
          - id: work
            actor: human
    - id: gate
      approval:
        prompt: Review?
        options: [keep]
        approvers: {teams: [oncall]}
        exclude_completers_of: [work]
"#;
    let trace = run_document(source, &[succeeded("collect#1.work"), second], directory);
    let latest = vote(&trace, 2, "pat_bob", "keep");
    assert_eq!(
        apply_vote(&trace.state, &latest).unwrap_err().code(),
        "approval.excluded_completer"
    );
    let earlier = vote(&trace, 1, "pat_alice", "keep");
    assert_eq!(
        apply_vote(&trace.state, &earlier).unwrap_err().code(),
        "approval.excluded_completer"
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_sponsored_report_excludes_the_sponsor_on_replay() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let source = r#"
metadata: {id: independent}
spec:
  steps:
    - id: work
      actor: any
    - id: gate
      approval:
        prompt: Review?
        options: [keep]
        approvers: {teams: [oncall]}
        exclude_completers_of: [work]
"#;
    let trace = run_document(
        source,
        &[Input::Report {
            step: "work".into(),
            status: runbook_model::ReportStatus::Succeeded,
            verdict: None,
            outputs: Default::default(),
            at_ms: None,
            actor: ActorRef::Agent {
                principal_id: "agent:usr_00000000000000000000000001".into(),
                session_id: "agent-session".into(),
                name: "test".into(),
                model: None,
                service_attribution: None,
            },
        }],
        directory,
    );
    let sponsor = vote(&trace, 1, "pat_sponsor", "keep");
    assert_eq!(
        apply_vote(&trace.replay(), &sponsor).unwrap_err().code(),
        "approval.excluded_completer"
    );
}

#[test]
fn an_amending_admin_and_released_claim_owner_are_excluded() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let source = r#"
metadata: {id: independent}
spec:
  steps:
    - id: work
      actor: human
    - id: gate
      approval:
        prompt: Review?
        options: [keep]
        approvers: {teams: [oncall]}
        exclude_completers_of: [work]
"#;
    let mut trace = run_document(source, &[], directory);
    let human = |n| ActorRef::Human {
        user_id: format!("usr_{n:026}").parse().unwrap(),
    };
    let id = trace.step("work").id.clone().unwrap();
    trace.append(
        Event::StepClaimed {
            step_run_id: id.clone(),
            agent_version_id: None,
            claim_id: None,
        },
        human(2),
        0,
    );
    trace.append(
        Event::StepClaimReleased {
            step_run_id: id.clone(),
            reason: None,
        },
        human(2),
        0,
    );
    trace.append(
        Event::StepReported {
            step_run_id: id.clone(),
            report: WorkReport {
                status: runbook_model::ReportStatus::Succeeded,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
                files: vec![],
                appended_logs: false,
            },
        },
        human(1),
        0,
    );
    trace.settle();
    let original = runbook_model::StepRecord {
        status: runbook_model::StepStatus::Succeeded,
        verdict: None,
        summary: None,
        outputs: Default::default(),
    };
    let amendment = validate_override(
        &trace.state,
        &id,
        &runbook_model::StepRecord {
            summary: Some("Amended".into()),
            ..original
        },
        "Clarify result",
        human(3),
    )
    .unwrap();
    trace.append(
        Event::StepOverridden {
            step_run_id: id,
            amendment: Box::new(amendment),
        },
        human(3),
        0,
    );
    for user in [2, 3] {
        assert_eq!(
            apply_vote(&trace.state, &vote(&trace, user, "pat_review", "keep"))
                .unwrap_err()
                .code(),
            "approval.excluded_completer"
        );
    }
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn concurrent_foreach_reviews_exclude_only_their_own_contributors() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let source = r#"
metadata: {id: independent}
spec:
  inputs: {items: {type: list, items: {type: string}, default: [east, west]}}
  steps:
    - id: each
      foreach:
        items: "${{ inputs.items }}"
        key: "${{ item }}"
        max_items: 2
        concurrency: 2
        steps:
          - {id: work, actor: human}
          - id: gate
            approval: {prompt: Review?, options: [keep], approvers: {teams: [oncall]}, exclude_completers_of: [work]}
"#;
    let trace = run_document(
        source,
        &[
            succeeded("each[east].work"),
            Input::Report {
                step: "each[west].work".into(),
                status: runbook_model::ReportStatus::Succeeded,
                actor: ActorRef::Human {
                    user_id: "usr_00000000000000000000000002".parse().unwrap(),
                },
                verdict: None,
                outputs: Default::default(),
                at_ms: None,
            },
        ],
        directory,
    );
    for (path, own, peer) in [("each[east].gate", 1, 2), ("each[west].gate", 2, 1)] {
        let mut own_vote = vote_for(&trace, path, own);
        assert_eq!(
            apply_vote(&trace.state, &own_vote).unwrap_err().code(),
            "approval.excluded_completer"
        );
        own_vote.actor = reviewer(peer);
        assert!(apply_vote(&trace.state, &own_vote).is_ok());
    }
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn timed_out_foreach_work_blocks_only_its_iteration_until_amended() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(
        r#"
metadata: {id: independent}
spec:
  inputs: {items: {type: list, items: {type: string}, default: [east, west]}}
  steps:
    - id: each
      foreach:
        items: "${{ inputs.items }}"
        key: "${{ item }}"
        max_items: 2
        concurrency: 2
        steps:
          - {id: work, actor: human, timeout: 1m, on_failure: continue, outputs: {result: {type: string}}}
          - id: gate
            approval: {prompt: Review?, options: [keep], approvers: {teams: [oncall]}, exclude_completers_of: [work]}
"#,
        &[succeeded("each[east].work"), Input::Tick { at_ms: 60_000 }],
        directory,
    );
    assert!(apply_vote(&trace.state, &vote_for(&trace, "each[east].gate", 2)).is_ok());
    assert_eq!(
        apply_vote(&trace.state, &vote_for(&trace, "each[west].gate", 2))
            .unwrap_err()
            .code(),
        "approval.review_missing_report"
    );
    let step = trace.step("each[west].work");
    let id = step.id.clone().unwrap();
    let amendment = validate_override(
        &trace.state,
        &id,
        &runbook_model::StepRecord {
            status: step.status,
            verdict: step.verdict,
            summary: None,
            outputs: [("result".into(), serde_json::json!("verified"))].into(),
        },
        "Verified the work independently",
        reviewer(3),
    )
    .unwrap();
    trace.append(
        Event::StepOverridden {
            step_run_id: id,
            amendment: Box::new(amendment),
        },
        reviewer(3),
        60_000,
    );
    assert!(apply_vote(&trace.state, &vote_for(&trace, "each[west].gate", 2)).is_ok());
    assert_eq!(
        apply_vote(&trace.state, &vote_for(&trace, "each[west].gate", 3))
            .unwrap_err()
            .code(),
        "approval.excluded_completer"
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_gate_after_foreach_excludes_contributors_from_every_iteration() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        r#"
metadata: {id: independent}
spec:
  inputs: {items: {type: list, items: {type: string}, default: [east, west]}}
  steps:
    - id: each
      foreach:
        items: "${{ inputs.items }}"
        key: "${{ item }}"
        max_items: 2
        concurrency: 2
        steps:
          - {id: work, actor: human}
    - id: gate
      approval: {prompt: Review?, options: [keep], approvers: {teams: [oncall]}, exclude_completers_of: [work]}
"#,
        &[
            succeeded("each[east].work"),
            Input::Report {
                step: "each[west].work".into(),
                status: runbook_model::ReportStatus::Succeeded,
                actor: reviewer(2),
                verdict: None,
                outputs: Default::default(),
                at_ms: None,
            },
        ],
        directory,
    );
    assert_eq!(
        excluded_completers(&trace.state, trace.step("gate")).len(),
        2
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn an_agent_cannot_decide_after_its_sponsor_contributed() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "metadata: {id: independent}\nspec: {steps: [{id: work, actor: human}, {id: gate, approval: {prompt: Review?, options: [keep], approvers: {teams: [oncall]}, allow_agents: true, exclude_completers_of: [work]}}]}",
        &[succeeded("work")],
        directory,
    );
    let decision = AgentDecision {
        audit_only_comment: true,
        base_sequence: trace.state.sequence,
        at_ms: trace.state.at_ms,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        option: "keep".into(),
        comment: None,
        evidence: vec![],
        principal: "agent:usr_00000000000000000000000001".into(),
        agent_name: "reviewer".into(),
        model: None,
        session_id: "review".into(),
    };
    assert_eq!(
        apply_agent_decision(&trace.state, &decision)
            .unwrap_err()
            .code(),
        "approval.excluded_completer"
    );
}

#[test]
fn independent_review_waits_for_a_report_after_a_continued_task_timeout() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "metadata: {id: independent}\nspec: {steps: [{id: work, actor: human, timeout: 1m, on_failure: continue}, {id: gate, approval: {prompt: Review?, options: [keep], approvers: {teams: [oncall]}, exclude_completers_of: [work]}}]}",
        &[Input::Tick { at_ms: 60_000 }],
        directory,
    );
    assert_eq!(
        apply_vote(&trace.state, &vote(&trace, 1, "pat_review", "keep"))
            .unwrap_err()
            .code(),
        "approval.review_missing_report"
    );
}

#[test]
fn a_managed_agent_cannot_review_its_work_from_another_runner() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(
        "metadata: {id: independent}\nspec: {steps: [{id: work, actor: any}, {id: gate, approval: {prompt: Review?, options: [keep], approvers: {teams: [oncall]}, allow_agents: true, exclude_completers_of: [work]}}]}",
        &[],
        directory,
    );
    let target = AgentTargetRef::Runner {
        agent_id: "managed-agent".into(),
    };
    let work = trace.step("work").id.clone().unwrap();
    trace.append(
        Event::AgentTargetResolved {
            step_id: "work".into(),
            step_run_id: work.clone(),
            target: target.clone(),
        },
        ActorRef::System,
        0,
    );
    trace.append(
        Event::StepReported {
            step_run_id: work,
            report: WorkReport {
                status: runbook_model::ReportStatus::Succeeded,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
                files: vec![],
                appended_logs: false,
            },
        },
        ActorRef::Agent {
            principal_id: "runner:first".into(),
            session_id: "first-claim".into(),
            name: "worker".into(),
            model: None,
            service_attribution: None,
        },
        0,
    );
    trace.settle();
    let gate = trace.step("gate").id.clone().unwrap();
    trace.append(
        Event::AgentTargetResolved {
            step_id: "gate".into(),
            step_run_id: gate.clone(),
            target,
        },
        ActorRef::System,
        0,
    );
    let decision = AgentDecision {
        audit_only_comment: true,
        base_sequence: trace.state.sequence,
        at_ms: trace.state.at_ms,
        step_run_id: gate,
        option: "keep".into(),
        comment: None,
        evidence: vec![],
        principal: "runner:second".into(),
        agent_name: "worker".into(),
        model: None,
        session_id: "review".into(),
    };
    assert_eq!(
        apply_agent_decision(&trace.replay(), &decision)
            .unwrap_err()
            .code(),
        "approval.excluded_completer"
    );
}

#[test]
fn unknown_service_contributor_requires_an_explicitly_named_approver() {
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "reviewer".into(),
        "usr_00000000000000000000000002".parse().unwrap(),
    );
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "metadata: {id: independent}\nspec: {steps: [{id: work, actor: any}, {id: gate, approval: {prompt: Review?, options: [keep], approvers: {teams: [oncall], users: [reviewer]}, exclude_completers_of: [work]}}]}",
        &[Input::Report {
            step: "work".into(),
            status: runbook_model::ReportStatus::Succeeded,
            actor: ActorRef::Agent {
                principal_id: "service:legacy".into(),
                session_id: "service".into(),
                name: "worker".into(),
                model: None,
                service_attribution: None,
            },
            verdict: None,
            outputs: Default::default(),
            at_ms: None,
        }],
        directory,
    );
    assert_eq!(
        apply_vote(&trace.state, &vote_for(&trace, "gate", 1))
            .unwrap_err()
            .code(),
        "approval.unknown_service_contributor"
    );
    let mut named = vote_for(&trace, "gate", 2);
    named.eligibility.matched_recipients = vec![RecipientRef::User(
        "usr_00000000000000000000000002".parse().unwrap(),
    )];
    assert!(apply_vote(&trace.state, &named).is_ok());
}
