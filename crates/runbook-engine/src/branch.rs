//! Guards and branch choices are evaluated by the decider, then persisted as facts.
use crate::{EngineError, ExecutionState};
use runbook_expr::ReadRef;
use runbook_spec::ir::Node;

pub(crate) struct Condition {
    pub passes: bool,
    pub reads: Vec<ReadRef>,
}
pub(crate) fn condition(
    state: &ExecutionState,
    step: &crate::StepState,
    path: &str,
) -> Result<Condition, EngineError> {
    let mut reads = vec![];
    let value = crate::data::evaluate_scoped_path(state, &step.scope, path, &mut reads)?;
    Ok(Condition {
        passes: value
            .as_bool()
            .ok_or(EngineError::new("expression.type_error"))?,
        reads,
    })
}
pub(crate) fn guard(
    state: &ExecutionState,
    step: &crate::StepState,
) -> Result<Condition, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    if node.guard.is_none() {
        return Ok(Condition {
            passes: true,
            reads: vec![],
        });
    }
    condition(state, step, &format!("{}/if", node.source_path))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "index", rename_all = "snake_case")]
pub enum BranchChoice {
    Case(u32),
    Default,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BranchSelected {
    pub step_run_id: runbook_model::StepRunId,
    pub base_sequence: u64,
    pub choice: BranchChoice,
    pub reads: Vec<ReadRef>,
}
pub fn choose_branch(state: &ExecutionState, node: &Node) -> Result<BranchSelected, EngineError> {
    choose_branch_for(state, crate::tree::unique_ready(state, node)?)
}
pub(crate) fn choose_branch_for(
    state: &ExecutionState,
    step: &crate::StepState,
) -> Result<BranchSelected, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let runbook_spec::ir::NodeKind::Switch(switch) = &node.kind else {
        return Err(EngineError::new("step.not_switch"));
    };
    if step.status != runbook_model::StepStatus::Ready
        || step.branch.is_some()
        || !crate::tree::is_frontier(state, &step.step_id)?
    {
        return Err(EngineError::new("step.not_ready"));
    }
    let mut selected = BranchSelected {
        step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
        base_sequence: state.sequence,
        choice: BranchChoice::Default,
        reads: vec![],
    };
    for (index, case) in switch.cases.iter().enumerate() {
        let tested = condition(state, step, &format!("{}/when", case.source_path))?;
        crate::data::append_reads(&mut selected.reads, tested.reads);
        if tested.passes {
            selected.choice = BranchChoice::Case(index as u32);
            break;
        }
    }
    Ok(selected)
}
pub(crate) fn validate_selection(
    state: &ExecutionState,
    selection: &BranchSelected,
) -> Result<usize, EngineError> {
    if selection.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let index = state.step_index(&selection.step_run_id)?;
    let step = &state.steps[index];
    if step.status != runbook_model::StepStatus::Ready
        || step.branch.is_some()
        || !crate::tree::is_frontier(state, &step.step_id)?
    {
        return Err(EngineError::new("step.not_ready"));
    }
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    crate::tree::selected(node, selection.choice)?;
    Ok(index)
}
pub(crate) fn completed_outcome(
    state: &ExecutionState,
    step: &crate::StepState,
) -> Result<(runbook_model::StepStatus, Option<runbook_model::Verdict>), EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    if matches!(node.kind, runbook_spec::ir::NodeKind::Group(_)) {
        return crate::failure::outcome(state, step);
    }
    if matches!(node.kind, runbook_spec::ir::NodeKind::Loop(_)) {
        return crate::loop_node::outcome(state, step);
    }
    if matches!(node.kind, runbook_spec::ir::NodeKind::Foreach(_)) {
        return crate::foreach::outcome(state, step);
    }
    let children = match &node.kind {
        runbook_spec::ir::NodeKind::Parallel { children, .. } => children.as_slice(),
        _ => crate::tree::selected(
            node,
            step.branch
                .ok_or(EngineError::new("step.branch_not_selected"))?,
        )?,
    };
    // LANG2: an `at_least` join fails when it can no longer be met; branch
    // failures before that are absorbed. Join-all fails on any failure.
    let at_least = crate::parallel::at_least_outcome(state, step)?;
    let failed = match at_least {
        Some(met) => !met,
        None => crate::tree::has_failure(state, children, &step.scope)?,
    };
    if failed && node.on_failure == runbook_spec::ir::FailurePolicy::Handoff {
        return Err(EngineError::new("engine.unsupported_failure_policy"));
    }
    let settled_early = match node.kind {
        runbook_spec::ir::NodeKind::Parallel { .. } => crate::parallel::settled_early(state, step)?,
        runbook_spec::ir::NodeKind::Switch(_) => failed,
        _ => false,
    };
    let mut verdicts = vec![];
    for child in children {
        let record = crate::tree::record_in(state, child, &step.scope)?;
        let unstarted_after_failure = settled_early
            && record.status == runbook_model::StepStatus::Pending
            && record.id.is_none();
        if !crate::tree::terminal(record.status) && !unstarted_after_failure {
            return Err(EngineError::new("step.not_complete"));
        }
        // LANG2: a met `at_least` join takes its finding from the branches
        // counted toward it; absorbed and cancelled losers add none.
        if at_least == Some(true) && record.status != runbook_model::StepStatus::Succeeded {
            continue;
        }
        verdicts.push(record.verdict);
    }
    Ok((
        if failed {
            runbook_model::StepStatus::Failed
        } else {
            runbook_model::StepStatus::Succeeded
        },
        crate::report::worst_verdict(verdicts.into_iter()),
    ))
}
