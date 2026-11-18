//! The complete PRD source, with explicit runtime input and directory facts.
use super::*;
use serde_json::{Value, json};
pub const SOURCE: &str = include_str!("../../../../conformance/spec/post-deploy.yaml");
fn human() -> ActorRef {
    ActorRef::Human {
        user_id: "usr_00000000000000000000000001".parse().unwrap(),
    }
}
fn agent() -> ActorRef {
    ActorRef::Agent {
        principal_id: "service-checker".into(),
        session_id: "session-current".into(),
        name: "fixture-agent".into(),
        model: None,
    }
}
fn report(
    trace: &mut Trace,
    name: &str,
    verdict: Option<Verdict>,
    outputs: Value,
    evidence: bool,
    actor: ActorRef,
) {
    let work = WorkReport {
        appended_logs: false,
        files: vec![],
        status: ReportStatus::Succeeded,
        verdict,
        outputs: outputs
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        summary: Some("Fixture work completed.".into()),
        evidence: if evidence {
            vec!["https://metrics.example.test/evidence".into()]
        } else {
            vec![]
        },
        logs: vec![],
    };
    trace.append(
        Event::StepReported {
            step_run_id: trace.step(name).id.clone().unwrap(),
            report: work,
        },
        actor,
        trace.state.at_ms + 1,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
}
pub fn run(recover_loop: bool) -> Trace {
    let mut directory = CompileContext::default();
    directory
        .skills
        .insert("./skills/check-error-rates".into(), [1; 32]);
    directory
        .skills
        .insert("./skills/rollback-service".into(), [2; 32]);
    directory.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    directory.teams.insert(
        "eng-leads".into(),
        "team_00000000000000000000000002".parse().unwrap(),
    );
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    directory.users.insert(
        "bob".into(),
        "usr_00000000000000000000000002".parse().unwrap(),
    );
    let start = StartContext {
        inputs: [
            ("service".into(), json!("api")),
            ("version".into(), json!("commit-a")),
            ("regions".into(), json!(["east", "west"])),
        ]
        .into(),
        url: "https://example.test/executions/exe_00000000000000000000000001".into(),
        ..Default::default()
    };
    let mut trace = run_document_with_start(SOURCE, &[], directory, start);
    trace.team_members.insert(
        "team_00000000000000000000000001".into(),
        [1, 2]
            .map(|i| format!("usr_{i:026}").parse().unwrap())
            .to_vec(),
    );
    report(
        &mut trace,
        "api_health",
        Some(if recover_loop {
            Verdict::Pass
        } else {
            Verdict::Fail
        }),
        json!({"status_code":if recover_loop {200} else {503}}),
        false,
        agent(),
    );
    let event = update_checklist(
        &trace.state,
        trace.step("login_flow").id.as_ref().unwrap(),
        &[true, true],
    )
    .unwrap();
    trace.append(event, human(), trace.state.at_ms + 1);
    report(
        &mut trace,
        "login_flow",
        Some(Verdict::Pass),
        json!({}),
        false,
        human(),
    );
    let rate_steps = trace.ready.clone();
    assert_eq!(rate_steps.len(), 2);
    for name in rate_steps {
        let rendered = render_step(
            &trace.state,
            trace.step(&name).id.as_ref().unwrap(),
            runbook_model::ActorKind::Agent,
        )
        .unwrap();
        assert!(matches!(
            rendered.skill_parameters["region"].as_str(),
            Some("east" | "west")
        ));
        report(
            &mut trace,
            &name,
            Some(if recover_loop {
                Verdict::Fail
            } else {
                Verdict::Pass
            }),
            json!({"rate": if recover_loop {0.02} else {0.001}}),
            true,
            agent(),
        );
    }
    if recover_loop {
        report(
            &mut trace,
            "stabilize#1.recheck",
            Some(Verdict::Fail),
            json!({"rate":0.02}),
            false,
            agent(),
        );
        let due = trace
            .step("stabilize")
            .loop_state
            .as_ref()
            .unwrap()
            .timer
            .as_ref()
            .unwrap()
            .due_at_ms;
        loop {
            let commands = advance_time(&trace.state, due).unwrap();
            if commands.is_empty() {
                break;
            }
            for command in commands {
                trace.commit_command(command, due);
            }
        }
        trace.settle();
        report(
            &mut trace,
            "stabilize#2.recheck",
            Some(Verdict::Pass),
            json!({"rate":0.001}),
            false,
            agent(),
        );
    }
    assert_eq!(trace.step("signoff").status, StepStatus::Waiting);
    assert_eq!(
        trace
            .step("signoff")
            .approval
            .as_ref()
            .unwrap()
            .requested
            .as_ref()
            .unwrap()
            .recipients
            .len(),
        2
    );
    let vote = ResolvedVote {
        at_ms: trace.state.at_ms + 1,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("signoff").id.clone().unwrap(),
        actor: human(),
        credential_ref: "tok_fixture_public_id".into(),
        option: if recover_loop { "keep" } else { "rollback" }.into(),
        comment: Some("Reviewed the evidence.".into()),
        eligibility: VoteEligibility {
            eligible: true,
            directory_revision: 1,
            matched_recipients: vec![runbook_model::RecipientRef::Team(
                "team_00000000000000000000000001".into(),
            )],
        },
    };
    for event in apply_vote(&trace.state, &vote).unwrap() {
        trace.append(event, vote.actor.clone(), vote.at_ms);
    }
    trace.settle();
    if !recover_loop {
        report(&mut trace, "rollback", None, json!({}), false, agent());
    }
    assert_eq!(trace.ready, ["notify"]);
    report(&mut trace, "notify", None, json!({}), false, agent());
    assert_eq!(trace.state.fields["severity"], "major");
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    trace
}
pub fn serialized(trace: &Trace) -> Value {
    json!({"events":trace.events,"commands":trace.commands,"status":trace.state.status,"result":trace.state.result,"fields":trace.state.fields,"loop":{"verdict":trace.step("stabilize").verdict,"satisfied":trace.step("stabilize").satisfied,"exhausted":trace.step("stabilize").exhausted}})
}
