//! End decisions persist evaluated text; replay never evaluates the message again.
use crate::{EngineError, ExecutionState, StepState, TerminalCause};
use runbook_expr::ReadRef;
use runbook_model::{ExecutionStatus, StepRunId, StepStatus};
use runbook_spec::ir::{EndStatus, Node, NodeKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndSelected {
    pub step_run_id: StepRunId,
    pub base_sequence: u64,
    pub status: EndStatus,
    pub message: Option<String>,
    pub reads: Vec<ReadRef>,
}

pub fn prepare_end(state: &ExecutionState, node: &Node) -> Result<EndSelected, EngineError> {
    prepare_end_for(state, crate::tree::unique_ready(state, node)?)
}

pub(crate) fn prepare_end_for(
    state: &ExecutionState,
    step: &StepState,
) -> Result<EndSelected, EngineError> {
    validate_ready(state, step)?;
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::End(end) = &node.kind else {
        return Err(EngineError::new("step.not_end"));
    };
    let mut reads = vec![];
    let message = end
        .message
        .as_ref()
        .map(|_| {
            crate::data::evaluate_scoped_path(
                state,
                &step.scope,
                &format!("{}/end/message", node.source_path),
                &mut reads,
            )?
            .as_str()
            .map(str::to_owned)
            .ok_or(EngineError::new("expression.type_error"))
        })
        .transpose()?;
    Ok(EndSelected {
        step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
        base_sequence: state.sequence,
        status: end.status,
        message,
        reads,
    })
}

fn validate_ready(state: &ExecutionState, step: &StepState) -> Result<(), EngineError> {
    if state.status != ExecutionStatus::Running {
        return Err(EngineError::new("execution.not_running"));
    }
    if state.phase == crate::ExecutionPhase::Cleanup
        || state
            .node(&step.step_id)
            .is_some_and(|node| node.source_path.contains("/finally/"))
    {
        return Err(EngineError::new("engine.unsupported_end_in_cleanup"));
    }
    if step.status != StepStatus::Ready || !crate::tree::is_frontier(state, &step.step_id)? {
        return Err(EngineError::new("step.not_ready"));
    }
    Ok(())
}

pub(crate) fn apply_end(
    state: &mut ExecutionState,
    selected: &EndSelected,
    at_ms: i64,
    seq: u64,
) -> Result<(), EngineError> {
    if selected.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let index = state.step_index(&selected.step_run_id)?;
    validate_ready(state, &state.steps[index])?;
    let node = state
        .node(&state.steps[index].step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::End(end) = &node.kind else {
        return Err(EngineError::new("step.not_end"));
    };
    if selected.status != end.status || selected.message.is_some() != end.message.is_some() {
        return Err(EngineError::new("end.invalid_selection"));
    }
    if state.phase != crate::ExecutionPhase::Ordinary {
        return Err(EngineError::new("engine.unsupported_end_in_cleanup"));
    }
    let cause = match selected.status {
        EndStatus::Succeeded => TerminalCause::Succeeded,
        EndStatus::Failed => TerminalCause::Failed,
        EndStatus::Cancelled => TerminalCause::Cancelled,
    };
    let step = &mut state.steps[index];
    step.status = StepStatus::Succeeded;
    step.summary = selected.message.clone();
    step.finished_at_ms = Some(at_ms);
    crate::data::append_reads(&mut step.reads, selected.reads.iter().cloned());
    crate::data::note_step_revision(state, index, seq);
    crate::cleanup::enter(state, cause, Some("end".into()), at_ms, seq)
}
