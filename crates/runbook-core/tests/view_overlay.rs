//! UI-3: the facts the run page's graph overlay reads from the step view:
//! the branch a switch took, how many iterations a foreach or loop has
//! started (and a foreach's item count), and why a step was skipped. State
//! facts both editions take from `apply_amendment_facts`.
use runbook_core::execution_document::{branch_of, iterations_of, skip_reason_of};
use runbook_engine::{BranchChoice, ExecutionState, ForeachSnapshot, LoopState, SkipReason};
use runbook_model::{StepBranchView, StepIterationsView, StepSkipReason, StepStatus};
use std::rc::Rc;

const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: overlay, title: Overlay}\n\
    spec:\n  inputs:\n    regions: {type: list, items: {type: string}, default: [a, b, c]}\n  steps:\n    \
    - id: gate\n      switch:\n        cases:\n          \
    - when: \"${{ true }}\"\n            steps:\n              - {id: one, title: One}\n        \
    default:\n          steps:\n            - {id: other, title: Other}\n    \
    - id: rollout\n      foreach:\n        items: \"${{ inputs.regions }}\"\n        as: region\n        max_items: 6\n        steps:\n          \
    - {id: deploy, title: Deploy}\n    \
    - id: settle\n      loop:\n        until: \"${{ true }}\"\n        max_iterations: 4\n        steps:\n          \
    - {id: check, title: Check}\n    \
    - {id: maybe, title: Maybe, if: \"${{ false }}\"}\n";

fn state() -> ExecutionState {
    let spec = runbook_spec::parse(SPEC).unwrap();
    let compiled = runbook_spec::compile(&spec, &Default::default()).unwrap();
    ExecutionState::new(
        "exe_00000000000000000000000001".parse().unwrap(),
        Rc::new(compiled),
    )
}

fn step<'a>(state: &'a mut ExecutionState, id: &str) -> &'a mut runbook_engine::StepState {
    state.steps.iter_mut().find(|s| s.step_id == id).unwrap()
}

#[test]
fn a_switch_names_the_branch_it_took() {
    let mut state = state();
    assert_eq!(branch_of(step(&mut state, "gate")), None);
    step(&mut state, "gate").branch = Some(BranchChoice::Case(0));
    assert_eq!(
        branch_of(step(&mut state, "gate")),
        Some(StepBranchView::Case { index: 0 })
    );
    step(&mut state, "gate").branch = Some(BranchChoice::Default);
    assert_eq!(
        branch_of(step(&mut state, "gate")),
        Some(StepBranchView::Default)
    );
    // Other steps never carry one.
    assert_eq!(branch_of(step(&mut state, "maybe")), None);
}

#[test]
fn a_foreach_counts_its_items_and_a_loop_its_iterations() {
    let mut state = state();
    // Not started: nothing to count yet.
    assert_eq!(iterations_of(step(&mut state, "rollout")), None);
    step(&mut state, "rollout").foreach = Some(ForeachSnapshot {
        base_sequence: 1,
        items: serde_json::json!(["a", "b", "c"]),
        keys: vec!["a".into(), "b".into(), "c".into()],
        reads: vec![],
        admitted: 2,
    });
    assert_eq!(
        iterations_of(step(&mut state, "rollout")),
        Some(StepIterationsView {
            started: 2,
            total: Some(3),
            labels: vec!["a".into(), "b".into(), "c".into()],
        })
    );
    step(&mut state, "settle").loop_state = Some(LoopState {
        admitted: 2,
        checked: 1,
        timer: None,
    });
    assert_eq!(
        iterations_of(step(&mut state, "settle")),
        Some(StepIterationsView {
            started: 2,
            total: None,
            labels: vec![],
        })
    );
    assert_eq!(iterations_of(step(&mut state, "maybe")), None);
}

#[test]
fn a_foreach_names_scalar_items_only() {
    let mut state = state();
    let long = "x".repeat(100);
    step(&mut state, "rollout").foreach = Some(ForeachSnapshot {
        base_sequence: 1,
        items: serde_json::json!(["us-east", 2, true, long]),
        keys: vec!["0".into(), "1".into(), "2".into(), "3".into()],
        reads: vec![],
        admitted: 1,
    });
    let labels = iterations_of(step(&mut state, "rollout")).unwrap().labels;
    assert_eq!(labels[..3], ["us-east", "2", "true"]);
    assert_eq!(labels[3].chars().count(), 80);
    assert!(labels[3].ends_with('…'));
    // An object item: no labels at all (a view falls back to positions).
    step(&mut state, "rollout").foreach.as_mut().unwrap().items =
        serde_json::json!(["a", {"region": "b"}]);
    assert!(
        iterations_of(step(&mut state, "rollout"))
            .unwrap()
            .labels
            .is_empty()
    );
}

#[test]
fn a_step_skipped_by_its_condition_says_so() {
    let mut state = state();
    assert_eq!(skip_reason_of(step(&mut state, "maybe")), None);
    let maybe = step(&mut state, "maybe");
    maybe.status = StepStatus::Skipped;
    maybe.skip_reason = Some(SkipReason::Condition);
    assert_eq!(
        skip_reason_of(step(&mut state, "maybe")),
        Some(StepSkipReason::Condition)
    );
}

#[test]
fn the_overlay_views_serialize_compactly() {
    assert_eq!(
        serde_json::to_value(StepBranchView::Case { index: 1 }).unwrap(),
        serde_json::json!({"kind": "case", "index": 1})
    );
    assert_eq!(
        serde_json::to_value(StepBranchView::Default).unwrap(),
        serde_json::json!({"kind": "default"})
    );
    assert_eq!(
        serde_json::to_value(StepIterationsView {
            started: 1,
            total: None,
            labels: vec![],
        })
        .unwrap(),
        serde_json::json!({"started": 1})
    );
    assert_eq!(
        serde_json::to_value(StepSkipReason::Condition).unwrap(),
        serde_json::json!("condition")
    );
}
