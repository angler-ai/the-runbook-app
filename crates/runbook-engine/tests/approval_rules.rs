//! Approval negative paths not covered by `approvals.rs` and
//! `agent_approvals.rs`: escalation eligibility windows, malformed vote facts,
//! cleanup-only voting, and the runtime agent/destructive guard
//! (PRD section 5, "Approval timeout and destructive-action policy").
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, ExecutionStatus, RecipientRef, StepStatus};
use support::*;

/// An expected rejection code and the edit that should cause it.
type Mutation<T> = (&'static str, fn(&mut T));

const ONCALL: &str = "team_00000000000000000000000001";
const LEADS: &str = "team_00000000000000000000000002";
const MIN: i64 = 60_000;

fn directory() -> runbook_spec::CompileContext {
    let mut directory = runbook_spec::CompileContext::default();
    for (name, id) in [("oncall", ONCALL), ("leads", LEADS)] {
        directory.teams.insert(name.into(), id.parse().unwrap());
    }
    directory
}
fn document(spec: &str, inputs: &[Input]) -> Trace {
    run_document(
        &format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test, owners: [team:leads]}}\n{spec}"
        ),
        inputs,
        directory(),
    )
}
fn vote(trace: &Trace, step: &str, user: u32, team: &str, option: &str) -> ResolvedVote {
    ResolvedVote {
        at_ms: trace.state.at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step(step).id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: format!("usr_{user:026}").parse().unwrap(),
        },
        credential_ref: format!("tok_{user}"),
        option: option.into(),
        comment: None,
        eligibility: VoteEligibility {
            eligible: true,
            directory_revision: 1,
            matched_recipients: vec![RecipientRef::Team(team.into())],
        },
    }
}
fn code(trace: &Trace, vote: &ResolvedVote) -> Result<(), &'static str> {
    apply_vote(&trace.state, vote)
        .map(|_| ())
        .map_err(|e| e.code())
}
fn tick(trace: &mut Trace, at_ms: i64) {
    loop {
        let commands = advance_time(&trace.state, at_ms).unwrap();
        if commands.is_empty() {
            break;
        }
        for command in commands {
            trace.commit_command(command, at_ms);
        }
    }
    trace.settle();
}

const ESCALATING: &str = "spec: {execution: {timeout: 4h}, steps: [{id: gate, approval: {prompt: Go?, options: [go, stop], approvers: {teams: [oncall]}, timeout: 30m, escalate_to: [team:leads], escalation_timeout: 30m}}, {id: next, title: Next}]}";

/// "Eligible escalation recipients may decide during escalation alongside
/// the original approvers": not before it, and a supplied team fact that the
/// gate never named is refused even when the runtime says eligible.
#[test]
fn escalation_recipients_may_vote_only_once_escalated() {
    let mut trace = document(ESCALATING, &[]);
    let lead = vote(&trace, "gate", 5, LEADS, "go");
    assert_eq!(code(&trace, &lead), Err("approval.not_eligible"));
    let stranger = vote(&trace, "gate", 6, "team_00000000000000000000000009", "go");
    assert_eq!(code(&trace, &stranger), Err("approval.not_eligible"));

    tick(&mut trace, 30 * MIN);
    assert_eq!(trace.count("ApprovalEscalated"), 1);
    let stranger = vote(&trace, "gate", 6, "team_00000000000000000000000009", "go");
    assert_eq!(code(&trace, &stranger), Err("approval.not_eligible"));
    // Original approvers keep their eligibility during escalation.
    assert_eq!(code(&trace, &vote(&trace, "gate", 1, ONCALL, "go")), Ok(()));
    let lead = vote(&trace, "gate", 5, LEADS, "stop");
    for event in apply_vote(&trace.state, &lead).unwrap() {
        trace.append(event, lead.actor.clone(), lead.at_ms);
    }
    trace.settle();
    assert_eq!(trace.step("gate").status, StepStatus::Succeeded);
    assert_eq!(trace.step("gate").outputs["decision"], "stop");
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state, trace.replay());
}

/// Each supplied authorization fact is necessary on its own.
#[test]
fn malformed_vote_facts_are_rejected_individually() {
    let trace = document(ESCALATING, &[]);
    let good = vote(&trace, "gate", 1, ONCALL, "go");
    assert_eq!(code(&trace, &good), Ok(()));
    let cases: [Mutation<ResolvedVote>; 7] = [
        ("approval.invalid_option", |v| v.option = "maybe".into()),
        ("approval.missing_credential", |v| v.credential_ref.clear()),
        ("approval.not_eligible", |v| v.eligibility.eligible = false),
        ("approval.not_eligible", |v| {
            v.eligibility.directory_revision = 0
        }),
        ("approval.not_eligible", |v| {
            v.eligibility.matched_recipients.clear()
        }),
        ("approval.not_eligible", |v| v.actor = ActorRef::System),
        ("event.sequence_conflict", |v| v.base_sequence -= 1),
    ];
    for (expected, edit) in cases {
        let mut bad = good.clone();
        edit(&mut bad);
        assert_eq!(code(&trace, &bad), Err(expected), "{bad:?}");
    }
}

/// Once the run is in cleanup, only cleanup gates accept decisions; after an
/// execution timeout the refusal names the deadline.
#[test]
fn ordinary_gates_are_closed_during_cleanup() {
    let spec = "spec: {execution: {timeout: 1h, cleanup_timeout: 30m}, steps: [{id: gate, approval: {prompt: Go?, options: [go], approvers: {teams: [oncall]}}}], finally: [{id: final_gate, approval: {prompt: Close?, options: [close], approvers: {teams: [oncall]}}}]}";
    let mut cancelled = document(spec, &[]);
    let gate = vote(&cancelled, "gate", 1, ONCALL, "go");
    cancelled.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        ActorRef::System,
        1,
    );
    cancelled.settle();
    let mut stale = gate.clone();
    stale.base_sequence = cancelled.state.sequence;
    stale.at_ms = 1;
    assert_eq!(code(&cancelled, &stale), Err("execution.cleanup_only"));
    // The cleanup gate itself is open and decides the run's cleanup.
    let close = vote(&cancelled, "final_gate", 1, ONCALL, "close");
    for event in apply_vote(&cancelled.state, &close).unwrap() {
        cancelled.append(event, close.actor.clone(), close.at_ms);
    }
    cancelled.settle();
    assert_eq!(cancelled.state.status, ExecutionStatus::Cancelled);

    let mut timed_out = document(spec, &[]);
    tick(&mut timed_out, 60 * MIN);
    assert_eq!(timed_out.state.terminal_reason.as_deref(), Some("timeout"));
    let late = vote(&timed_out, "gate", 1, ONCALL, "go");
    assert_eq!(code(&timed_out, &late), Err("execution.deadline_exceeded"));
    assert!(cancelled.error_codes.is_empty() && timed_out.error_codes.is_empty());
}

/// "Agent approval is forbidden on gates leading to destructive tasks, even
/// when the gate has `allow_agents: true`": a compiled draft that skipped
/// publication policy is still refused at decision time.
#[test]
fn draft_agent_gate_cannot_reach_destructive_work() {
    let draft = |destructive: bool| {
        document(
            &format!(
                "spec: {{steps: [{{id: gate, approval: {{prompt: Go?, options: [go], approvers: {{teams: [oncall]}}, allow_agents: true, min: 1}}}}, {{id: wipe, title: Wipe, destructive: {destructive}}}]}}"
            ),
            &[],
        )
    };
    let decision = |trace: &Trace| AgentDecision {
        at_ms: trace.state.at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        option: "go".into(),
        comment: None,
        principal: "agt_ops".into(),
        agent_name: "ops".into(),
        model: None,
        session_id: "s1".into(),
    };
    let blocked = draft(true);
    assert!(!runbook_spec::validate_policy(blocked.state.compiled()).is_empty());
    assert_eq!(
        apply_agent_decision(&blocked.state, &decision(&blocked))
            .unwrap_err()
            .code(),
        "approval.invalid_agent_policy"
    );
    let allowed = draft(false);
    assert!(apply_agent_decision(&allowed.state, &decision(&allowed)).is_ok());
}
