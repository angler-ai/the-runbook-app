use super::*;

/// Release a claim without impersonating its owner. Route scope checks remain
/// mandatory; the current in-session directory also proves human project admin.
pub async fn admin_release<S: SessionStore>(
    mut work: ActorWork<S>,
    admin: &ActorPrincipal,
    step_run_id: &StepRunId,
    reason: &str,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    let user = admin
        .user_id()
        .ok_or(ActorError::rejected("auth.forbidden"))?;
    let directory = &work.runtime.directory;
    if !directory.users.get(user).is_some_and(|u| u.active)
        || directory.project_role(&work.runtime.scope.project_id, user)
            != Some(runbook_model::ProjectRole::Admin)
    {
        return Err(ActorError::rejected("auth.forbidden"));
    }
    let operation = format!("admin_release:{step_run_id}");
    if let Some(saved) = work.replayed::<()>(admin, replay, &operation).await? {
        return Ok(saved);
    }
    if reason.trim().is_empty() {
        return Err(ActorError::rejected("claim.release_reason_required"));
    }
    let current = work
        .snapshot
        .claims
        .get(step_run_id)
        .cloned()
        .ok_or(ActorError::rejected("claim.not_found"))?;
    if current.released {
        return Err(ActorError::rejected("claim.stale_generation"));
    }
    let generation = current
        .generation
        .checked_add(1)
        .ok_or(ActorError::rejected("claim.generation_exhausted"))?;
    let index = work.step_index(step_run_id)?;
    if let Some(event) =
        end_decision_lease(&work.runtime.state.steps[index], &current, "admin_released")
    {
        work.append(event, ActorRef::System)?;
    } else {
        work.append(
            Event::StepClaimReleased {
                step_run_id: step_run_id.clone(),
                reason: Some(reason.to_owned()),
            },
            admin.actor.clone(),
        )?;
    }
    work.settle()?;
    let write = ClaimWrite {
        action: ClaimAction::AdminRelease,
        expected_generation: current.generation,
        generation,
        released: true,
        ..current
    };
    match work
        .commit_replay(vec![write], replay, &operation, &())
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => Err(ActorError::rejected("claim.conflict")),
    }
}
