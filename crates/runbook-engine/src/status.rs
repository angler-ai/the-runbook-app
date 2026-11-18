//! The execution status a run reports (R50).
//!
//! PRD §6: `waiting` means the run waits only on people or timers. The
//! reducer keeps `running` for every open, unpaused run (its rules key off
//! it); the reported status is derived from the same state, so replay is
//! unchanged. Runs recorded before engine version 3 report `running`.
use crate::{ExecutionState, StepState};
use runbook_model::{ActorRef, ExecutionStatus, StepStatus};
use runbook_spec::ir::NodeKind;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    /// An agent can act now: ready or agent-claimed agent work, or an
    /// approval agents may decide.
    Agent,
    /// Only a person or a timer can move it.
    PeopleOrTimers,
}

fn open_work(state: &ExecutionState, step: &StepState) -> Option<Open> {
    let node = state.node(&step.step_id)?;
    match (&node.kind, step.status) {
        (NodeKind::Task(_), StepStatus::Ready) if step.human_continuation => {
            Some(Open::PeopleOrTimers)
        }
        // LANG7: a strict `actor: any` task waits on its assignee, a person.
        (NodeKind::Task(task), StepStatus::Ready) => {
            Some(if crate::assignment::agents_may_claim(task) {
                Open::Agent
            } else {
                Open::PeopleOrTimers
            })
        }
        (NodeKind::Task(_), StepStatus::Claimed) => Some(match step.actor {
            Some(ActorRef::Human { .. }) => Open::PeopleOrTimers,
            _ => Open::Agent,
        }),
        // A retry backoff, or a handoff waiting for a person.
        (NodeKind::Task(_), StepStatus::Waiting) => Some(Open::PeopleOrTimers),
        (NodeKind::Approval(approval), StepStatus::Waiting) => Some(
            if approval.allow_agents && !step.approval.as_ref().is_some_and(|a| a.expired) {
                Open::Agent
            } else {
                Open::PeopleOrTimers
            },
        ),
        (NodeKind::Wait(_), StepStatus::Waiting) => Some(Open::PeopleOrTimers),
        // A loop's delay timer.
        (NodeKind::Loop(_), _) if step.loop_state.as_ref().is_some_and(|l| l.timer.is_some()) => {
            Some(Open::PeopleOrTimers)
        }
        _ => None,
    }
}

/// The status a run reports: `waiting` when it is open and unpaused, and
/// its only open work waits on people or timers; any agent-actionable work
/// keeps it `running`. A pause and terminal statuses are reported as is.
pub fn reported_status(state: &ExecutionState) -> ExecutionStatus {
    if state.status != ExecutionStatus::Running
        || state.engine_version < crate::version::WAITING_STATUS
    {
        return state.status;
    }
    let mut waiting = false;
    for step in &state.steps {
        match open_work(state, step) {
            Some(Open::Agent) => return ExecutionStatus::Running,
            Some(Open::PeopleOrTimers) => waiting = true,
            None => {}
        }
    }
    if waiting {
        ExecutionStatus::Waiting
    } else {
        ExecutionStatus::Running
    }
}
