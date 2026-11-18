//! LANG4 `call`: a step that runs a pinned version of another runbook as a
//! child execution and takes the child's outcome as its own.
//!
//! The engine records three facts, all system events:
//! - `CallRequested` when the step starts: its child's execution ID (from
//!   the host, derived from this run and the step run) and the inputs it
//!   evaluated; the step waits. The host starts the child from this fact.
//! - `CallStarted` once the child's start committed (the link, both ways).
//! - `CallCompleted` with the child's terminal status, verdict and outputs,
//!   or with the code that refused its start. The step succeeds only when
//!   the child succeeded and its outputs satisfy the step's `outputs`.
//!
//! The engine never starts, cancels or reads another execution.
use crate::{EngineError, ExecutionState, StepState};
use runbook_model::{Data, ExecutionId, ExecutionStatus, StepRunId, StepStatus, Verdict};
use runbook_spec::ir::NodeKind;
use serde::{Deserialize, Serialize};

/// What the decider asks for when a call step is ready; the host adds the
/// child's execution ID and records it as [`CallStart`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallRequest {
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    #[serde(with = "runbook_spec::stored_data")]
    pub inputs: Data,
    pub reads: Vec<runbook_expr::ReadRef>,
}

/// `CallRequested`: the request and the child it will start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallStart {
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    pub child_execution_id: ExecutionId,
    #[serde(with = "runbook_spec::stored_data")]
    pub inputs: Data,
    pub reads: Vec<runbook_expr::ReadRef>,
}

/// `CallCompleted`: how the child ended, or why it never started.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallOutcome {
    pub step_run_id: StepRunId,
    pub child_execution_id: ExecutionId,
    /// The child's terminal status; `failed` for a start that was refused.
    pub status: ExecutionStatus,
    pub verdict: Option<Verdict>,
    /// The child's execution outputs (`spec.outputs`), as it reported them.
    #[serde(with = "runbook_spec::stored_data")]
    pub outputs: Data,
    /// The refusal code when the child could not start (for example
    /// `limit.executions_monthly`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A call step's link to its child.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallState {
    pub child_execution_id: ExecutionId,
    #[serde(with = "runbook_spec::stored_data")]
    pub inputs: Data,
    pub requested_at_ms: i64,
    /// The child's start committed.
    pub started: bool,
    /// The child's outcome (or its refusal) was recorded.
    pub completed: bool,
    /// Why the step failed: a refused start (`limit.executions_monthly`,
    /// `execution.inputs_invalid`, …), `call.child_failed`,
    /// `call.child_cancelled` or `call.outputs_invalid`.
    pub error: Option<String>,
}

fn definition<'a>(
    state: &'a ExecutionState,
    step: &StepState,
) -> Result<&'a runbook_spec::ir::Call, EngineError> {
    match &state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("call declaration is absent"))?
        .kind
    {
        NodeKind::Call(call) => Ok(call),
        _ => Err(EngineError::new("step.not_call")),
    }
}

/// A ready call step: evaluate `with` in its scope.
pub(crate) fn prepare(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Vec<crate::Command>, EngineError> {
    let call = definition(state, step)?;
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("call declaration is absent"))?;
    let mut reads = vec![];
    let mut inputs = Data::new();
    for name in call.with.keys() {
        let path = format!(
            "{}/call/with/{}",
            node.source_path,
            name.replace('~', "~0").replace('/', "~1")
        );
        let value = crate::data::evaluate_scoped_path(state, &step.scope, &path, &mut reads)?;
        inputs.insert(name.clone(), value);
    }
    crate::data::guard_copy(&inputs)?;
    Ok(vec![crate::Command::RequestCall {
        request: CallRequest {
            base_sequence: state.sequence,
            step_run_id: step
                .id
                .clone()
                .ok_or(EngineError::invariant("call has no run ID"))?,
            inputs,
            reads,
        },
    }])
}

pub(crate) fn request(
    state: &mut ExecutionState,
    start: &CallStart,
    at_ms: i64,
) -> Result<usize, EngineError> {
    if state.sequence != start.base_sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let index = state.step_index(&start.step_run_id)?;
    let step = &state.steps[index];
    definition(state, step)?;
    if step.status != StepStatus::Ready
        || step.call.is_some()
        || !crate::tree::is_frontier(state, &step.step_id)?
    {
        return Err(EngineError::new("call.not_ready"));
    }
    crate::data::guard_copy(&start.inputs)?;
    let step = &mut state.steps[index];
    step.status = StepStatus::Waiting;
    step.call = Some(CallState {
        child_execution_id: start.child_execution_id.clone(),
        inputs: start.inputs.clone(),
        requested_at_ms: at_ms,
        started: false,
        completed: false,
        error: None,
    });
    crate::data::append_reads(&mut step.reads, start.reads.iter().cloned());
    Ok(index)
}

fn waiting_call<'a>(
    state: &'a ExecutionState,
    step_run_id: &StepRunId,
    child: &ExecutionId,
) -> Result<(usize, &'a CallState), EngineError> {
    if !crate::deadline::open(state) {
        return Err(EngineError::new("call.not_waiting"));
    }
    let index = state.step_index(step_run_id)?;
    let step = &state.steps[index];
    match &step.call {
        Some(call)
            if step.status == StepStatus::Waiting
                && !call.completed
                && &call.child_execution_id == child =>
        {
            Ok((index, call))
        }
        Some(call) if &call.child_execution_id != child => {
            Err(EngineError::new("call.wrong_child"))
        }
        _ => Err(EngineError::new("call.not_waiting")),
    }
}

/// Whether `step_run_id` still waits for `child`'s start to be recorded.
pub fn awaits_start(state: &ExecutionState, step_run_id: &StepRunId, child: &ExecutionId) -> bool {
    waiting_call(state, step_run_id, child).is_ok_and(|(_, call)| !call.started)
}

/// Whether `step_run_id` still waits for `child`'s outcome.
pub fn awaits_outcome(
    state: &ExecutionState,
    step_run_id: &StepRunId,
    child: &ExecutionId,
) -> bool {
    waiting_call(state, step_run_id, child).is_ok()
}

pub(crate) fn started(
    state: &mut ExecutionState,
    step_run_id: &StepRunId,
    child: &ExecutionId,
) -> Result<usize, EngineError> {
    let (index, call) = waiting_call(state, step_run_id, child)?;
    if call.started {
        return Err(EngineError::new("call.already_started"));
    }
    if let Some(call) = state.steps[index].call.as_mut() {
        call.started = true;
    }
    Ok(index)
}

pub(crate) fn complete(
    state: &mut ExecutionState,
    outcome: &CallOutcome,
    at_ms: i64,
) -> Result<usize, EngineError> {
    let (index, _) = waiting_call(state, &outcome.step_run_id, &outcome.child_execution_id)?;
    if outcome.error.is_some() && outcome.status != ExecutionStatus::Failed {
        return Err(EngineError::new("call.invalid_outcome"));
    }
    if !matches!(
        outcome.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    ) {
        return Err(EngineError::new("call.invalid_outcome"));
    }
    crate::data::guard_copy(&outcome.outputs)?;
    let declared = &definition(state, &state.steps[index])?.outputs;
    let (status, outputs, error) = match (&outcome.error, outcome.status) {
        (Some(code), _) => (StepStatus::Failed, Data::new(), Some(code.clone())),
        (None, ExecutionStatus::Succeeded) => {
            let runbook_spec::DataType::Object { properties } = &declared.kind else {
                return Err(EngineError::invariant("call outputs are not an object"));
            };
            // Only the outputs the step declares; the child may report more.
            let projected: Data = outcome
                .outputs
                .iter()
                .filter(|(name, _)| properties.contains_key(*name))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if crate::report::output_errors(declared, &projected, true)?.is_empty() {
                (StepStatus::Succeeded, projected, None)
            } else {
                (
                    StepStatus::Failed,
                    Data::new(),
                    Some("call.outputs_invalid".into()),
                )
            }
        }
        (None, ExecutionStatus::Cancelled) => (
            StepStatus::Failed,
            Data::new(),
            Some("call.child_cancelled".into()),
        ),
        (None, _) => (
            StepStatus::Failed,
            Data::new(),
            Some("call.child_failed".into()),
        ),
    };
    let step = &mut state.steps[index];
    step.status = status;
    step.outputs = outputs;
    step.verdict = outcome.verdict;
    step.finished_at_ms = Some(at_ms);
    if let Some(call) = step.call.as_mut() {
        call.completed = true;
        call.error = error;
    }
    Ok(index)
}
