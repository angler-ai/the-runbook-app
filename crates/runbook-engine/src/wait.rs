//! Wait targets are resolved once, then driven by persisted timer/signal facts.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{StepRunId, StepStatus};
use runbook_spec::ir::{NodeKind, Wait, WaitKind};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitStart {
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    pub until_at_ms: Option<i64>,
    pub reads: Vec<runbook_expr::ReadRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitTarget {
    Time(i64),
    Signal(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitState {
    pub target: WaitTarget,
    pub timeout_at_ms: Option<i64>,
    pub generation: u32,
}
fn definition<'a>(state: &'a ExecutionState, step: &StepState) -> Result<&'a Wait, EngineError> {
    match &state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("wait declaration is absent"))?
        .kind
    {
        NodeKind::Wait(wait) => Ok(wait),
        _ => Err(EngineError::new("step.not_wait")),
    }
}
fn add_duration(at_ms: i64, millis: u64) -> Result<i64, EngineError> {
    at_ms
        .checked_add(i64::try_from(millis).map_err(|_| EngineError::new("wait.deadline_overflow"))?)
        .ok_or(EngineError::new("wait.deadline_overflow"))
}
/// Fault code for an `until` wait beyond the PRD §17 cap (engine version 4).
pub(crate) const WAIT_EXCEEDED: &str = "limit.wait_exceeded";
/// A wait starting at `at_ms` may park the run at most 7 days (PRD §17).
/// Runs recorded before engine version 4 keep any target.
fn exceeds_cap(state: &ExecutionState, target_ms: i64, at_ms: i64) -> bool {
    state.engine_version >= crate::version::WAIT_CAP
        && i128::from(target_ms) - i128::from(at_ms) > i128::from(runbook_spec::MAX_WAIT_MS)
}
pub(crate) fn prepare(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Vec<Command>, EngineError> {
    let wait = definition(state, step)?;
    let mut reads = vec![];
    let until_at_ms = if matches!(wait.kind, WaitKind::Until(_)) {
        let node = state
            .node(&step.step_id)
            .ok_or(EngineError::invariant("wait declaration is absent"))?;
        let value = crate::data::evaluate_scoped_path(
            state,
            &step.scope,
            &format!("{}/wait/until", node.source_path),
            &mut reads,
        )?;
        let timestamp: runbook_model::Timestamp = value
            .as_str()
            .ok_or(EngineError::new("wait.invalid_timestamp"))?
            .parse()
            .map_err(|_| EngineError::new("wait.invalid_timestamp"))?;
        // Millisecond event clocks must never wake before a submillisecond target.
        let floor = timestamp.timestamp_millis();
        Some(
            floor
                .checked_add(i64::from(
                    !timestamp.timestamp_subsec_nanos().is_multiple_of(1_000_000),
                ))
                .ok_or(EngineError::new("wait.deadline_overflow"))?,
        )
    } else {
        None
    };
    if let Some(target) = until_at_ms
        && exceeds_cap(state, target, state.at_ms)
    {
        return Ok(vec![Command::RecordFault {
            fault: crate::RuntimeFault {
                step_id: step.step_id.clone(),
                code: WAIT_EXCEEDED.into(),
                step_run_id: step.id.clone(),
            },
        }]);
    }
    Ok(vec![Command::StartWait {
        start: WaitStart {
            base_sequence: state.sequence,
            step_run_id: step
                .id
                .clone()
                .ok_or(EngineError::invariant("wait has no run ID"))?,
            until_at_ms,
            reads,
        },
    }])
}
pub(crate) fn start(
    state: &mut ExecutionState,
    start: &WaitStart,
    at_ms: i64,
) -> Result<usize, EngineError> {
    if state.sequence != start.base_sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let index = state.step_index(&start.step_run_id)?;
    let step = &state.steps[index];
    let wait = definition(state, step)?;
    if step.status != StepStatus::Ready
        || step.wait.is_some()
        || !crate::tree::is_frontier(state, &step.step_id)?
    {
        return Err(EngineError::new("wait.not_ready"));
    }
    if matches!(wait.kind, WaitKind::Until(_)) != start.until_at_ms.is_some() {
        return Err(EngineError::new("wait.invalid_target"));
    }
    let target = match &wait.kind {
        WaitKind::For(duration) => WaitTarget::Time(add_duration(at_ms, duration.millis)?),
        WaitKind::Until(_) => {
            let until = start
                .until_at_ms
                .ok_or(EngineError::new("wait.invalid_target"))?;
            if exceeds_cap(state, until, at_ms) {
                return Err(EngineError::new(WAIT_EXCEEDED));
            }
            WaitTarget::Time(until)
        }
        WaitKind::Signal(name) => WaitTarget::Signal(name.clone()),
    };
    let timeout_at_ms = wait
        .timeout
        .as_ref()
        .map(|duration| add_duration(at_ms, duration.millis))
        .transpose()?;
    let step = &mut state.steps[index];
    step.status = StepStatus::Waiting;
    step.wait = Some(WaitState {
        target,
        timeout_at_ms,
        generation: 1,
    });
    crate::data::append_reads(&mut step.reads, start.reads.iter().cloned());
    Ok(index)
}
pub(crate) fn timer(step: &StepState) -> Option<(i64, u32)> {
    let wait = step.wait.as_ref()?;
    let at = match (&wait.target, wait.timeout_at_ms) {
        (WaitTarget::Time(due), Some(timeout)) => (*due).min(timeout),
        (WaitTarget::Time(due), None) => *due,
        (WaitTarget::Signal(_), Some(timeout)) => timeout,
        _ => return None,
    };
    Some((at, wait.generation))
}
pub(crate) fn advance(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Vec<Command>, EngineError> {
    let Some((due, generation)) = timer(step) else {
        return Ok(vec![]);
    };
    if due > state.at_ms {
        return Ok(vec![]);
    }
    Ok(vec![Command::FireWaitTimer {
        step_run_id: step
            .id
            .clone()
            .ok_or(EngineError::invariant("wait has no run ID"))?,
        generation,
    }])
}
pub(crate) fn fire_timer(
    state: &mut ExecutionState,
    index: usize,
    generation: u32,
    at_ms: i64,
) -> Result<(), EngineError> {
    let target_wins_ties = state.engine_version >= crate::version::TIMEOUT_IS_FAILURE;
    let step = &mut state.steps[index];
    let (due, current) = timer(step).ok_or(EngineError::new("timer.stale_generation"))?;
    if step.status != StepStatus::Waiting || generation != current {
        return Err(EngineError::new("timer.stale_generation"));
    }
    if at_ms < due {
        return Err(EngineError::new("timer.not_due"));
    }
    let wait = step
        .wait
        .as_ref()
        .ok_or(EngineError::invariant("wait state disappeared"))?;
    // A wait reaching its target at the same instant as its timeout has
    // succeeded (F17); runs recorded under engine version 1 keep the tie
    // as a timeout.
    let timed_out = match (&wait.target, wait.timeout_at_ms) {
        (WaitTarget::Time(target), Some(timeout)) if target_wins_ties => timeout < *target,
        (_, timeout) => timeout.is_some_and(|timeout| timeout <= due),
    };
    step.status = if timed_out {
        StepStatus::TimedOut
    } else {
        StepStatus::Succeeded
    };
    step.finished_at_ms = Some(at_ms);
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalDelivery {
    pub base_sequence: u64,
    pub execution_id: runbook_model::ExecutionId,
    pub signal_id: String,
    pub name: String,
    #[serde(with = "runbook_spec::stored_data")]
    pub payload: runbook_model::Data,
    pub step_run_ids: Vec<StepRunId>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum WaitInput {
    Timer {
        execution_id: runbook_model::ExecutionId,
        step_run_id: StepRunId,
        generation: u32,
        at_ms: i64,
    },
    Signal {
        execution_id: runbook_model::ExecutionId,
        signal_id: String,
        name: String,
        payload: runbook_model::Data,
        at_ms: i64,
    },
}
fn signal_targets(
    state: &ExecutionState,
    name: &str,
    at_ms: i64,
) -> Result<Vec<StepRunId>, EngineError> {
    state
        .steps
        .iter()
        .filter(|step| step.status == StepStatus::Waiting)
        .filter(|step| {
            step.wait.as_ref().is_some_and(|wait| {
                matches!(&wait.target, WaitTarget::Signal(target) if target == name)
                    && wait.timeout_at_ms.is_none_or(|deadline| at_ms < deadline)
            })
        })
        .map(|step| {
            step.id
                .clone()
                .ok_or(EngineError::invariant("signal wait has no run ID"))
        })
        .collect()
}
/// Named signals wake the currently waiting matching instances, without buffering.
/// A persisted signal ID cannot wake a later iteration or be reused with new data.
pub fn advance_wait(
    state: &ExecutionState,
    input: &WaitInput,
) -> Result<Vec<Command>, EngineError> {
    match input {
        WaitInput::Timer {
            execution_id,
            step_run_id,
            generation,
            at_ms,
        } => {
            if execution_id != &state.id {
                return Err(EngineError::new("event.wrong_execution"));
            }
            if !crate::deadline::open(state) {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(step_run_id)?;
            let step = &state.steps[index];
            let (due, current) = timer(step).ok_or(EngineError::new("timer.stale_generation"))?;
            if step.status != StepStatus::Waiting || *generation != current {
                return Err(EngineError::new("timer.stale_generation"));
            }
            if *at_ms < due {
                return Err(EngineError::new("timer.not_due"));
            }
            Ok(vec![Command::FireWaitTimer {
                step_run_id: step_run_id.clone(),
                generation: *generation,
            }])
        }
        WaitInput::Signal {
            execution_id,
            signal_id,
            name,
            payload,
            at_ms,
        } => {
            if execution_id != &state.id {
                return Err(EngineError::new("event.wrong_execution"));
            }
            if signal_id.is_empty() {
                return Err(EngineError::new("signal.id_required"));
            }
            crate::data::guard_copy(payload)?;
            if let Some(previous) = state.signals.get(signal_id) {
                return if &previous.name == name && &previous.payload == payload {
                    Ok(vec![])
                } else {
                    Err(EngineError::new("signal.id_conflict"))
                };
            }
            if !crate::deadline::open(state) {
                return Err(EngineError::new("signal.not_waiting"));
            }
            let step_run_ids = signal_targets(state, name, *at_ms)?;
            if step_run_ids.is_empty() {
                return Err(EngineError::new("signal.not_waiting"));
            }
            // LANG9: a payload must match every waiting step's schema;
            // otherwise nothing is recorded and the steps keep waiting.
            for id in &step_run_ids {
                if let Some(schema) = payload_schema(state, state.step_index(id)?) {
                    let declared = declared_fields(schema, payload)?;
                    let errors = crate::report::output_errors(schema, &declared, true)?;
                    if !errors.is_empty() {
                        return Err(EngineError::Rejected {
                            code: "signal.invalid_payload",
                            errors: errors.into_iter().map(as_payload_error).collect(),
                        });
                    }
                }
            }
            Ok(vec![Command::ConsumeSignal {
                signal: Box::new(SignalDelivery {
                    base_sequence: state.sequence,
                    execution_id: execution_id.clone(),
                    signal_id: signal_id.clone(),
                    name: name.clone(),
                    payload: payload.clone(),
                    step_run_ids,
                }),
            }])
        }
    }
}
pub(crate) fn consume_signal(
    state: &mut ExecutionState,
    signal: &SignalDelivery,
    at_ms: i64,
) -> Result<Vec<usize>, EngineError> {
    if !crate::deadline::open(state) {
        return Err(EngineError::new("signal.not_waiting"));
    }
    if signal.execution_id != state.id {
        return Err(EngineError::new("event.wrong_execution"));
    }
    if signal.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    if signal.signal_id.is_empty() {
        return Err(EngineError::new("signal.id_required"));
    }
    if state.signals.contains_key(&signal.signal_id) {
        return Err(EngineError::new("signal.duplicate"));
    }
    crate::data::guard_copy(&signal.payload)?;
    let expected = signal_targets(state, &signal.name, at_ms)?;
    if expected.is_empty() || expected != signal.step_run_ids {
        return Err(EngineError::new("signal.not_waiting"));
    }
    let indices = expected
        .iter()
        .map(|id| state.step_index(id))
        .collect::<Result<Vec<_>, _>>()?;
    for &index in &indices {
        // LANG9: a checked payload becomes the step's outputs (only the
        // fields it declares), so later steps read `steps.x.outputs.y`.
        if let Some(schema) = payload_schema(state, index).cloned() {
            let declared = declared_fields(&schema, &signal.payload)?;
            if !crate::report::output_errors(&schema, &declared, true)?.is_empty() {
                return Err(EngineError::new("signal.invalid_payload"));
            }
            state.steps[index].outputs = declared;
        }
        state.steps[index].status = StepStatus::Succeeded;
        state.steps[index].finished_at_ms = Some(at_ms);
    }
    state
        .signals
        .insert(signal.signal_id.clone(), std::rc::Rc::new(signal.clone()));
    Ok(indices)
}

/// LANG9: the payload schema of the signal wait at `index`, if it has one.
fn payload_schema(state: &ExecutionState, index: usize) -> Option<&runbook_spec::DataSchema> {
    match &state.node(&state.steps[index].step_id)?.kind {
        NodeKind::Wait(Wait {
            outputs: Some(schema),
            ..
        }) => Some(schema),
        _ => None,
    }
}

/// LANG9: the payload's fields the schema declares. Others are ignored, as a
/// `call` step ignores outputs its child reports beyond those it declares:
/// senders often know more than a step asks for.
fn declared_fields(
    schema: &runbook_spec::DataSchema,
    payload: &runbook_model::Data,
) -> Result<runbook_model::Data, EngineError> {
    let runbook_spec::DataType::Object { properties } = &schema.kind else {
        return Err(EngineError::invariant("signal outputs are not an object"));
    };
    Ok(payload
        .iter()
        .filter(|(name, _)| properties.contains_key(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect())
}

/// A payload field error reads `payload.<name>`, the request's own field,
/// rather than the step's `outputs.<name>`.
fn as_payload_error(mut error: runbook_model::FieldError) -> runbook_model::FieldError {
    if let Some(rest) = error.path.strip_prefix("outputs") {
        error.path = format!("payload{rest}");
    }
    error
}
