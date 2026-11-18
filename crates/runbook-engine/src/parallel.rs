//! Join scheduling over persisted branch state: `all` (with optional
//! `fail_fast`) and, from LANG2, `any` / `at_least: n`.
use crate::{Command, EngineError, ExecutionState};
use runbook_model::StepStatus;
use runbook_spec::ir::{Join, Node, NodeKind};

/// LANG2: the outcome of an `at_least` join once it is decided: `Some(true)`
/// when `n` branches succeeded, `Some(false)` when fewer than `n` could still
/// succeed, `None` while undecided or for a join-all parallel. Only
/// `succeeded` branches count; skipped, failed and cancelled ones do not.
pub(crate) fn at_least_outcome(
    state: &ExecutionState,
    record: &crate::StepState,
) -> Result<Option<bool>, EngineError> {
    let node = state
        .node(&record.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Parallel {
        children,
        join: Join::AtLeast(n),
        ..
    } = &node.kind
    else {
        return Ok(None);
    };
    let (mut succeeded, mut open) = (0u32, 0u32);
    for child in children {
        let branch = crate::tree::record_in(state, child, &record.scope)?;
        if branch.status == StepStatus::Succeeded {
            succeeded += 1;
        } else if !crate::tree::terminal(branch.status) {
            open += 1;
        }
    }
    Ok(if succeeded >= *n {
        Some(true)
    } else if succeeded + open < *n {
        Some(false)
    } else {
        None
    })
}

/// Whether the parallel stops scheduling its branches before they all
/// finish: join-all with `fail_fast` after a failure, or a decided
/// `at_least` join. Its open branches are then cancelled.
pub(crate) fn settled_early(
    state: &ExecutionState,
    record: &crate::StepState,
) -> Result<bool, EngineError> {
    let node = state
        .node(&record.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Parallel {
        children,
        fail_fast,
        join,
    } = &node.kind
    else {
        return Err(EngineError::new("step.not_parallel"));
    };
    match join {
        Join::All => Ok(*fail_fast && crate::tree::has_failure(state, children, &record.scope)?),
        Join::AtLeast(_) => Ok(at_least_outcome(state, record)?.is_some()),
    }
}
pub fn advance_parallel(state: &ExecutionState, node: &Node) -> Result<Vec<Command>, EngineError> {
    advance_for(state, crate::tree::unique_ready(state, node)?)
}
pub(crate) fn advance_for(
    state: &ExecutionState,
    record: &crate::StepState,
) -> Result<Vec<Command>, EngineError> {
    let node = state
        .node(&record.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Parallel { .. } = &node.kind else {
        return Err(EngineError::new("step.not_parallel"));
    };
    if record.status != StepStatus::Ready {
        return Err(EngineError::new("step.not_ready"));
    }
    let frontier = crate::tree::frontier_step(state, record)?;
    if frontier.len() == 1 && frontier[0].step_id == record.step_id {
        let targets = cancellation_targets(state, record)?;
        if !targets.is_empty() {
            return Ok(vec![Command::CancelSiblings {
                parallel_run_id: record
                    .id
                    .clone()
                    .ok_or(EngineError::new("step.not_found"))?,
                step_run_ids: targets,
            }]);
        }
        let (status, verdict) = crate::branch::completed_outcome(state, record)?;
        Ok(vec![Command::CompleteStep {
            step_run_id: record
                .id
                .clone()
                .ok_or(EngineError::new("step.not_found"))?,
            status,
            verdict,
        }])
    } else {
        crate::decider::advance_nodes(state, &frontier)
    }
}

/// Cancellation covers materialized open descendants only. Unentered branches
/// keep no run IDs, and already completed work retains its outcome.
pub(crate) fn cancellation_targets(
    state: &ExecutionState,
    parent: &crate::StepState,
) -> Result<Vec<runbook_model::StepRunId>, EngineError> {
    let node = state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Parallel { children, .. } = &node.kind else {
        return Err(EngineError::new("step.not_parallel"));
    };
    if !settled_early(state, parent)? {
        return Ok(vec![]);
    }
    let mut nodes = vec![];
    crate::tree::collect(children, &mut nodes);
    let mut targets = vec![];
    for record in &state.steps {
        if record.scope.starts_with(&parent.scope)
            && nodes.iter().any(|n| n.id == record.node_id)
            && !crate::tree::terminal(record.status)
            && let Some(id) = &record.id
        {
            let mut cleanup_work = false;
            for owner in &state.steps {
                if owner.id.is_some()
                    && !owner.group_finally_done
                    && owner.scope.starts_with(&parent.scope)
                    && nodes.iter().any(|n| n.id == owner.node_id)
                    && matches!(state.node(&owner.step_id).map(|n| &n.kind), Some(NodeKind::Group(g)) if !g.finally.is_empty())
                    && let Some(owner_id) = &owner.id
                    && crate::cleanup::contains(
                        state,
                        &crate::CleanupRoot::Group(owner_id.clone()),
                        record,
                    )?
                {
                    cleanup_work = true;
                    break;
                }
            }
            if cleanup_work {
                continue;
            }
            targets.push(id.clone());
        }
    }
    Ok(targets)
}
