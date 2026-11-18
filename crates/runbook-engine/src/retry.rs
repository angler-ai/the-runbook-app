//! Attempts remain immutable history; retries and human continuation use one step run.
use crate::{Command, EngineError, ExecutionState, StepState, WorkReport};
use runbook_model::{ActorRef, ReportStatus, StepStatus};
use runbook_spec::ir::{FailurePolicy, Node, NodeKind};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub actor: ActorRef,
    pub at_ms: i64,
    pub report: WorkReport,
    /// The task deadline ended this attempt; `report` is the failure it
    /// stands for (PRD §6: `failed` covers a timeout), not an actor report.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryTimer {
    pub generation: u32,
    pub due_at_ms: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureAction {
    RetryAt(i64),
    Handoff,
    Propagate,
}
pub fn failure_action(state: &ExecutionState, node: &Node) -> Result<FailureAction, EngineError> {
    let mut records = state
        .steps
        .iter()
        .filter(|s| s.node_id == node.id && s.recovery_pending);
    let step = records.next().ok_or(EngineError::new("step.not_failed"))?;
    if records.next().is_some() {
        return Err(EngineError::new("step.ambiguous_instance"));
    }
    failure_action_for(state, step)
}
pub(crate) fn failure_action_for(
    state: &ExecutionState,
    step: &StepState,
) -> Result<FailureAction, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Task(task) = &node.kind else {
        return Err(EngineError::new("step.not_task"));
    };
    let attempt = step
        .attempt_history
        .last()
        .ok_or(EngineError::new("step.no_attempt"))?;
    if !matches!(
        attempt.report.status,
        ReportStatus::Failed | ReportStatus::Blocked
    ) {
        return Err(EngineError::new("step.not_failed"));
    }
    if step.human_continuation {
        return Ok(FailureAction::Propagate);
    }
    if attempt.report.status == ReportStatus::Blocked {
        return Ok(FailureAction::Handoff);
    }
    // LANG9: `retry.on` retries only failures of those types.
    let retryable = task.retry.on.is_empty()
        || task.retry.on.contains(&if attempt.timed_out {
            runbook_spec::ir::FailureType::Timeout
        } else {
            runbook_spec::ir::FailureType::Failed
        });
    if retryable && step.attempt < task.retry.max_attempts {
        let delay = i64::try_from(task.retry.backoff.as_ref().map_or(0, |d| d.millis))
            .map_err(|_| EngineError::new("retry.deadline_overflow"))?;
        let at = attempt
            .at_ms
            .checked_add(delay)
            .ok_or(EngineError::new("retry.deadline_overflow"))?;
        return Ok(FailureAction::RetryAt(at));
    }
    Ok(if node.on_failure == FailurePolicy::Handoff {
        FailureAction::Handoff
    } else {
        FailureAction::Propagate
    })
}
pub(crate) fn advance(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Vec<Command>, EngineError> {
    if !step.recovery_pending {
        return Ok(vec![]);
    }
    match failure_action_for(state, step)? {
        FailureAction::RetryAt(due_at_ms) => Ok(vec![Command::ScheduleRetry {
            step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
            attempt: step.attempt,
            due_at_ms,
        }]),
        FailureAction::Handoff => Ok(vec![Command::ResolveHandoff {
            request: Box::new(crate::handoff::request(state, step)?),
        }]),
        FailureAction::Propagate => Err(EngineError::new("step.invalid_recovery")),
    }
}
pub(crate) fn ready_next(step: &mut StepState) -> Result<(), EngineError> {
    step.attempt = step
        .attempt
        .checked_add(1)
        .ok_or(EngineError::new("step.attempt_overflow"))?;
    step.status = StepStatus::Ready;
    step.outputs.clear();
    step.verdict = None;
    step.summary = None;
    step.actor = None;
    step.taken_over = None;
    step.finished_at_ms = None;
    step.retry_timer = None;
    step.recovery_pending = false;
    Ok(())
}
/// Re-arm the task deadline for a new attempt or a handoff: each attempt
/// gets the task's full `timeout` from when it becomes ready again (F4).
/// Runs recorded before engine version 2 keep the original deadline.
pub(crate) fn rearm(
    state: &mut ExecutionState,
    index: usize,
    at_ms: i64,
) -> Result<(), EngineError> {
    if state.engine_version < crate::version::TIMEOUT_IS_FAILURE
        || state.steps[index].status != StepStatus::Ready
    {
        return Ok(());
    }
    let timeout = state
        .node(&state.steps[index].step_id)
        .and_then(|node| match node.kind {
            NodeKind::Task(_) => node.timeout.as_ref().map(|d| d.millis),
            _ => None,
        });
    state.steps[index].deadline_at_ms = timeout
        .map(|millis| crate::deadline::add(at_ms, millis))
        .transpose()?;
    Ok(())
}
/// Enter recovery after a failed attempt when retry or handoff applies.
pub(crate) fn recover(state: &mut ExecutionState, index: usize) -> Result<(), EngineError> {
    if failure_action_for(state, &state.steps[index])? != FailureAction::Propagate {
        let step = &mut state.steps[index];
        step.status = StepStatus::Waiting;
        step.recovery_pending = true;
    }
    Ok(())
}
/// A task deadline elapsed (F4). From engine version 2, a deadline that
/// ends a running attempt (ready or claimed) is recorded as a failed
/// attempt and follows `retry` and `on_failure`. A deadline reached while
/// no attempt runs (an unresolved handoff) is final, as in older runs.
pub(crate) fn time_out(
    state: &mut ExecutionState,
    index: usize,
    at_ms: i64,
) -> Result<(), EngineError> {
    let step = &mut state.steps[index];
    let running = matches!(step.status, StepStatus::Ready | StepStatus::Claimed);
    step.status = StepStatus::TimedOut;
    step.finished_at_ms = Some(at_ms);
    if state.engine_version < crate::version::TIMEOUT_IS_FAILURE || !running {
        return Ok(());
    }
    step.attempt_history.push(std::rc::Rc::new(AttemptRecord {
        attempt: step.attempt,
        actor: step.actor.clone().unwrap_or(ActorRef::System),
        at_ms,
        report: WorkReport {
            appended_logs: false,
            status: ReportStatus::Failed,
            verdict: None,
            outputs: Default::default(),
            summary: None,
            evidence: vec![],
            files: vec![],
            logs: vec![],
        },
        timed_out: true,
    }));
    recover(state, index)
}
pub(crate) fn schedule(
    state: &mut ExecutionState,
    index: usize,
    attempt: u32,
    due_at_ms: i64,
    at_ms: i64,
) -> Result<(), EngineError> {
    let step = &state.steps[index];
    if step.status != StepStatus::Waiting
        || !step.recovery_pending
        || step.attempt != attempt
        || failure_action_for(state, step)? != FailureAction::RetryAt(due_at_ms)
    {
        return Err(EngineError::new("retry.invalid_schedule"));
    }
    let step = &mut state.steps[index];
    step.recovery_pending = false;
    if due_at_ms <= at_ms {
        ready_next(step)?;
        rearm(state, index, at_ms)?;
    } else {
        step.retry_timer = Some(RetryTimer {
            generation: attempt,
            due_at_ms,
        });
        // The spent attempt's deadline must not end the backoff wait; the
        // next attempt gets its own when the timer fires (F4).
        if state.engine_version >= crate::version::TIMEOUT_IS_FAILURE {
            state.steps[index].deadline_at_ms = None;
        }
    }
    Ok(())
}
pub(crate) fn fire_timer(
    state: &mut ExecutionState,
    index: usize,
    generation: u32,
    at_ms: i64,
) -> Result<(), EngineError> {
    let step = &mut state.steps[index];
    let timer = step
        .retry_timer
        .as_ref()
        .ok_or(EngineError::new("timer.stale_generation"))?;
    if step.status != StepStatus::Waiting
        || timer.generation != generation
        || step.attempt != generation
    {
        return Err(EngineError::new("timer.stale_generation"));
    }
    if at_ms < timer.due_at_ms {
        return Err(EngineError::new("timer.not_due"));
    }
    ready_next(step)?;
    rearm(state, index, at_ms)
}
