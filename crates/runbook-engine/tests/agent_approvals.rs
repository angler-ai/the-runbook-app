mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, RecipientRef};
use support::*;
fn gate(allow: bool, min: u32) -> Trace {
    let mut directory = runbook_spec::CompileContext::default();
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    run_document(
        &format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test}}\nspec: {{steps: [{{id: gate, approval: {{prompt: Proceed?, options: [keep, rollback], approvers: {{teams: [oncall]}}, allow_agents: {allow}, min: {min}}}}}, {{id: next, title: Next}}]}}"
        ),
        &[],
        directory,
    )
}
fn agent(t: &Trace) -> AgentDecision {
    AgentDecision {
        audit_only_comment: true,
        at_ms: t.state.at_ms,
        base_sequence: t.state.sequence,
        step_run_id: t.step("gate").id.clone().unwrap(),
        option: "keep".into(),
        evidence: vec![],
        comment: Some("checked".into()),
        principal: "agt_ops".into(),
        agent_name: "ops".into(),
        model: Some("model-a".into()),
        session_id: "session-1".into(),
    }
}
fn human(t: &Trace) -> ResolvedVote {
    ResolvedVote {
        at_ms: t.state.at_ms,
        base_sequence: t.state.sequence,
        step_run_id: t.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "pat-a".into(),
        option: "rollback".into(),
        comment: None,
        eligibility: VoteEligibility {
            eligible: true,
            directory_revision: 1,
            matched_recipients: vec![RecipientRef::Team("team_00000000000000000000000001".into())],
        },
    }
}
#[test]
fn agent_decision_preserves_provenance_without_human_quorum_and_replays() {
    let mut t = gate(true, 1);
    let d = agent(&t);
    let events = apply_agent_decision(&t.state, &d).unwrap();
    let json = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(json["type"], "DecisionApplied");
    assert_eq!(json["decision"]["principal"], "agt_ops");
    assert_eq!(json["decision"]["agent_name"], "ops");
    assert_eq!(json["decision"]["model"], "model-a");
    assert_eq!(json["decision"]["session_id"], "session-1");
    let event: Event = serde_json::from_value(json).unwrap();
    t.append(event, d.actor(), d.at_ms);
    t.settle();
    assert!(t.error_codes.is_empty(), "{:?}", t.error_codes);
    let a = t.step("gate").approval.as_ref().unwrap();
    assert_eq!(a.distinct_votes, 0);
    assert_eq!(t.step("gate").outputs["comment"], "");
    assert_eq!(
        a.agent_decision.as_ref().unwrap().comment.as_deref(),
        Some("checked")
    );
    assert!(a.votes.is_empty());
    assert!(a.decision.is_none());
    assert_eq!(a.agent_decision.as_ref(), Some(&d));
    assert_eq!(t.count("ApprovalDecided"), 0);
    assert_eq!(t.state, t.replay());
}
#[test]
fn agent_runtime_rejects_disabled_and_invalid_draft_quorum() {
    for (allow, min) in [(false, 1), (true, 2)] {
        let t = gate(allow, min);
        assert_eq!(
            apply_agent_decision(&t.state, &agent(&t))
                .unwrap_err()
                .code(),
            "approval.invalid_agent_policy"
        );
    }
}
#[test]
fn first_valid_human_or_agent_decision_wins() {
    for agent_first in [true, false] {
        let mut t = gate(true, 1);
        let d = agent(&t);
        let v = human(&t);
        if agent_first {
            for e in apply_agent_decision(&t.state, &d).unwrap() {
                t.append(e, d.actor(), d.at_ms);
            }
            let mut late = v;
            late.base_sequence = t.state.sequence;
            assert_eq!(
                apply_vote(&t.state, &late).unwrap_err().code(),
                "approval.not_open"
            );
        } else {
            for e in apply_vote(&t.state, &v).unwrap() {
                t.append(e, v.actor.clone(), v.at_ms);
            }
            // A valid human signature at min=1 already reserves the decision before draining.
            let mut late = d;
            late.base_sequence = t.state.sequence;
            assert_eq!(
                apply_agent_decision(&t.state, &late).unwrap_err().code(),
                "approval.not_open"
            );
            t.settle();
        }
        assert_eq!(t.state, t.replay());
    }
}
#[test]
fn agent_event_cannot_forge_actor_or_provenance() {
    let mut t = gate(true, 1);
    let d = agent(&t);
    let e = apply_agent_decision(&t.state, &d).unwrap().remove(0);
    t.append(e, ActorRef::System, d.at_ms);
    assert_eq!(t.error_codes.last().unwrap(), "approval.actor_mismatch");
    let mut invalid = d;
    invalid.session_id.clear();
    assert_eq!(
        apply_agent_decision(&t.state, &invalid).unwrap_err().code(),
        "approval.invalid_agent_provenance"
    );
}
