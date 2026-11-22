//! R0-4: a runner step's target is recorded when it becomes ready, once,
//! and replays the same however the project's directory or default agent
//! changed after publish; releases, waiting reasons and the "released but
//! never reported" flag are facts of the record.
mod support;
use runbook_engine::{AgentTargetRef, Event, ExternalTarget};
use runbook_model::{
    ActorRef, AgentCeiling, AgentDirectoryEntry, AgentStatus, ReportStatus, StepStatus,
};
use runbook_spec::CompileContext;
use support::run_document;

fn entry(name: &str, id: &str) -> AgentDirectoryEntry {
    AgentDirectoryEntry {
        agent_id: id.into(),
        name: name.into(),
        project_id: "prj_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        group_id: "rg_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        status: AgentStatus::Active,
        version_id: format!("{id}_v1"),
        ceiling: AgentCeiling {
            profiles: vec!["fast".into()],
            profile_models: Default::default(),
            max_cost_usd: 1.0,
            max_tokens: 1000,
            timeout_s: 60,
            credentials: vec![],
            tools: vec![],
            hosts: vec![],
        },
    }
}
fn context(default_agent: Option<&str>) -> CompileContext {
    CompileContext {
        agents: Some(
            [entry("checker", "agt_1"), entry("other", "agt_2")]
                .into_iter()
                .map(|e| (e.name.clone(), e))
                .collect(),
        ),
        default_agent: default_agent.map(str::to_owned),
        ..Default::default()
    }
}
const SOURCE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: test, title: Test}\n\
    spec: {steps: [{id: check, title: Check, actor: agent}, \
    {id: open, title: Open, actor: agent, agent: {name: external}}, \
    {id: human, title: Human, actor: human}]}\n";

fn runner() -> ActorRef {
    ActorRef::Agent {
        principal_id: "runner:rn_1".into(),
        session_id: "s1".into(),
        name: "checker".into(),
        model: None,
    }
}

#[test]
fn target_resolved_once_and_replayed() {
    let mut trace = run_document(SOURCE, &[], context(Some("checker")));
    assert_eq!(
        trace.step("check").runner.target,
        Some(AgentTargetRef::Runner {
            agent_id: "agt_1".into()
        })
    );
    assert_eq!(trace.count("AgentTargetResolved"), 1);
    assert_eq!(trace.state, trace.replay());
    // The stored event names the agent, or "external", as the contract says.
    let stored: Vec<serde_json::Value> = trace
        .events
        .iter()
        .filter(|e| e.event.name() == "AgentTargetResolved")
        .map(|e| serde_json::to_value(&e.event).unwrap())
        .collect();
    assert_eq!(
        stored[0]["target"],
        serde_json::json!({"agent_id": "agt_1"})
    );
    assert_eq!(stored[0]["step_id"], "check");
    // Settling again records nothing new.
    let before = trace.events.len();
    trace.settle();
    assert_eq!(trace.events.len(), before);
    // The project default changing later does not move this run: the
    // version pins the resolution, and replay reads only the log.
    let republished = run_document(SOURCE, &[], context(Some("other")));
    assert_eq!(
        republished.step("check").runner.target,
        Some(AgentTargetRef::Runner {
            agent_id: "agt_2".into()
        })
    );
    assert_eq!(
        trace.replay().steps[0].runner.target,
        Some(AgentTargetRef::Runner {
            agent_id: "agt_1".into()
        })
    );
    // The external step's target becomes ready, recorded as "external";
    // the person's step has none.
    let id = trace.step("check").id.clone().unwrap();
    trace.append(
        Event::StepClaimed {
            step_run_id: id.clone(),
            agent_version_id: Some("agt_1_v1".into()),
            claim_id: None,
        },
        runner(),
        trace.state.at_ms,
    );
    trace.append(
        Event::StepReported {
            step_run_id: id,
            report: report(),
        },
        runner(),
        trace.state.at_ms,
    );
    trace.settle();
    assert_eq!(
        trace.step("open").runner.target,
        Some(AgentTargetRef::External(ExternalTarget::External))
    );
    let open = trace
        .events
        .iter()
        .rev()
        .find(|e| e.event.name() == "AgentTargetResolved")
        .unwrap();
    assert_eq!(
        serde_json::to_value(&open.event).unwrap()["target"],
        "external"
    );
    assert_eq!(
        trace.step("check").runner.agent_version_id.as_deref(),
        Some("agt_1_v1")
    );
    assert_eq!(trace.state, trace.replay());
}

const GATED: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gated, title: Gated}\n\
    spec: {steps: [{id: gate, approval: {prompt: Deploy?, options: [go, stop], \
    approvers: {teams: [oncall]}}}, \
    {id: deploy, title: Deploy, actor: agent, destructive: true}]}\n";

fn gate_vote(trace: &mut support::Trace, option: &str) {
    let vote = runbook_engine::ResolvedVote {
        at_ms: trace.state.at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "pat_1".into(),
        option: option.into(),
        comment: None,
        eligibility: runbook_engine::VoteEligibility {
            directory_revision: 1,
            eligible: true,
            matched_recipients: vec![runbook_model::RecipientRef::Team(
                "team_00000000000000000000000001".into(),
            )],
        },
    };
    for event in runbook_engine::apply_vote(&trace.state, &vote).unwrap() {
        trace.append(event, vote.actor.clone(), trace.state.at_ms);
    }
    trace.settle();
}

/// Review L4 (§6.5 rule 2): a destructive runner step counts as approved
/// only once a person decided an approval that dominates it in this run;
/// any option counts (the runbook's own `if` on the decision handles a
/// rejection).
#[test]
fn destructive_step_needs_a_person_decided_approval() {
    let mut context = context(Some("checker"));
    context.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(GATED, &[], context);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert!(!runbook_engine::person_approved(
        &trace.state,
        trace.step("deploy")
    ));
    gate_vote(&mut trace, "stop");
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("deploy").status, StepStatus::Ready);
    assert!(runbook_engine::person_approved(
        &trace.state,
        trace.step("deploy")
    ));
    // A step no approval dominates is never person-approved.
    let other = run_document(SOURCE, &[], self::context(Some("checker")));
    assert!(!runbook_engine::person_approved(
        &other.state,
        other.step("check")
    ));
}

#[test]
fn destructive_step_requires_an_option_listed_in_approves() {
    let source = GATED.replace("options: [go, stop]", "options: [go, stop], approves: [go]");
    for (option, expected) in [("stop", false), ("go", true)] {
        let mut context = context(Some("checker"));
        context.teams.insert(
            "oncall".into(),
            "team_00000000000000000000000001".parse().unwrap(),
        );
        let mut trace = run_document(&source, &[], context);
        gate_vote(&mut trace, option);
        assert_eq!(
            runbook_engine::person_approved(&trace.state, trace.step("deploy")),
            expected
        );
    }
}

fn report() -> runbook_engine::WorkReport {
    runbook_engine::WorkReport {
        appended_logs: false,
        files: vec![],
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Default::default(),
        summary: None,
        evidence: vec![],
        logs: vec![],
    }
}

#[test]
fn waiting_releases_and_never_reported_flag_replay() {
    let mut trace = run_document(SOURCE, &[], context(Some("checker")));
    let id = trace.step("check").id.clone().unwrap();
    trace.append(
        Event::StepWaiting {
            step_run_id: id.clone(),
            reason: "agent_paused".into(),
            detail: None,
        },
        ActorRef::System,
        trace.state.at_ms,
    );
    assert_eq!(
        trace.step("check").runner.waiting_reason.as_deref(),
        Some("agent_paused")
    );
    trace.append(
        Event::StepClaimed {
            step_run_id: id.clone(),
            agent_version_id: Some("agt_1_v1".into()),
            claim_id: None,
        },
        runner(),
        trace.state.at_ms,
    );
    assert_eq!(trace.step("check").runner.waiting_reason, None);
    trace.append(
        Event::CredentialReleased {
            step_run_id: id.clone(),
            credential_id: "cred_dd".into(),
            credential_version: 2,
            delivery: "proxy".into(),
            runner_id: "rn_1".into(),
            claim_id: None,
        },
        ActorRef::System,
        trace.state.at_ms,
    );
    assert_eq!(trace.step("check").runner.releases.len(), 1);
    // The lease lapses with no report: flagged.
    trace.append(
        Event::StepClaimExpired {
            step_run_id: id.clone(),
        },
        ActorRef::System,
        trace.state.at_ms,
    );
    assert!(trace.step("check").runner.released_never_reported);
    assert_eq!(trace.step("check").status, StepStatus::Ready);
    trace.append(
        Event::CredentialRevoked {
            step_run_id: id,
            credential_id: "cred_dd".into(),
            reason: "lease_ended".into(),
        },
        ActorRef::System,
        trace.state.at_ms,
    );
    assert_eq!(
        trace.step("check").runner.releases[0].revoked.as_deref(),
        Some("lease_ended")
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state, trace.replay());
}
