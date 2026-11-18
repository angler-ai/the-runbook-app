//! Repeat-until decisions are persisted independently of historical findings.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{StepStatus, Verdict};
use runbook_spec::ir::{Node, NodeKind};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopState {
    pub admitted: u32,
    pub checked: u32,
    pub timer: Option<LoopTimer>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopTimer {
    pub generation: u32,
    pub due_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopCheck {
    pub base_sequence: u64,
    pub iteration: u32,
    pub satisfied: bool,
    pub reads: Vec<runbook_expr::ReadRef>,
}
fn definition<'a>(
    state: &'a ExecutionState,
    parent: &StepState,
) -> Result<&'a runbook_spec::ir::Loop, EngineError> {
    match &state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    {
        NodeKind::Loop(l) => Ok(l),
        _ => Err(EngineError::new("step.not_loop")),
    }
}
fn current_scope(parent: &StepState) -> Result<Vec<crate::IterationRef>, EngineError> {
    let iteration = parent
        .loop_state
        .as_ref()
        .ok_or(EngineError::new("loop.not_started"))?
        .admitted;
    let index = iteration
        .checked_sub(1)
        .ok_or(EngineError::new("loop.not_started"))?;
    Ok(crate::foreach::scope_for(parent, index))
}
pub(crate) fn frontier<'a>(
    state: &'a ExecutionState,
    parent: &'a StepState,
) -> Result<Vec<&'a StepState>, EngineError> {
    let l = definition(state, parent)?;
    let record = parent
        .loop_state
        .as_ref()
        .ok_or(EngineError::new("loop.not_started"))?;
    if parent.satisfied || parent.exhausted || record.admitted == record.checked {
        return Ok(vec![]);
    }
    let scope = current_scope(parent)?;
    if crate::tree::has_failure(state, &l.children, &scope)? {
        return Ok(vec![]);
    }
    crate::tree::sequence(state, &l.children, &scope)
}
pub fn advance_loop(state: &ExecutionState, node: &Node) -> Result<Vec<Command>, EngineError> {
    advance_for(state, crate::tree::unique_ready(state, node)?)
}
pub(crate) fn advance_for(
    state: &ExecutionState,
    parent: &StepState,
) -> Result<Vec<Command>, EngineError> {
    let l = definition(state, parent)?;
    let record = parent
        .loop_state
        .as_ref()
        .ok_or(EngineError::new("loop.not_started"))?;
    let id = parent
        .id
        .clone()
        .ok_or(EngineError::new("step.not_found"))?;
    if parent.satisfied
        || parent.exhausted
        || (record.admitted > 0
            && crate::tree::has_failure(state, &l.children, &current_scope(parent)?)?)
    {
        let (status, verdict) = outcome(state, parent)?;
        return Ok(vec![Command::CompleteStep {
            step_run_id: id,
            status,
            verdict,
        }]);
    }
    if record.admitted == record.checked {
        return Ok(vec![Command::StartLoopIteration {
            loop_run_id: id,
            index: record.admitted,
        }]);
    }
    let children = frontier(state, parent)?;
    if !children.is_empty() {
        return crate::decider::advance_nodes(state, &children);
    }
    let mut reads = vec![];
    let node = state
        .node(&parent.step_id)
        .ok_or(EngineError::invariant("parent declaration is absent"))?;
    // LANG1: `until` stops when true, `while` stops when false; both are
    // tested after the iteration's children, and the recorded check is
    // replayed as a fact, never re-evaluated.
    let condition = l.condition();
    let value = crate::data::evaluate_scoped_path(
        state,
        &current_scope(parent)?,
        &format!("{}/loop/{}", node.source_path, condition.key()),
        &mut reads,
    )?
    .as_bool()
    .ok_or(EngineError::new("expression.type_error"))?;
    let satisfied = condition.satisfied_by(value);
    Ok(vec![Command::RecordLoopCheck {
        step_run_id: id,
        check: LoopCheck {
            base_sequence: state.sequence,
            iteration: record.admitted,
            satisfied,
            reads,
        },
    }])
}
pub(crate) fn admit(
    state: &mut ExecutionState,
    parent_index: usize,
    index: u32,
) -> Result<(), EngineError> {
    let parent = &state.steps[parent_index];
    let l = definition(state, parent)?;
    let record = parent
        .loop_state
        .as_ref()
        .ok_or(EngineError::new("loop.not_started"))?;
    if parent.status != StepStatus::Ready
        || parent.satisfied
        || parent.exhausted
        || index != record.admitted
        || record.checked != record.admitted
        || record.timer.is_some()
        || index >= l.max_iterations
    {
        return Err(EngineError::new("loop.invalid_admission"));
    }
    let scope = crate::foreach::scope_for(parent, index);
    let prefix = format!("{}#{}", parent.step_id, index + 1);
    let mut nodes = vec![];
    crate::tree::collect_local(&l.children, &mut nodes);
    let rows: Vec<_> = nodes
        .iter()
        .map(|n| StepState::pending(n, format!("{prefix}.{}", n.id), scope.clone()))
        .collect();
    state.steps.extend(rows);
    state.steps[parent_index]
        .loop_state
        .as_mut()
        .ok_or(EngineError::invariant(
            "loop state disappeared during admission",
        ))?
        .admitted += 1;
    Ok(())
}
pub(crate) fn apply_check(
    state: &mut ExecutionState,
    parent_index: usize,
    check: &LoopCheck,
    at_ms: i64,
) -> Result<(), EngineError> {
    if check.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let parent = &state.steps[parent_index];
    let l = definition(state, parent)?;
    let record = parent
        .loop_state
        .as_ref()
        .ok_or(EngineError::new("loop.not_started"))?;
    if parent.status != StepStatus::Ready
        || parent.satisfied
        || parent.exhausted
        || record.admitted != check.iteration
        || record.checked >= check.iteration
        || !crate::tree::is_frontier(state, &parent.step_id)?
    {
        return Err(EngineError::new("loop.invalid_check"));
    }
    let scope = current_scope(parent)?;
    if crate::tree::has_failure(state, &l.children, &scope)?
        || !crate::tree::sequence(state, &l.children, &scope)?.is_empty()
    {
        return Err(EngineError::new("loop.invalid_check"));
    }
    let exhausted = !check.satisfied && check.iteration == l.max_iterations;
    let timer = if !check.satisfied && !exhausted {
        l.delay
            .as_ref()
            .map(|delay| {
                let millis = i64::try_from(delay.millis)
                    .map_err(|_| EngineError::new("loop.deadline_overflow"))?;
                let due_at_ms = at_ms
                    .checked_add(millis)
                    .ok_or(EngineError::new("loop.deadline_overflow"))?;
                Ok(LoopTimer {
                    generation: check.iteration,
                    due_at_ms,
                })
            })
            .transpose()?
    } else {
        None
    };
    let parent = &mut state.steps[parent_index];
    if timer.is_some() {
        parent.status = StepStatus::Waiting;
    }
    let record = parent.loop_state.as_mut().ok_or(EngineError::invariant(
        "loop state disappeared during check",
    ))?;
    record.timer = timer;
    record.checked = check.iteration;
    parent.satisfied = check.satisfied;
    parent.exhausted = exhausted;
    crate::data::append_reads(&mut parent.reads, check.reads.iter().cloned());
    Ok(())
}
pub(crate) fn outcome(
    state: &ExecutionState,
    parent: &StepState,
) -> Result<(StepStatus, Option<Verdict>), EngineError> {
    let l = definition(state, parent)?;
    let record = parent
        .loop_state
        .as_ref()
        .ok_or(EngineError::new("loop.not_started"))?;
    let failed = record.admitted > 0
        && crate::tree::has_failure(state, &l.children, &current_scope(parent)?)?;
    if !failed && !parent.satisfied && !parent.exhausted {
        return Err(EngineError::new("step.not_complete"));
    }
    if failed
        && state
            .node(&parent.step_id)
            .ok_or(EngineError::invariant("parent declaration is absent"))?
            .on_failure
            == runbook_spec::ir::FailurePolicy::Handoff
    {
        return Err(EngineError::new("engine.unsupported_failure_policy"));
    }
    let mut verdicts = vec![];
    for index in 0..record.admitted {
        let scope = crate::foreach::scope_for(parent, index);
        for node in &l.children {
            verdicts.push(crate::tree::record_in(state, node, &scope)?.verdict);
        }
    }
    if parent.exhausted {
        verdicts.push(Some(Verdict::Fail));
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

/// The caller supplies time and persists each returned event. No clock is sampled.
/// A stale or duplicated generation is rejected by apply, never re-admitted.
pub fn due_timers(state: &ExecutionState, at_ms: i64) -> Result<Vec<crate::Event>, EngineError> {
    if state.status != runbook_model::ExecutionStatus::Running {
        return Ok(vec![]);
    }
    let mut events = vec![];
    for step in state
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Waiting)
    {
        let timer = step
            .loop_state
            .as_ref()
            .and_then(|l| l.timer.as_ref())
            .map(|t| (t.due_at_ms, t.generation))
            .or_else(|| {
                step.retry_timer
                    .as_ref()
                    .map(|t| (t.due_at_ms, t.generation))
            });
        let timer = timer.or_else(|| crate::wait::timer(step));
        let Some((deadline, generation)) = timer else {
            continue;
        };
        let id = step
            .id
            .as_ref()
            .ok_or(EngineError::invariant("waiting timer has no step run ID"))?;
        if deadline <= at_ms {
            events.push(crate::Event::TimerFired {
                step_run_id: id.clone(),
                generation,
            });
        }
    }
    Ok(events)
}
pub(crate) fn fire_timer(
    state: &mut ExecutionState,
    index: usize,
    generation: u32,
    at_ms: i64,
) -> Result<(), EngineError> {
    let parent = &mut state.steps[index];
    let record = parent
        .loop_state
        .as_mut()
        .ok_or(EngineError::new("timer.not_found"))?;
    let timer = record
        .timer
        .as_ref()
        .ok_or(EngineError::new("timer.stale_generation"))?;
    if parent.status != StepStatus::Waiting || timer.generation != generation {
        return Err(EngineError::new("timer.stale_generation"));
    }
    if at_ms < timer.due_at_ms {
        return Err(EngineError::new("timer.not_due"));
    }
    record.timer = None;
    parent.status = StepStatus::Ready;
    Ok(())
}
