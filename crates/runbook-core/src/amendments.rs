//! Step overrides (B9a/D35): a human project admin amends a finished
//! step's verdict, outputs and summary. The original record is preserved
//! on the step; already-evaluated consumers become stale, branches and
//! assigned variables never change, and a completed run does not reopen.
use crate::{
    actor::{ActorError, ActorPrincipal, ActorWork},
    session::SessionStore,
};
use runbook_model::{Amendment, OverrideRequest, ProjectRole, StepRunId};

pub async fn override_step<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    request: &OverrideRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<Amendment, ActorError> {
    let Some(user) = principal.user_id() else {
        return Err(ActorError::rejected("amendment.human_required"));
    };
    let admin = work
        .runtime
        .directory
        .project_role(&work.runtime.scope.project_id, user)
        == Some(ProjectRole::Admin);
    if !admin {
        return Err(ActorError::rejected("amendment.admin_required"));
    }
    // D20/D35: a retry replays the saved amendment after authorization,
    // before any state check, even though the step is already amended.
    let operation = format!("override:{step_run_id}");
    if let Some(saved) = work
        .replayed::<Amendment>(principal, replay, &operation)
        .await?
    {
        return Ok(saved);
    }
    let amendment = runbook_engine::validate_override(
        &work.runtime.state,
        step_run_id,
        &request.to,
        &request.reason,
        principal.actor.clone(),
    )?;
    work.append(
        runbook_engine::Event::StepOverridden {
            step_run_id: step_run_id.clone(),
            amendment: Box::new(amendment.clone()),
        },
        principal.actor.clone(),
    )?;
    work.settle()?;
    match work
        .commit_replay(vec![], replay, &operation, &amendment)
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(amendment),
        crate::CommitResult::Conflict => Err(ActorError::rejected("amendment.conflict")),
    }
}
