//! Checklist ticks are task data; they never create additional step runs.
use crate::{EngineError, Event, ExecutionState};
use runbook_model::{ChecklistTick, StepRunId, StepStatus};
use runbook_spec::ir::NodeKind;

fn task_index(state: &ExecutionState, id: &StepRunId) -> Result<usize, EngineError> {
    if !crate::deadline::open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    let index = state.step_index(id)?;
    let record = &state.steps[index];
    if !matches!(record.status, StepStatus::Ready | StepStatus::Claimed) {
        return Err(EngineError::new("step.not_reportable"));
    }
    let node = state
        .node(&record.step_id)
        .ok_or(EngineError::invariant("checklist node absent"))?;
    let NodeKind::Task(task) = &node.kind else {
        return Err(EngineError::new("step.not_task"));
    };
    if record.checklist.len() != task.checklist.len()
        || record
            .checklist
            .iter()
            .zip(&task.checklist)
            .any(|(tick, title)| tick.title != *title)
    {
        return Err(EngineError::invariant(
            "stored checklist differs from declaration",
        ));
    }
    Ok(index)
}
/// Envelope time and claim authorization are checked again when the event commits.
pub fn update_checklist(
    state: &ExecutionState,
    id: &StepRunId,
    ticks: &[bool],
) -> Result<Event, EngineError> {
    crate::authorize_at_deadline(state, state.at_ms)?;
    let index = task_index(state, id)?;
    let record = &state.steps[index];
    if record
        .deadline_at_ms
        .is_some_and(|deadline| state.at_ms >= deadline)
    {
        return Err(EngineError::new("step.deadline_exceeded"));
    }
    if ticks.len() != record.checklist.len() {
        return Err(EngineError::new("step.invalid_checklist"));
    }
    Ok(Event::ChecklistUpdated {
        step_run_id: id.clone(),
        ticks: record
            .checklist
            .iter()
            .zip(ticks)
            .map(|(item, completed)| ChecklistTick {
                title: item.title.clone(),
                completed: *completed,
            })
            .collect(),
    })
}
pub(crate) fn validate(
    state: &ExecutionState,
    id: &StepRunId,
    ticks: &[ChecklistTick],
) -> Result<usize, EngineError> {
    let index = task_index(state, id)?;
    let record = &state.steps[index];
    if ticks.len() != record.checklist.len()
        || ticks
            .iter()
            .zip(&record.checklist)
            .any(|(a, b)| a.title != b.title)
    {
        return Err(EngineError::new("step.invalid_checklist"));
    }
    Ok(index)
}
