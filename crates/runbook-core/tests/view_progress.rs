//! UI-2: the run view's top-level progress and a waiting step's target,
//! state facts both editions take from `apply_amendment_facts`.
use runbook_core::execution_document::{progress_of, wait_of};
use runbook_engine::{ExecutionState, WaitState, WaitTarget};
use runbook_model::{ExecutionProgressView, StepStatus, StepWaitView};
use std::rc::Rc;

const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: waits, title: Waits}\n\
    spec:\n  steps:\n    \
    - {id: first, title: First}\n    \
    - id: checks\n      parallel:\n        \
    - {id: a, title: A}\n        \
    - {id: b, title: B}\n    \
    - id: go\n      wait: {signal: go, timeout: 1h}\n    \
    - id: pause\n      wait: {for: 5m}\n  \
    finally:\n    - {id: notes, title: Notes}\n";

fn state() -> ExecutionState {
    let spec = runbook_spec::parse(SPEC).unwrap();
    let compiled = runbook_spec::compile(&spec, &Default::default()).unwrap();
    ExecutionState::new(
        "exe_00000000000000000000000001".parse().unwrap(),
        Rc::new(compiled),
    )
}

fn set(state: &mut ExecutionState, id: &str, status: StepStatus) {
    let step = state.steps.iter_mut().find(|s| s.step_id == id).unwrap();
    step.status = status;
}

#[test]
fn progress_counts_finished_top_level_steps_only() {
    let mut state = state();
    assert_eq!(
        progress_of(&state),
        ExecutionProgressView { done: 0, total: 4 }
    );
    set(&mut state, "first", StepStatus::Succeeded);
    // A child finishing does not finish its container.
    set(&mut state, "a", StepStatus::Succeeded);
    set(&mut state, "b", StepStatus::Failed);
    // `finally` steps are not part of the count.
    set(&mut state, "notes", StepStatus::Succeeded);
    assert_eq!(
        progress_of(&state),
        ExecutionProgressView { done: 1, total: 4 }
    );
    set(&mut state, "checks", StepStatus::Failed);
    set(&mut state, "go", StepStatus::Skipped);
    assert_eq!(
        progress_of(&state),
        ExecutionProgressView { done: 3, total: 4 }
    );
}

#[test]
fn a_waiting_step_names_its_signal_or_timer() {
    let mut state = state();
    let go = state.steps.iter_mut().find(|s| s.step_id == "go").unwrap();
    go.wait = Some(WaitState {
        target: WaitTarget::Signal("go".into()),
        timeout_at_ms: Some(9_000),
        generation: 1,
    });
    // Not yet waiting: nothing to show.
    assert_eq!(wait_of(go), None);
    go.status = StepStatus::Waiting;
    assert_eq!(
        wait_of(go),
        Some(StepWaitView {
            signal: Some("go".into()),
            until_ms: Some(9_000)
        })
    );
    let pause = state
        .steps
        .iter_mut()
        .find(|s| s.step_id == "pause")
        .unwrap();
    pause.status = StepStatus::Waiting;
    pause.wait = Some(WaitState {
        target: WaitTarget::Time(5_000),
        timeout_at_ms: None,
        generation: 1,
    });
    assert_eq!(
        wait_of(pause),
        Some(StepWaitView {
            signal: None,
            until_ms: Some(5_000)
        })
    );
    // A finished wait shows nothing.
    pause.status = StepStatus::Succeeded;
    assert_eq!(wait_of(pause), None);
}

#[test]
fn views_serialize_without_absent_parts() {
    let view = StepWaitView {
        signal: Some("go".into()),
        until_ms: None,
    };
    assert_eq!(
        serde_json::to_value(&view).unwrap(),
        serde_json::json!({"signal": "go"})
    );
}

const TASKS: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: tasks, title: Tasks}\n\
    spec:\n  defaults: {actor: agent}\n  steps:\n    \
    - {id: bot, title: Bot}\n    \
    - {id: person, title: Person, actor: human, assignee: team:oncall}\n    \
    - {id: anyone, title: Anyone, actor: any}\n    \
    - id: gate\n      approval: {prompt: Go?, options: [go], approvers: {teams: [oncall]}}\n";

#[test]
fn a_task_names_its_actor_and_assignee() {
    use runbook_core::execution_document::{task_actor_kind, task_assignees};
    use runbook_model::{RecipientRef, StepActorKind};
    let spec = runbook_spec::parse(TASKS).unwrap();
    let mut context = runbook_spec::CompileContext::default();
    let team: runbook_model::TeamId = "team_00000000000000000000000001".parse().unwrap();
    context.teams.insert("oncall".into(), team.clone());
    let compiled = runbook_spec::compile(&spec, &context).unwrap();
    let state = ExecutionState::new(
        "exe_00000000000000000000000001".parse().unwrap(),
        Rc::new(compiled),
    );
    let of = |id: &str| state.steps.iter().find(|s| s.step_id == id).unwrap();
    assert_eq!(
        task_actor_kind(&state, of("bot")),
        Some(StepActorKind::Agent)
    );
    assert_eq!(
        task_actor_kind(&state, of("person")),
        Some(StepActorKind::Human)
    );
    assert_eq!(
        task_actor_kind(&state, of("anyone")),
        Some(StepActorKind::Any)
    );
    assert_eq!(task_actor_kind(&state, of("gate")), None);
    assert_eq!(
        task_assignees(&state, of("person")),
        vec![RecipientRef::Team(team.to_string())]
    );
    assert!(task_assignees(&state, of("bot")).is_empty());
    assert!(task_assignees(&state, of("gate")).is_empty());
}
