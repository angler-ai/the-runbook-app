//! LANG9: once a task escalates, its escalation recipients join its inbox
//! entry alongside the assignee; before that the entry is unchanged.
#[path = "../../runbook-engine/tests/support/mod.rs"]
mod support;
use runbook_core::{catalog::AssignmentKind, runtime::assignments};
use runbook_model::RecipientRef;

const ALICE: &str = "usr_00000000000000000000000001";
const CAROL: &str = "usr_00000000000000000000000003";

fn user(id: &str) -> RecipientRef {
    RecipientRef::User(id.parse().unwrap())
}

#[test]
fn an_escalated_task_routes_to_its_escalation_too() {
    let mut directory = runbook_spec::CompileContext::default();
    directory
        .users
        .insert("alice".into(), ALICE.parse().unwrap());
    directory
        .users
        .insert("carol".into(), CAROL.parse().unwrap());
    let mut trace = support::run_document(
        "metadata: {id: f}\nspec: {steps: [{id: work, actor: human, assignee: user:alice, escalate_after: 4h, escalate_to: [user:carol]}]}",
        &[support::Input::Tick { at_ms: 1_000 }],
        directory,
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let inbox = |state: &runbook_engine::ExecutionState| {
        assignments(state, &Default::default(), state.sequence, &[])
            .into_iter()
            .find(|a| a.kind == AssignmentKind::Work && a.path == "work")
            .unwrap()
            .recipients
    };
    assert_eq!(inbox(&trace.state), [user(ALICE)]);
    for command in runbook_engine::advance_time(&trace.state, 14_400_000).unwrap() {
        trace.commit_command(command, 14_400_000);
    }
    assert_eq!(trace.count("StepEscalated"), 1);
    assert_eq!(inbox(&trace.state), [user(ALICE), user(CAROL)]);
}
