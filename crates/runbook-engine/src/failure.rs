//! Persisted catch selection. A handler consumes failure without erasing history.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{StepRunId, StepStatus, Verdict};
use runbook_spec::ir::{FailurePolicy, NodeKind};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatchSelection {
    pub group_run_id: StepRunId,
    pub failed_run_id: StepRunId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupFinallyState {
    pub status: StepStatus,
    pub verdict: Option<Verdict>,
}
fn group<'a>(
    state: &'a ExecutionState,
    step: &StepState,
) -> Result<&'a runbook_spec::ir::Group, EngineError> {
    match &state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    {
        NodeKind::Group(g) => Ok(g),
        _ => Err(EngineError::new("step.not_group")),
    }
}
pub(crate) fn frontier<'a>(
    state: &'a ExecutionState,
    step: &'a StepState,
) -> Result<Vec<&'a StepState>, EngineError> {
    let g = group(state, step)?;
    let body = if step.group_finally.is_some() {
        &g.finally
    } else if step.catch_source.is_some() {
        &g.catch
    } else {
        &g.children
    };
    if crate::tree::has_failure(state, body, &step.scope)? {
        return Ok(vec![]);
    }
    crate::tree::sequence(state, body, &step.scope)
}
pub fn route_failure(state: &ExecutionState, failed: &str) -> Result<Vec<Command>, EngineError> {
    if state.status != runbook_model::ExecutionStatus::Running {
        return Ok(vec![]);
    }
    let failure = state
        .steps
        .iter()
        .find(|s| s.step_id == failed)
        .ok_or(EngineError::new("step.not_found"))?;
    if !crate::tree::failed(failure.status) {
        return Err(EngineError::new("step.not_failed"));
    }
    for step in crate::tree::frontier(state)? {
        if step.status != StepStatus::Ready
            || step.catch_source.is_some()
            || step.group_finally.is_some()
        {
            continue;
        }
        let Ok(g) = group(state, step) else {
            continue;
        };
        if g.catch.is_empty() {
            continue;
        }
        // LANG9: a typed catch handles only failures of its types; others
        // propagate as if the group had no catch.
        if !g.catch_on.is_empty() && !g.catch_on.contains(&failure_type(state, failure)?) {
            continue;
        }
        if crate::tree::first_failure(state, &g.children, &step.scope)?
            .is_some_and(|s| s.step_id == failed)
        {
            return Ok(vec![Command::ActivateCatch {
                selection: CatchSelection {
                    group_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
                    failed_run_id: failure
                        .id
                        .clone()
                        .ok_or(EngineError::new("step.not_found"))?,
                },
            }]);
        }
    }
    Ok(vec![])
}
/// LANG9: the type of a failed step's failure, from what the engine records:
/// `timeout` for a step that timed out or expired (or a task whose last
/// attempt timed out), `call_failed` for a failed child runbook, `failed`
/// otherwise. A container's failure takes the type of its earliest failed
/// step inside it.
pub fn failure_type(
    state: &ExecutionState,
    step: &StepState,
) -> Result<runbook_spec::ir::FailureType, EngineError> {
    use runbook_spec::ir::FailureType;
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let leaf = match &node.kind {
        NodeKind::Task(_) => Some(
            if step.status == StepStatus::TimedOut
                || step.attempt_history.last().is_some_and(|a| a.timed_out)
            {
                FailureType::Timeout
            } else {
                FailureType::Failed
            },
        ),
        NodeKind::Call(_) => Some(FailureType::CallFailed),
        NodeKind::Wait(_) | NodeKind::Approval(_) | NodeKind::Set(_) | NodeKind::End(_) => Some(
            if matches!(step.status, StepStatus::TimedOut | StepStatus::Expired) {
                FailureType::Timeout
            } else {
                FailureType::Failed
            },
        ),
        _ => None,
    };
    if let Some(kind) = leaf {
        return Ok(kind);
    }
    if matches!(step.status, StepStatus::TimedOut | StepStatus::Expired) {
        return Ok(FailureType::Timeout);
    }
    // A container: the earliest failed leaf below it, in this scope.
    let mut below = Vec::new();
    for children in crate::tree::children(node) {
        crate::tree::collect(children, &mut below);
    }
    let earliest = state
        .steps
        .iter()
        .filter(|s| {
            crate::tree::failed(s.status)
                && s.scope.starts_with(&step.scope)
                && below.iter().any(|n| {
                    n.id == s.node_id
                        && !matches!(
                            n.kind,
                            NodeKind::Group(_)
                                | NodeKind::Parallel { .. }
                                | NodeKind::Foreach(_)
                                | NodeKind::Loop(_)
                                | NodeKind::Switch(_)
                        )
                })
        })
        .min_by_key(|s| s.finished_at_ms.unwrap_or(i64::MAX));
    match earliest {
        Some(leaf) => failure_type(state, leaf),
        None => Ok(FailureType::Failed),
    }
}
/// LANG9: whether this group's catch handles the failure in its body: any
/// failure without `catch_on`, otherwise only its types. With nothing failed
/// yet, a catch is assumed to apply, as before.
fn catches(
    state: &ExecutionState,
    g: &runbook_spec::ir::Group,
    scope: &[crate::IterationRef],
) -> Result<bool, EngineError> {
    if g.catch.is_empty() {
        return Ok(false);
    }
    if g.catch_on.is_empty() {
        return Ok(true);
    }
    match crate::tree::first_failure(state, &g.children, scope)? {
        Some(failed) => Ok(g.catch_on.contains(&failure_type(state, failed)?)),
        None => Ok(true),
    }
}
pub(crate) fn advance_group(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Vec<Command>, EngineError> {
    let g = group(state, step)?;
    let children = frontier(state, step)?;
    if !children.is_empty() {
        return crate::decider::advance_nodes(state, &children);
    }
    if step.group_finally.is_none()
        && step.catch_source.is_none()
        && let Some(failed) = crate::tree::first_failure(state, &g.children, &step.scope)?
    {
        let commands = route_failure(state, &failed.step_id)?;
        if !commands.is_empty() {
            return Ok(commands);
        }
    }
    if step.group_finally.is_none() && !g.finally.is_empty() {
        let (status, verdict) = body_outcome(state, step)?;
        if status == StepStatus::Failed && propagates_to_execution(state, step)? {
            return crate::begin_cleanup(state, crate::TerminalCause::Failed, state.at_ms);
        }
        return Ok(vec![Command::BeginGroupFinally {
            group_run_id: step
                .id
                .clone()
                .ok_or(EngineError::invariant("group has no run ID"))?,
            outcome: GroupFinallyState { status, verdict },
        }]);
    }
    let (status, verdict) = outcome(state, step)?;
    Ok(vec![Command::CompleteStep {
        step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
        status,
        verdict,
    }])
}
pub(crate) fn validate_selection(
    state: &ExecutionState,
    selection: &CatchSelection,
) -> Result<usize, EngineError> {
    let index = state.step_index(&selection.group_run_id)?;
    let step = &state.steps[index];
    let g = group(state, step)?;
    if step.status != StepStatus::Ready
        || step.catch_source.is_some()
        || step.group_finally.is_some()
        || g.catch.is_empty()
        || !crate::tree::is_frontier(state, &step.step_id)?
    {
        return Err(EngineError::new("group.invalid_catch"));
    }
    let failed = crate::tree::first_failure(state, &g.children, &step.scope)?;
    if failed.and_then(|s| s.id.as_ref()) != Some(&selection.failed_run_id) {
        return Err(EngineError::new("group.invalid_catch"));
    }
    // LANG9: a recorded catch of a failure its group doesn't handle is refused.
    if let Some(failed) = failed
        && !g.catch_on.is_empty()
        && !g.catch_on.contains(&failure_type(state, failed)?)
    {
        return Err(EngineError::new("group.invalid_catch"));
    }
    Ok(index)
}
fn body_outcome(
    state: &ExecutionState,
    step: &StepState,
) -> Result<(StepStatus, Option<Verdict>), EngineError> {
    let g = group(state, step)?;
    let body = if step.catch_source.is_some() {
        &g.catch
    } else {
        &g.children
    };
    let failed = crate::tree::has_failure(state, body, &step.scope)?;
    if failed && step.catch_source.is_none() && catches(state, g, &step.scope)? {
        return Err(EngineError::new("group.catch_required"));
    }
    if !failed && !crate::tree::sequence(state, body, &step.scope)?.is_empty() {
        return Err(EngineError::new("step.not_complete"));
    }
    if failed
        && state
            .node(&step.step_id)
            .ok_or(EngineError::invariant("step declaration is absent"))?
            .on_failure
            == FailurePolicy::Handoff
    {
        return Err(EngineError::new("engine.unsupported_failure_policy"));
    }
    let mut verdicts = vec![];
    for node in g.children.iter().chain(if step.catch_source.is_some() {
        g.catch.iter()
    } else {
        [].iter()
    }) {
        verdicts.push(crate::tree::record_in(state, node, &step.scope)?.verdict);
    }
    Ok((
        if failed {
            StepStatus::Failed
        } else {
            StepStatus::Succeeded
        },
        crate::report::worst_verdict(verdicts.into_iter()),
    ))
}

pub(crate) fn begin_finally(
    state: &mut ExecutionState,
    id: &StepRunId,
    outcome: &GroupFinallyState,
) -> Result<usize, EngineError> {
    let index = state.step_index(id)?;
    let step = &state.steps[index];
    let g = group(state, step)?;
    if step.status != StepStatus::Ready
        || step.group_finally.is_some()
        || g.finally.is_empty()
        || !crate::tree::is_frontier(state, &step.step_id)?
        || body_outcome(state, step)? != (outcome.status, outcome.verdict)
    {
        return Err(EngineError::new("group.invalid_finally"));
    }
    state.steps[index].group_finally = Some(outcome.clone());
    Ok(index)
}
pub(crate) fn outcome(
    state: &ExecutionState,
    step: &StepState,
) -> Result<(StepStatus, Option<Verdict>), EngineError> {
    let g = group(state, step)?;
    if let Some(original) = &step.group_finally {
        let failed = crate::tree::has_failure(state, &g.finally, &step.scope)?;
        if !failed && !crate::tree::sequence(state, &g.finally, &step.scope)?.is_empty() {
            return Err(EngineError::new("step.not_complete"));
        }
        let verdicts = g
            .finally
            .iter()
            .map(|node| crate::tree::record_in(state, node, &step.scope).map(|s| s.verdict))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((
            if failed {
                StepStatus::Failed
            } else {
                original.status
            },
            crate::report::worst_verdict(std::iter::once(original.verdict).chain(verdicts)),
        ));
    }
    if !g.finally.is_empty() {
        return Err(EngineError::new("group.finally_required"));
    }
    body_outcome(state, step)
}

/// A failed sequential region with no remaining handler can enter terminal cleanup
/// before its finally starts. Join-all parents must still finish their other work.
fn propagates_to_execution(
    state: &ExecutionState,
    failed: &StepState,
) -> Result<bool, EngineError> {
    if state.phase != crate::ExecutionPhase::Ordinary {
        return Ok(false);
    }
    for parent in &state.steps {
        let node = state
            .node(&parent.step_id)
            .ok_or(EngineError::invariant("enclosing node absent"))?;
        let enclosing = parent.step_id == failed.step_id
            || (parent.status == StepStatus::Ready
                && failed.scope.starts_with(&parent.scope)
                && crate::tree::children(node)
                    .iter()
                    .any(|body| crate::tree::find(body, &failed.node_id).is_some()));
        if !enclosing {
            continue;
        }
        if node.on_failure == FailurePolicy::Continue {
            return Ok(false);
        }
        if parent.step_id == failed.step_id {
            continue;
        }
        match &node.kind {
            NodeKind::Group(g)
                if parent.catch_source.is_none()
                    && parent.group_finally.is_none()
                    && crate::tree::find(&g.children, &failed.node_id).is_some() =>
            {
                if catches(state, g, &parent.scope)? {
                    return Ok(false);
                }
            }
            NodeKind::Parallel {
                fail_fast: false, ..
            }
            | NodeKind::Foreach(_) => return Ok(false),
            _ => {}
        }
    }
    Ok(true)
}
pub(crate) fn terminal_cleanup_required(state: &ExecutionState) -> Result<bool, EngineError> {
    for step in crate::tree::frontier(state)? {
        if step.status != StepStatus::Ready || step.group_finally.is_some() {
            continue;
        }
        let Ok(g) = group(state, step) else {
            continue;
        };
        if g.finally.is_empty() || (step.catch_source.is_none() && catches(state, g, &step.scope)?)
        {
            continue;
        }
        if body_outcome(state, step)?.0 == StepStatus::Failed
            && propagates_to_execution(state, step)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
