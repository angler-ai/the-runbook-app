//! B8a: the resume brief. A fresh actor session starts from it, not from
//! the previous session (PRD §8): completed work with outputs and summaries
//! (labelled as untrusted data), approval decisions and comments, references
//! to work that is ready now, and the next action. Future step instructions
//! are never included; each is disclosed when its step is claimed.
use crate::execution_document::{decisions_of, path_of};
use runbook_engine::ExecutionState;
use runbook_model::{
    ActorKind, ActorRef, CommentView, CompletedStepSummary, ExecutionStatus, NextAction, ReadyWork,
    ResumeBrief, StepStatus,
};
use runbook_spec::ir::{Actor, NodeKind};
use std::num::NonZeroU32;

const SCHEMA_REVISION: NonZeroU32 = NonZeroU32::MIN;

fn finished(status: StepStatus) -> bool {
    matches!(
        status,
        StepStatus::Succeeded
            | StepStatus::Failed
            | StepStatus::Skipped
            | StepStatus::Cancelled
            | StepStatus::TimedOut
            | StepStatus::Expired
    )
}

/// Build the brief for `audience`: an agent sees ready agent and `any`
/// work, a human sees ready human and `any` work. Without an audience every
/// ready task is listed.
pub fn resume_brief(state: &ExecutionState, audience: Option<ActorKind>) -> ResumeBrief {
    let mut completed_steps = Vec::new();
    let mut decisions = Vec::new();
    let mut comments = Vec::new();
    let mut ready_work = Vec::new();
    for step in &state.steps {
        let Some(id) = step.id.clone() else {
            continue;
        };
        let node = state.node(&step.step_id);
        if let Some(approval) = &step.approval {
            decisions.extend(decisions_of(approval));
            for signature in &approval.votes {
                if let (ActorRef::Human { user_id }, Some(body)) =
                    (&signature.vote.actor, &signature.vote.comment)
                {
                    comments.push(CommentView {
                        author: user_id.clone(),
                        body: body.clone(),
                    });
                }
            }
        }
        if finished(step.status) {
            completed_steps.push(CompletedStepSummary {
                step_run_id: id,
                path: path_of(step),
                status: step.status,
                verdict: step.verdict,
                outputs: step.outputs.clone(),
                summary: step.summary.clone(),
                amended: step.amended,
                stale: step.stale,
            });
            continue;
        }
        // A paused run offers no work; actors wait for resume.
        if state.status == ExecutionStatus::Paused {
            continue;
        }
        let actor_kind = match node.map(|node| &node.kind) {
            // A handed-off task continues with a person (PRD §6 handoff).
            Some(NodeKind::Task(_))
                if step.status == StepStatus::Ready && step.human_continuation =>
            {
                ActorKind::Human
            }
            Some(NodeKind::Task(task)) if step.status == StepStatus::Ready => {
                match (task.actor, audience) {
                    (Actor::Human, _) => ActorKind::Human,
                    (Actor::Agent, _) => ActorKind::Agent,
                    // LANG7: a strict `actor: any` task belongs to its
                    // assignee, a person; agents may not claim it.
                    (Actor::Any, _) if !runbook_engine::agents_may_claim(task) => ActorKind::Human,
                    (Actor::Any, Some(kind)) => kind,
                    (Actor::Any, None) => ActorKind::Agent,
                }
            }
            // An open approval that allows agents is agent work (D26); human
            // approvals reach people through the inbox, not the brief.
            Some(NodeKind::Approval(definition))
                if definition.allow_agents
                    && step.status == StepStatus::Waiting
                    && step
                        .approval
                        .as_ref()
                        .is_some_and(|approval| approval.decision.is_none()) =>
            {
                ActorKind::Agent
            }
            _ => continue,
        };
        if audience.is_some_and(|kind| kind != actor_kind) {
            continue;
        }
        ready_work.push(ReadyWork {
            step_run_id: id,
            path: path_of(step),
            title: runbook_engine::step_title(state, step).unwrap_or_else(|| step.step_id.clone()),
            actor_kind,
            agent_id: None,
            group_id: None,
        });
    }
    let terminal = matches!(
        state.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    );
    let next_action = if terminal {
        NextAction::Exit
    } else if !ready_work.is_empty() {
        NextAction::ClaimReadyWork
    } else {
        NextAction::Wait
    };
    ResumeBrief {
        schema_revision: SCHEMA_REVISION,
        execution_id: state.id.clone(),
        completed_steps,
        decisions,
        comments,
        ready_work,
        next_action,
    }
}
