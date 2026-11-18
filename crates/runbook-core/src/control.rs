//! Execution control (B6a): pause/resume/cancel over existing engine
//! events, signals through advance_wait, and clock advancement. Route
//! authorization (D27) sits above these operations in B6b.
use crate::{
    actor::{ActorError, ActorPrincipal, ActorWork},
    session::SessionStore,
    start::StartStore,
};
use runbook_engine::{Command, Event, WaitInput, advance_time, advance_wait};
use runbook_model::{ActorRef, ExecutionStatus, SignalRequest};

/// Validate and append metadata without committing, for combination with a control action.
pub fn update_metadata<S: SessionStore>(
    work: &mut ActorWork<S>,
    principal: &ActorPrincipal,
    title: Option<&str>,
    tags: Option<&[String]>,
    fields_patch: Option<&runbook_model::Data>,
) -> Result<(), ActorError> {
    if work.runtime.state.status == ExecutionStatus::Queued {
        return Err(ActorError::rejected("execution.not_started"));
    }
    if matches!(
        work.runtime.state.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    ) {
        return Err(ActorError::rejected("execution.closed"));
    }
    let state = &work.runtime.state;
    let patch = fields_patch.cloned().unwrap_or_default();
    crate::search::validate_field_update(&state.compiled().ir().fields, &state.fields, &patch)
        .map_err(|diagnostics| ActorError::Rejected {
            code: "data.invalid_fields",
            errors: diagnostics
                .into_iter()
                .map(|d| runbook_model::FieldError {
                    path: d.path,
                    code: d.code,
                    message: d.message,
                })
                .collect(),
        })?;
    let fields = patch
        .into_iter()
        .filter(|(k, v)| state.fields.get(k) != Some(v))
        .collect::<runbook_model::Data>();
    let title = title.filter(|t| *t != state.title).map(str::to_owned);
    let tags = tags.filter(|t| *t != state.tags).map(<[String]>::to_vec);
    if title.is_some() || tags.is_some() || !fields.is_empty() {
        work.append(
            Event::ExecutionMetadataUpdated {
                title,
                tags,
                fields,
            },
            principal.actor.clone(),
        )?;
    }
    Ok(())
}

fn prepare_pause<S: SessionStore>(
    work: &mut ActorWork<S>,
    principal: &ActorPrincipal,
    paused: bool,
) -> Result<(), ActorError> {
    if work.runtime.state.status == ExecutionStatus::Queued {
        return Err(ActorError::rejected("execution.not_started"));
    }
    let manual = work
        .runtime
        .state
        .pause_reasons
        .contains(&runbook_engine::PauseReason::Manual);
    if manual != paused {
        work.append(
            Event::ManualPauseChanged { paused },
            principal.actor.clone(),
        )?;
    }
    Ok(())
}
fn prepare_cancel<S: SessionStore>(
    work: &mut ActorWork<S>,
    principal: &ActorPrincipal,
) -> Result<(), ActorError> {
    prepare_cancel_as(work, principal.actor.clone(), None)
}
/// Append the cancellation a person's cancel would, recorded as `actor`
/// (LANG3 `cancel_previous` records the system and the replacing run and
/// its starter as `superseded`). Nothing is appended for a run that is
/// closed or already cancelling.
pub(crate) fn prepare_cancel_as<S: SessionStore>(
    work: &mut ActorWork<S>,
    actor: ActorRef,
    superseded: Option<&runbook_engine::Supersession>,
) -> Result<(), ActorError> {
    let superseded_by = superseded.map(|s| s.execution_id.clone());
    let started_by = superseded.map(|s| s.started_by.clone());
    if work.runtime.state.status == ExecutionStatus::Queued {
        let Event::ExecutionStarted { data, .. } =
            runbook_engine::prepare_start(&work.runtime.state, &work.record.context, work.at_ms())?
        else {
            return Err(ActorError::rejected("execution.invalid_start"));
        };
        work.append(
            Event::ExecutionCancelledBeforeStart {
                data,
                superseded_by,
                started_by,
            },
            actor,
        )?;
    } else if matches!(
        work.runtime.state.status,
        ExecutionStatus::Running | ExecutionStatus::Paused
    ) && work.runtime.state.pending_terminal_cause
        != Some(runbook_engine::TerminalCause::Cancelled)
    {
        work.append(
            Event::CancellationRequested {
                superseded_by,
                started_by,
            },
            actor,
        )?;
    }
    Ok(())
}
async fn commit_control<S: SessionStore>(
    mut work: ActorWork<S>,
    replay: Option<&crate::actor::Replay>,
    operation: &str,
) -> Result<(), ActorError> {
    if !work.has_pending() && replay.is_none() {
        return Ok(());
    }
    work.settle()?;
    match work.commit_replay(vec![], replay, operation, &()).await? {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => Err(ActorError::rejected("execution.conflict")),
    }
}

pub async fn set_paused<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    paused: bool,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    if let Some(saved) = work.replayed::<()>(principal, replay, "set_paused").await? {
        return Ok(saved);
    }
    prepare_pause(&mut work, principal, paused)?;
    commit_control(work, replay, "set_paused").await
}
pub async fn cancel<S: StartStore + SessionStore>(
    _store: &S,
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    if let Some(saved) = work.replayed::<()>(principal, replay, "cancel").await? {
        return Ok(saved);
    }
    prepare_cancel(&mut work, principal)?;
    commit_control(work, replay, "cancel").await
}
/// D27/D39: one PATCH may combine metadata and one action, all in one commit.
pub async fn patch<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    request: &runbook_model::ExecutionPatchRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    use runbook_model::ExecutionAction;
    if let Some(saved) = work.replayed::<()>(principal, replay, "patch").await? {
        return Ok(saved);
    }
    let metadata = request.title.is_some() || request.tags.is_some() || request.fields.is_some();
    if work.runtime.state.status == ExecutionStatus::Queued
        && (metadata || request.action != Some(ExecutionAction::Cancel))
    {
        return Err(ActorError::rejected("execution.not_started"));
    }
    if metadata {
        update_metadata(
            &mut work,
            principal,
            request.title.as_deref(),
            request.tags.as_deref(),
            request.fields.as_ref(),
        )?;
    }
    match request.action {
        Some(ExecutionAction::Pause) => prepare_pause(&mut work, principal, true)?,
        Some(ExecutionAction::Resume) => prepare_pause(&mut work, principal, false)?,
        Some(ExecutionAction::Cancel) => prepare_cancel(&mut work, principal)?,
        None => (),
    }
    commit_control(work, replay, "patch").await
}

pub async fn signal<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    name: &str,
    signal_id: &str,
    request: &SignalRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    let operation = "signal".to_owned();
    if let Some(saved) = work.replayed::<()>(principal, replay, &operation).await? {
        return Ok(saved);
    }
    let input = WaitInput::Signal {
        execution_id: work.runtime.state.id.clone(),
        signal_id: signal_id.to_owned(),
        name: name.to_owned(),
        payload: request.payload.clone(),
        at_ms: work.at_ms(),
    };
    let commands = advance_wait(&work.runtime.state, &input)?;
    if commands.is_empty() {
        if replay.is_some()
            && work.commit_replay(vec![], replay, &operation, &()).await?
                == crate::CommitResult::Conflict
        {
            return Err(ActorError::rejected("execution.conflict"));
        }
        return Ok(());
    }
    for command in commands {
        let Command::ConsumeSignal { signal } = command else {
            continue;
        };
        work.append(Event::SignalReceived { signal }, principal.actor.clone())?;
    }
    work.settle()?;
    match work.commit_replay(vec![], replay, &operation, &()).await? {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => Err(ActorError::rejected("execution.conflict")),
    }
}

/// Advance due timers and deadlines to the session clock, settling after.
pub async fn tick<S: SessionStore>(mut work: ActorWork<S>) -> Result<(), ActorError> {
    for _ in 0..1000 {
        let commands = advance_time(&work.runtime.state, work.at_ms())?;
        if commands.is_empty() {
            break;
        }
        for command in commands {
            work.runtime.realize(command)?;
        }
        work.settle()?;
    }
    if work.has_pending() {
        match work.commit(vec![]).await? {
            crate::CommitResult::Committed(_) => Ok(()),
            crate::CommitResult::Conflict => Err(ActorError::rejected("execution.conflict")),
        }
    } else {
        Ok(())
    }
}

pub fn system() -> ActorRef {
    ActorRef::System
}
