//! Bounded cleanup keeps the terminal cause separate from cleanup progress.
use crate::{Command, EngineError, ExecutionState, StepState, TerminalCause};
use runbook_model::{ExecutionStatus, StepRunId, StepStatus};
use runbook_spec::ir::{Node, NodeKind};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionPhase {
    #[default]
    Ordinary,
    Cleanup,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionLimits {
    pub max_step_runs: Option<u64>,
    pub hard_deadline_at_ms: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleanupRoot {
    Group(StepRunId),
    Top,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupState {
    pub deadline_at_ms: i64,
    pub roots: Vec<CleanupRoot>,
    pub current: usize,
    pub suppressed: Option<String>,
}
pub(crate) fn body<'a>(
    state: &'a ExecutionState,
    root: &CleanupRoot,
) -> Result<(&'a [Node], &'a [crate::IterationRef]), EngineError> {
    match root {
        CleanupRoot::Top => Ok((&state.compiled().ir().finally, &[])),
        CleanupRoot::Group(id) => {
            let step = &state.steps[state.step_index(id)?];
            let NodeKind::Group(group) = &state
                .node(&step.step_id)
                .ok_or(EngineError::invariant("cleanup group is absent"))?
                .kind
            else {
                return Err(EngineError::new("step.not_group"));
            };
            Ok((&group.finally, &step.scope))
        }
    }
}
pub(crate) fn contains(
    state: &ExecutionState,
    root: &CleanupRoot,
    step: &StepState,
) -> Result<bool, EngineError> {
    let (nodes, scope) = body(state, root)?;
    Ok(step.scope.starts_with(scope) && crate::tree::find(nodes, &step.node_id).is_some())
}
fn roots(state: &ExecutionState) -> Result<Vec<CleanupRoot>, EngineError> {
    let mut roots = vec![];
    for step in state.steps.iter().rev() {
        if step.id.is_some()
            && !step.group_finally_done
            && !matches!(
                step.status,
                StepStatus::Succeeded
                    | StepStatus::Failed
                    | StepStatus::Skipped
                    | StepStatus::TimedOut
                    | StepStatus::Expired
            )
            && matches!(state.node(&step.step_id).map(|n|&n.kind),Some(NodeKind::Group(g)) if !g.finally.is_empty())
        {
            roots.push(CleanupRoot::Group(
                step.id
                    .clone()
                    .ok_or(EngineError::invariant("cleanup group has no ID"))?,
            ));
        }
    }
    // A group already executing inside another finally remains that root's work.
    let mut selected = vec![];
    for root in &roots {
        let CleanupRoot::Group(id) = root else {
            continue;
        };
        let step = &state.steps[state.step_index(id)?];
        if !roots
            .iter()
            .filter(|other| *other != root)
            .map(|other| contains(state, other, step))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .any(|inside| inside)
        {
            selected.push(root.clone());
        }
    }
    if !state.compiled().ir().finally.is_empty() {
        selected.push(CleanupRoot::Top)
    }
    Ok(selected)
}
pub fn begin_cleanup(
    state: &ExecutionState,
    cause: TerminalCause,
    _at_ms: i64,
) -> Result<Vec<Command>, EngineError> {
    if state.phase != ExecutionPhase::Ordinary || !crate::deadline::open(state) {
        return Err(EngineError::new("cleanup.already_started"));
    }
    Ok(vec![Command::BeginCleanup {
        cause,
        reason: None,
    }])
}
pub(crate) fn enter(
    state: &mut ExecutionState,
    cause: TerminalCause,
    reason: Option<String>,
    at_ms: i64,
    seq: u64,
) -> Result<(), EngineError> {
    if state.phase != ExecutionPhase::Ordinary || !crate::deadline::open(state) {
        return Err(EngineError::new("cleanup.already_started"));
    }
    let roots = roots(state)?;
    let mut deadline = crate::deadline::add(
        at_ms,
        state.compiled().ir().execution.cleanup_timeout.millis,
    )?;
    if let Some(hard) = state.limits.hard_deadline_at_ms {
        deadline = deadline.min(hard);
    }
    let preserved = state
        .steps
        .iter()
        .map(|step| {
            roots
                .iter()
                .map(|root| contains(state, root, step))
                .collect::<Result<Vec<_>, _>>()
                .map(|v| v.into_iter().any(|inside| inside))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (index, keep) in preserved.into_iter().enumerate() {
        if !keep
            && state.steps[index].id.is_some()
            && !crate::tree::terminal(state.steps[index].status)
        {
            state.steps[index].status = StepStatus::Cancelled;
            state.steps[index].finished_at_ms = Some(at_ms);
            crate::data::note_step_revision(state, index, seq);
        }
    }
    state.phase = ExecutionPhase::Cleanup;
    state.status = ExecutionStatus::Running;
    state.pause_reasons.clear();
    state.pending_terminal_cause = Some(cause);
    state.terminal_reason = reason;
    state.cleanup = Some(CleanupState {
        deadline_at_ms: deadline,
        roots,
        current: 0,
        suppressed: None,
    });
    Ok(())
}
/// Cancellation during cleanup (F13): cleanup keeps running, bounded by its
/// deadline, but a success-path cleanup now ends the run as cancelled. A
/// failed or already cancelled run keeps its cause.
pub(crate) fn cancel_during(state: &mut ExecutionState) {
    if state.pending_terminal_cause == Some(TerminalCause::Succeeded) {
        state.pending_terminal_cause = Some(TerminalCause::Cancelled);
        state.terminal_reason = Some("cancelled".into());
    }
}
pub(crate) fn active_root(state: &ExecutionState) -> Result<Option<&CleanupRoot>, EngineError> {
    if state.phase != ExecutionPhase::Cleanup {
        return Ok(None);
    }
    let cleanup = state
        .cleanup
        .as_ref()
        .ok_or(EngineError::invariant("cleanup phase has no state"))?;
    Ok(cleanup.roots.get(cleanup.current))
}
pub(crate) fn frontier(state: &ExecutionState) -> Result<Vec<&StepState>, EngineError> {
    let Some(root) = active_root(state)? else {
        return Ok(vec![]);
    };
    let (nodes, scope) = body(state, root)?;
    if crate::tree::has_failure(state, nodes, scope)? {
        return Ok(vec![]);
    }
    crate::tree::sequence(state, nodes, scope)
}
pub(crate) fn status(state: &ExecutionState) -> Result<ExecutionStatus, EngineError> {
    Ok(
        match state
            .pending_terminal_cause
            .ok_or(EngineError::invariant("cleanup has no terminal cause"))?
        {
            TerminalCause::Succeeded => ExecutionStatus::Succeeded,
            TerminalCause::Failed => ExecutionStatus::Failed,
            TerminalCause::Cancelled => ExecutionStatus::Cancelled,
        },
    )
}
pub(crate) fn advance(state: &ExecutionState) -> Result<Vec<Command>, EngineError> {
    if let Some(root) = cancelled_root(state)? {
        return advance_cancelled(state, &root);
    }
    let Some(root) = active_root(state)? else {
        let status = status(state)?;
        let outputs = crate::data::evaluate_closing_outputs(state, status)?;
        if status == ExecutionStatus::Succeeded && crate::data::outputs_too_large(state, &outputs) {
            // CF13: past the budget after cleanup, the run fails instead.
            return Ok(vec![Command::SuppressCleanup {
                reason: crate::OUTPUTS_TOO_LARGE.into(),
            }]);
        }
        return Ok(vec![Command::CompleteExecution { status, outputs }]);
    };
    let children = frontier(state)?;
    if children.is_empty() {
        return Ok(vec![Command::CompleteCleanupRoot { root: root.clone() }]);
    }
    crate::decider::advance_nodes(state, &children)
}
pub(crate) fn finish_root(
    state: &mut ExecutionState,
    root: &CleanupRoot,
) -> Result<(), EngineError> {
    if cancelled_root(state)?.as_ref() == Some(root) {
        if !root_frontier(state, root)?.is_empty() {
            return Err(EngineError::new("cleanup.root_not_complete"));
        }
        let CleanupRoot::Group(id) = root else {
            return Err(EngineError::invariant("cancelled root is not a group"));
        };
        let index = state.step_index(id)?;
        state.steps[index].group_finally_done = true;
        return Ok(());
    }
    if active_root(state)? != Some(root) || !frontier(state)?.is_empty() {
        return Err(EngineError::new("cleanup.root_not_complete"));
    }
    let (nodes, scope) = body(state, root)?;
    let failed = crate::tree::has_failure(state, nodes, scope)?;
    if failed && state.pending_terminal_cause == Some(TerminalCause::Succeeded) {
        state.pending_terminal_cause = Some(TerminalCause::Failed);
    }
    if let CleanupRoot::Group(id) = root {
        let index = state.step_index(id)?;
        state.steps[index].group_finally_done = true;
    }
    state
        .cleanup
        .as_mut()
        .ok_or(EngineError::invariant("cleanup state disappeared"))?
        .current += 1;
    Ok(())
}
pub(crate) fn suppress(
    state: &mut ExecutionState,
    reason: &str,
    at_ms: i64,
    seq: u64,
) -> Result<(), EngineError> {
    if state.phase != ExecutionPhase::Cleanup || !crate::deadline::open(state) {
        return Err(EngineError::new("cleanup.not_active"));
    }
    let deadline = state
        .cleanup
        .as_ref()
        .ok_or(EngineError::invariant("cleanup state is absent"))?
        .deadline_at_ms;
    let valid = match reason {
        "cleanup.timeout" => at_ms >= deadline,
        "limit.execution_duration" => state
            .limits
            .hard_deadline_at_ms
            .is_some_and(|hard| at_ms >= hard && hard <= deadline),
        "limit.step_runs_exceeded" => at_cap(state),
        crate::QUIESCE_FAULT => true,
        // CF13: cleanup is done and the run would succeed past the budget.
        crate::OUTPUTS_TOO_LARGE => {
            active_root(state)?.is_none()
                && state.pending_terminal_cause == Some(TerminalCause::Succeeded)
                && crate::data::succeeding_outputs_too_large(state)?
        }
        _ => false,
    };
    if !valid {
        return Err(EngineError::new("cleanup.invalid_suppression"));
    }
    end(state, reason, at_ms, seq)
}
/// Suppress the active cleanup for `reason` (checked by the caller): every
/// open step is cancelled and the next decision closes the run.
fn end(state: &mut ExecutionState, reason: &str, at_ms: i64, seq: u64) -> Result<(), EngineError> {
    let cleanup = state
        .cleanup
        .as_mut()
        .ok_or(EngineError::invariant("cleanup state is absent"))?;
    if cleanup.suppressed.is_some() {
        return Err(EngineError::new("cleanup.already_suppressed"));
    }
    cleanup.suppressed = Some(reason.into());
    cleanup.current = cleanup.roots.len();
    let flipped = state.pending_terminal_cause == Some(TerminalCause::Succeeded);
    if flipped {
        state.pending_terminal_cause = Some(TerminalCause::Failed);
    }
    if reason == crate::OUTPUTS_TOO_LARGE {
        state.terminal_reason = Some(reason.into());
    }
    if reason == crate::RUN_SIZE_EXCEEDED
        && state.pending_terminal_cause != Some(TerminalCause::Cancelled)
    {
        // A failing run keeps a reason it recorded (an `end` step's); one
        // failing from a plain step failure recorded none and gets this
        // one, as does a succeeding run, which now fails for its size.
        if state.terminal_reason.is_none() || flipped {
            state.terminal_reason = Some(reason.into());
        }
    }
    // Suppression ends cleanup: no pause (manual or an expired cleanup
    // approval) may keep the run from closing, as `enter` also clears them.
    state.pause_reasons.clear();
    state.status = ExecutionStatus::Running;
    for index in 0..state.steps.len() {
        if state.steps[index].id.is_some() && !crate::tree::terminal(state.steps[index].status) {
            state.steps[index].status = StepStatus::Cancelled;
            state.steps[index].finished_at_ms = Some(at_ms);
            crate::data::note_step_revision(state, index, seq);
        }
    }
    Ok(())
}
/// Whether a host must hold `state`'s event log to the hard ceiling and may
/// record [`crate::RUN_SIZE_EXCEEDED`] now (engine version 6, CF13b): the
/// run is open, not yet faulted for its size, and no automatic decision is
/// pending (the reducer accepts nothing else then).
pub fn run_ceiling_applies(state: &ExecutionState) -> bool {
    state.engine_version >= crate::version::RUN_CEILING
        && crate::deadline::open(state)
        && !state
            .runtime_errors
            .iter()
            .any(|fault| fault.code == crate::RUN_SIZE_EXCEEDED)
        && !state
            .steps
            .iter()
            .any(|s| s.approval.as_ref().is_some_and(|a| a.automatic_pending))
}
/// Apply a recorded [`crate::RUN_SIZE_EXCEEDED`] fault: every open step is
/// cancelled, cleanup is entered (cause failed, unless the run is already
/// cleaning up) and suppressed at once, so the next decision closes the run
/// with its closing outputs (each held to the outputs budget). Nothing it
/// records can grow the run again.
pub(crate) fn exceed_run_size(
    state: &mut ExecutionState,
    at_ms: i64,
    seq: u64,
) -> Result<(), EngineError> {
    if state.phase == ExecutionPhase::Ordinary {
        enter(
            state,
            TerminalCause::Failed,
            Some(crate::RUN_SIZE_EXCEEDED.into()),
            at_ms,
            seq,
        )?;
    }
    let suppressed = state
        .cleanup
        .as_ref()
        .ok_or(EngineError::invariant("cleanup state is absent"))?
        .suppressed
        .is_some();
    if !suppressed {
        end(state, crate::RUN_SIZE_EXCEEDED, at_ms, seq)?;
    }
    Ok(())
}
pub(crate) fn needed(state: &ExecutionState) -> Result<bool, EngineError> {
    Ok(!roots(state)?.is_empty())
}
pub(crate) fn is_cleanup_step(
    state: &ExecutionState,
    step: &StepState,
) -> Result<bool, EngineError> {
    let cleanup = state
        .cleanup
        .as_ref()
        .ok_or(EngineError::invariant("cleanup state is absent"))?;
    for root in &cleanup.roots {
        if contains(state, root, step)? {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(crate) fn validate_begin(
    state: &ExecutionState,
    cause: TerminalCause,
    reason: &Option<String>,
) -> Result<(), EngineError> {
    if let Some(reason) = reason {
        // CF13: a version 5 run that would succeed past the outputs budget
        // fails instead.
        if reason == crate::OUTPUTS_TOO_LARGE
            && cause == TerminalCause::Failed
            && crate::tree::frontier(state)?.is_empty()
            && !needed(state)?
            && crate::data::succeeding_outputs_too_large(state)?
        {
            return Ok(());
        }
        return Err(EngineError::new("cleanup.invalid_cause"));
    }
    if cause == TerminalCause::Cancelled {
        return Ok(());
    }
    let frontier = crate::tree::frontier(state)?;
    let expected = if frontier.is_empty() {
        Some(TerminalCause::Succeeded)
    } else if (frontier.len() == 1 && crate::tree::failed(frontier[0].status))
        || crate::failure::terminal_cleanup_required(state)?
    {
        Some(TerminalCause::Failed)
    } else {
        None
    };
    if Some(cause) != expected {
        return Err(EngineError::new("cleanup.invalid_cause"));
    }
    Ok(())
}
pub(crate) fn counted(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Task(_) | NodeKind::Approval(_) | NodeKind::Wait(_)
    )
}
pub(crate) fn at_cap(state: &ExecutionState) -> bool {
    state
        .limits
        .max_step_runs
        .is_some_and(|limit| state.materialized_steps >= limit)
}

/// Cancelled sibling groups unwind before their parent can recover or continue.
pub(crate) fn cancelled_root(state: &ExecutionState) -> Result<Option<CleanupRoot>, EngineError> {
    for step in state.steps.iter().rev() {
        if step.status != StepStatus::Cancelled
            || step.group_finally_done
            || !matches!(state.node(&step.step_id).map(|n| &n.kind), Some(NodeKind::Group(g)) if !g.finally.is_empty())
        {
            continue;
        }
        let root = CleanupRoot::Group(
            step.id
                .clone()
                .ok_or(EngineError::invariant("cancelled group has no ID"))?,
        );
        if state.phase == ExecutionPhase::Cleanup {
            let cleanup = state
                .cleanup
                .as_ref()
                .ok_or(EngineError::invariant("cleanup state absent"))?;
            if cleanup.roots.contains(&root) {
                continue;
            }
            let Some(active) = active_root(state)? else {
                continue;
            };
            if !contains(state, active, step)? {
                continue;
            }
        }
        return Ok(Some(root));
    }
    Ok(None)
}
pub(crate) fn root_frontier<'a>(
    state: &'a ExecutionState,
    root: &CleanupRoot,
) -> Result<Vec<&'a StepState>, EngineError> {
    let (nodes, scope) = body(state, root)?;
    if crate::tree::has_failure(state, nodes, scope)? {
        return Ok(vec![]);
    }
    crate::tree::sequence(state, nodes, scope)
}
pub(crate) fn advance_cancelled(
    state: &ExecutionState,
    root: &CleanupRoot,
) -> Result<Vec<Command>, EngineError> {
    let children = root_frontier(state, root)?;
    if children.is_empty() {
        return Ok(vec![Command::CompleteCleanupRoot { root: root.clone() }]);
    }
    crate::decider::advance_nodes(state, &children)
}
