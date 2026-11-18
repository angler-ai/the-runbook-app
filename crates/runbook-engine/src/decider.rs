use crate::{Command, EngineError, ExecutionState};
use runbook_model::{ExecutionStatus, StepStatus};
use runbook_spec::ir::NodeKind;
pub(crate) fn supported(state: &ExecutionState) -> Result<(), EngineError> {
    let mut nodes = vec![];
    crate::tree::collect(&state.compiled().ir().steps, &mut nodes);
    crate::tree::collect(&state.compiled().ir().finally, &mut nodes);
    if nodes.iter().any(|n| {
        !matches!(
            n.kind,
            NodeKind::Task(_)
                | NodeKind::End(_)
                | NodeKind::Wait(_)
                | NodeKind::Approval(_)
                | NodeKind::Group(_)
                | NodeKind::Set(_)
                | NodeKind::Switch(_)
                | NodeKind::Parallel { .. }
                | NodeKind::Foreach(_)
                | NodeKind::Loop(_)
                | NodeKind::Call(_)
        )
    }) {
        return Err(EngineError::new("engine.unsupported_node"));
    }
    Ok(())
}
/// Decisions depend on persisted facts. Conditions are only evaluated at their
/// frontier; subsequent calls follow the recorded branch or materialized step.
pub fn decide(state: &ExecutionState) -> Result<Vec<Command>, EngineError> {
    if !crate::deadline::open(state) {
        return Ok(vec![]);
    }
    supported(state)?;
    if let Some(command) = crate::automatic::pending(state, state.at_ms)? {
        return Ok(vec![command]);
    }
    let notifications = crate::handoff::pending_notifications(state);
    if !notifications.is_empty() {
        return Ok(notifications);
    }
    let notices = crate::deadline::pending_notifications(state);
    if !notices.is_empty() {
        return Ok(notices);
    }
    // LANG7: a ready strict task resolves its assignee before anything else
    // can act on it (claims check against the resolution). Not while paused:
    // the reducer accepts the resolution only while running, and claims are
    // refused on a paused run anyway; resume settles and resolves it.
    if state.status != ExecutionStatus::Paused
        && let Some(command) = crate::assignment::pending(state)?
    {
        return Ok(vec![command]);
    }
    let timers = crate::advance_time(state, state.at_ms)?;
    if !timers.is_empty() {
        return Ok(timers);
    }
    for step in &state.steps {
        if step.approval.is_some()
            && matches!(step.status, StepStatus::Waiting | StepStatus::Expired)
        {
            let decision = crate::approval::advance(state, step)?;
            if !decision.is_empty() {
                return Ok(decision);
            }
        }
    }
    if state.status == ExecutionStatus::Paused {
        return Ok(vec![]);
    }
    if let Some(command) = crate::work_notifications::pending_agent(state)? {
        return Ok(vec![command]);
    }
    if state.phase == crate::ExecutionPhase::Cleanup {
        return crate::cleanup::advance(state);
    }
    if let Some(root) = crate::cleanup::cancelled_root(state)? {
        return crate::cleanup::advance_cancelled(state, &root);
    }
    let nodes = crate::tree::frontier(state)?;
    if nodes.is_empty() {
        if crate::cleanup::needed(state)? {
            return crate::begin_cleanup(state, crate::TerminalCause::Succeeded, state.at_ms);
        }
        let outputs = crate::evaluate_outputs(state)?;
        if crate::data::outputs_too_large(state, &outputs) {
            // CF13: the run fails instead; its close nulls what does not fit.
            return Ok(vec![Command::BeginCleanup {
                cause: crate::TerminalCause::Failed,
                reason: Some(crate::OUTPUTS_TOO_LARGE.into()),
            }]);
        }
        return Ok(vec![Command::CompleteExecution {
            status: ExecutionStatus::Succeeded,
            outputs,
        }]);
    }
    advance_nodes(state, &nodes)
}
/// Materialize independent frontier nodes together. Computed mutations are
/// serialized so each is evaluated against the sequence it will commit on.
pub(crate) fn advance_nodes(
    state: &ExecutionState,
    nodes: &[&crate::StepState],
) -> Result<Vec<Command>, EngineError> {
    let pending: Vec<_> = nodes
        .iter()
        .filter(|s| s.status == StepStatus::Pending)
        .collect();
    if !pending.is_empty() {
        let mut commands = vec![];
        let mut reserved = state.materialized_steps;
        for node in pending {
            let definition = state
                .node(&node.step_id)
                .ok_or(EngineError::invariant("frontier node is absent"))?;
            if crate::cleanup::counted(definition) {
                if state
                    .limits
                    .max_step_runs
                    .is_some_and(|cap| reserved >= cap)
                {
                    if !commands.is_empty() {
                        break;
                    }
                    return Ok(vec![if state.phase == crate::ExecutionPhase::Cleanup {
                        Command::SuppressCleanup {
                            reason: "limit.step_runs_exceeded".into(),
                        }
                    } else {
                        Command::RecordStepLimit {
                            step_id: node.step_id.clone(),
                        }
                    }]);
                }
                reserved = reserved
                    .checked_add(1)
                    .ok_or(EngineError::invariant("step count overflow"))?;
            }
            if matches!(
                state.node(&node.step_id).map(|n| &n.kind),
                Some(NodeKind::Foreach(_))
            ) {
                if commands.is_empty() {
                    return advance_node(state, node);
                }
                break;
            }
            commands.extend(advance_node(state, node)?);
        }
        return Ok(commands);
    }
    for node in nodes {
        let commands = advance_node(state, node)?;
        if !commands.is_empty() {
            return Ok(commands);
        }
    }
    Ok(vec![])
}
fn advance_node(
    state: &ExecutionState,
    step: &crate::StepState,
) -> Result<Vec<Command>, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    match step.status {
        StepStatus::Pending => {
            let guard = crate::branch::guard(state, step)?;
            if guard.passes && matches!(node.kind, NodeKind::Foreach(_)) {
                return Ok(vec![Command::StartForeach {
                    step_id: step.step_id.clone(),
                    snapshot: match crate::foreach::prepare(state, step, guard.reads) {
                        Ok(snapshot) => Box::new(snapshot),
                        Err(error) => {
                            return Ok(vec![Command::RecordFault {
                                fault: crate::RuntimeFault {
                                    step_id: step.step_id.clone(),
                                    code: error.code().into(),
                                    step_run_id: None,
                                },
                            }]);
                        }
                    },
                }]);
            }
            Ok(vec![if guard.passes {
                Command::MakeStepReady {
                    step_id: step.step_id.clone(),
                    reads: guard.reads,
                }
            } else {
                Command::SkipStep {
                    step_id: step.step_id.clone(),
                    reason: crate::SkipReason::Condition,
                    reads: guard.reads,
                }
            }])
        }
        StepStatus::Ready => match &node.kind {
            NodeKind::End(_) => Ok(vec![Command::SelectEnd {
                selection: crate::end_node::prepare_end_for(state, step)?,
            }]),
            NodeKind::Wait(_) => crate::wait::prepare(state, step),
            NodeKind::Call(_) => crate::call::prepare(state, step),
            NodeKind::Task(_) => Ok(vec![]),
            NodeKind::Approval(_) => Err(EngineError::invariant("opened approval is not waiting")),
            NodeKind::Set(_) => Ok(vec![Command::ApplyData {
                change: crate::data::evaluate_assignments_for(state, step)?,
            }]),
            NodeKind::Switch(_) if step.branch.is_none() => Ok(vec![Command::SelectBranch {
                selection: crate::branch::choose_branch_for(state, step)?,
            }]),
            NodeKind::Group(_) => crate::failure::advance_group(state, step),
            NodeKind::Loop(_) => crate::loop_node::advance_for(state, step),
            NodeKind::Foreach(_) => crate::foreach::advance_for(state, step),
            NodeKind::Parallel { .. } => crate::parallel::advance_for(state, step),
            NodeKind::Switch(_) => {
                let (status, verdict) = crate::branch::completed_outcome(state, step)?;
                Ok(vec![Command::CompleteStep {
                    step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
                    status,
                    verdict,
                }])
            }
        },
        StepStatus::Failed | StepStatus::TimedOut | StepStatus::Expired => {
            if crate::cleanup::needed(state)? {
                return crate::begin_cleanup(state, crate::TerminalCause::Failed, state.at_ms);
            }
            Ok(vec![Command::CompleteExecution {
                status: ExecutionStatus::Failed,
                outputs: crate::data::evaluate_closing_outputs(state, ExecutionStatus::Failed)?,
            }])
        }
        StepStatus::Waiting if step.wait.is_some() => crate::wait::advance(state, step),
        StepStatus::Waiting if step.call.is_some() => Ok(vec![]),
        StepStatus::Waiting if step.approval.is_some() => crate::approval::advance(state, step),
        StepStatus::Waiting if step.recovery_pending => crate::retry::advance(state, step),
        StepStatus::Claimed | StepStatus::Waiting => Ok(vec![]),
        _ => Err(EngineError::new("engine.unsupported_status")),
    }
}
