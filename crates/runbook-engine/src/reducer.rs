use crate::{EngineError, Event, EventEnvelope, ExecutionState};
use runbook_model::{ActorRef, ExecutionStatus, ReportStatus, StepStatus};

pub fn apply(
    state: &ExecutionState,
    envelope: &EventEnvelope,
) -> Result<ExecutionState, EngineError> {
    if envelope.execution_id != state.id {
        return Err(EngineError::new("event.wrong_execution"));
    }
    if state.sequence.checked_add(1) != Some(envelope.seq) {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let mut next = state.clone();
    if state
        .steps
        .iter()
        .any(|s| s.approval.as_ref().is_some_and(|a| a.automatic_pending))
        && !matches!(
            envelope.event,
            Event::DecisionApplied {
                decision: crate::AppliedDecision::Automatic(_)
            }
        )
    {
        return Err(EngineError::new("approval.automatic_pending"));
    }
    if matches!(
        envelope.event,
        Event::ExecutionStarted { .. }
            | Event::ApprovalRequested { .. }
            | Event::AgentWorkAvailable { .. }
            | Event::EndSelected { .. }
            | Event::StepLimitExceeded { .. }
            | Event::StepReady { .. }
            | Event::ExecutionCompleted { .. }
            | Event::DataChanged { .. }
            | Event::StepSkipped { .. }
            | Event::BranchSelected { .. }
            | Event::StepCompleted { .. }
            | Event::SiblingsCancelled { .. }
            | Event::ExecutionFaulted { .. }
            | Event::ForeachStarted { .. }
            | Event::IterationAdmitted { .. }
            | Event::LoopIterationStarted { .. }
            | Event::LoopChecked { .. }
            | Event::TimerFired { .. }
            | Event::CatchActivated { .. }
            | Event::GroupFinallyStarted { .. }
            | Event::CleanupStarted { .. }
            | Event::CleanupRootCompleted { .. }
            | Event::CleanupSuppressed { .. }
            | Event::DecisionApplied {
                decision: crate::AppliedDecision::Automatic(_)
            }
            | Event::ExecutionDeadlineElapsed { .. }
            | Event::TaskDeadlineElapsed { .. }
            | Event::ApprovalEscalated { .. }
            | Event::StepReminded { .. }
            | Event::StepEscalated { .. }
            | Event::ApprovalExpired { .. }
            | Event::ApprovalNotificationQueued { .. }
            | Event::AutomaticDecisionWithheld { .. }
            | Event::WaitStarted { .. }
            | Event::CallRequested { .. }
            | Event::CallStarted { .. }
            | Event::CallCompleted { .. }
            | Event::ApprovalDecided { .. }
            | Event::RetryScheduled { .. }
            | Event::StepHandedOff { .. }
            | Event::StepAssigneeResolved { .. }
            | Event::StepBlockedNotificationQueued { .. }
            | Event::AgentTargetResolved { .. }
            | Event::DecisionLeaseEnded { .. }
    ) && envelope.actor != ActorRef::System
    {
        return Err(EngineError::new("event.system_actor_required"));
    }
    if matches!(
        envelope.event,
        Event::StepReported { .. }
            | Event::ChecklistUpdated { .. }
            | Event::ApprovalVoted { .. }
            | Event::SignalReceived { .. }
    ) {
        crate::authorize_at_deadline(state, envelope.at_ms)?;
    }
    if let Event::StepReported { step_run_id, .. } | Event::ChecklistUpdated { step_run_id, .. } =
        &envelope.event
    {
        let index = state.step_index(step_run_id)?;
        if state.steps[index]
            .deadline_at_ms
            .is_some_and(|at| envelope.at_ms >= at)
        {
            return Err(EngineError::new("step.deadline_exceeded"));
        }
    }
    match &envelope.event {
        Event::ApprovalRequested { snapshot } => {
            crate::work_notifications::record_request(&mut next, snapshot)?
        }
        Event::AgentWorkAvailable { work } => {
            crate::work_notifications::record_agent(&mut next, work)?
        }
        Event::EndSelected { selection } => {
            crate::end_node::apply_end(&mut next, selection, envelope.at_ms, envelope.seq)?
        }
        Event::GroupFinallyStarted {
            group_run_id,
            outcome,
        } => {
            let index = crate::failure::begin_finally(&mut next, group_run_id, outcome)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::StepLimitExceeded { step_id } => {
            let record = state
                .steps
                .iter()
                .find(|s| s.step_id == *step_id)
                .ok_or(EngineError::invariant("limit step is absent"))?;
            let node = state
                .node(step_id)
                .ok_or(EngineError::invariant("limit node is absent"))?;
            if state.phase != crate::ExecutionPhase::Ordinary
                || record.status != StepStatus::Pending
                || !crate::cleanup::at_cap(state)
                || !crate::cleanup::counted(node)
                || !crate::tree::is_frontier(state, step_id)?
            {
                return Err(EngineError::new("limit.invalid_step_limit"));
            }
            crate::cleanup::enter(
                &mut next,
                crate::TerminalCause::Failed,
                Some("limit.step_runs_exceeded".into()),
                envelope.at_ms,
                envelope.seq,
            )?;
        }
        Event::CleanupStarted { cause, reason } => {
            crate::cleanup::validate_begin(state, *cause, reason)?;
            crate::cleanup::enter(
                &mut next,
                *cause,
                reason.clone(),
                envelope.at_ms,
                envelope.seq,
            )?;
        }
        Event::CleanupRootCompleted { root } => crate::cleanup::finish_root(&mut next, root)?,
        Event::CleanupSuppressed { reason } => {
            crate::cleanup::suppress(&mut next, reason, envelope.at_ms, envelope.seq)?
        }
        Event::ExecutionCancelledBeforeStart {
            data,
            superseded_by,
            started_by,
        } => {
            if state.sequence != 0 || state.status != ExecutionStatus::Queued {
                return Err(EngineError::new("execution.already_started"));
            }
            next.superseded = crate::state::supersession(superseded_by, started_by);
            crate::data::install_start(&mut next, data, envelope.seq, envelope.at_ms)?;
            next.started_at_ms = None;
            next.status = ExecutionStatus::Cancelled;
            next.pending_terminal_cause = Some(crate::TerminalCause::Cancelled);
            next.terminal_reason = Some("cancelled".into());
        }
        Event::ExecutionMetadataUpdated {
            title,
            tags,
            fields,
        } => {
            crate::data::apply_metadata(&mut next, title, tags, fields, envelope.seq)?;
        }
        Event::CancellationRequested {
            superseded_by,
            started_by,
        } => {
            if let Some(superseded) = crate::state::supersession(superseded_by, started_by) {
                next.superseded = Some(superseded);
            }
            if state.phase == crate::ExecutionPhase::Cleanup && crate::deadline::open(state) {
                crate::cleanup::cancel_during(&mut next);
            } else {
                crate::cleanup::enter(
                    &mut next,
                    crate::TerminalCause::Cancelled,
                    Some("cancelled".into()),
                    envelope.at_ms,
                    envelope.seq,
                )?
            }
        }
        Event::DecisionApplied { decision } => {
            let index = match decision {
                crate::AppliedDecision::Automatic(fact) => {
                    crate::automatic::apply(&mut next, fact, envelope.at_ms)?
                }
                crate::AppliedDecision::Agent(fact) => {
                    crate::agent_approval::apply(&mut next, fact, envelope.at_ms, &envelope.actor)?
                }
            };
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::ManualPauseChanged { paused } => {
            crate::deadline::manual_pause(&mut next, *paused)?;
        }
        Event::ApprovalNotificationQueued { notice } => {
            crate::deadline::queue_notice(&mut next, notice)?
        }
        Event::ApprovalEscalated { snapshot } => {
            let index = crate::deadline::escalate(&mut next, snapshot, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::StepReminded {
            step_run_id,
            due_at_ms,
        } => {
            let index =
                crate::followup::remind(&mut next, step_run_id, *due_at_ms, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::StepEscalated { snapshot } => {
            let index = crate::followup::escalate(&mut next, snapshot, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::ApprovalExpired {
            step_run_id,
            deadline_at_ms,
        } => {
            let index = crate::deadline::expire_approval(
                &mut next,
                step_run_id,
                *deadline_at_ms,
                envelope.at_ms,
            )?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::AutomaticDecisionWithheld {
            step_run_id,
            deadline_at_ms,
            reason,
        } => {
            let index = crate::automatic::withhold(
                &mut next,
                &crate::TimeoutPolicy {
                    step_run_id: step_run_id.clone(),
                    deadline_at_ms: *deadline_at_ms,
                    at_ms: envelope.at_ms,
                },
                reason,
            )?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::TaskDeadlineElapsed {
            step_run_id,
            deadline_at_ms,
        } => {
            let index = state.step_index(step_run_id)?;
            if !crate::deadline::open(state)
                || state.steps[index].deadline_at_ms != Some(*deadline_at_ms)
                || envelope.at_ms < *deadline_at_ms
                || crate::tree::terminal(state.steps[index].status)
                || !matches!(
                    state.node(&state.steps[index].step_id).map(|n| &n.kind),
                    Some(runbook_spec::ir::NodeKind::Task(_))
                )
            {
                return Err(EngineError::new("task.deadline_not_due"));
            }
            crate::retry::time_out(&mut next, index, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::ExecutionDeadlineElapsed { deadline_at_ms } => {
            if !crate::deadline::open(state)
                || state.execution_deadline_at_ms != Some(*deadline_at_ms)
                || envelope.at_ms < *deadline_at_ms
            {
                return Err(EngineError::new("execution.deadline_not_due"));
            }
            crate::cleanup::enter(
                &mut next,
                crate::TerminalCause::Failed,
                Some("timeout".into()),
                envelope.at_ms,
                envelope.seq,
            )?;
        }
        Event::SignalReceived { signal } => {
            for index in crate::wait::consume_signal(&mut next, signal, envelope.at_ms)? {
                crate::data::note_step_revision(&mut next, index, envelope.seq);
            }
        }
        Event::WaitStarted { start } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = crate::wait::start(&mut next, start, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::CallRequested { start } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = crate::call::request(&mut next, start, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::CallStarted {
            step_run_id,
            child_execution_id,
        } => {
            crate::call::started(&mut next, step_run_id, child_execution_id)?;
        }
        Event::CallCompleted { outcome } => {
            let index = crate::call::complete(&mut next, outcome, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::ApprovalDecided { decision } => {
            let index = crate::approval::record_decision(&mut next, decision, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::ApprovalVoted { vote } => {
            if envelope.actor != vote.actor {
                return Err(EngineError::new("approval.actor_mismatch"));
            }
            let index = crate::approval::record_vote(&mut next, vote, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::StepHandedOff { handoff } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = crate::handoff::apply(&mut next, handoff, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::StepAssigneeResolved { resolution } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = crate::assignment::apply(&mut next, resolution)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::StepTakenOver {
            step_run_id,
            previous_actor_kind,
        } => {
            if !crate::deadline::open(state) {
                return Err(EngineError::new("execution.not_running"));
            }
            crate::assignment::take_over(
                &mut next,
                step_run_id,
                *previous_actor_kind,
                &envelope.actor,
                envelope.at_ms,
            )?;
        }
        Event::StepBlockedNotificationQueued { notification } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(&notification.step_run_id)?;
            if state.steps[index].blocked_notification_queued
                || crate::handoff::notification(&state.steps[index]).as_ref() != Some(notification)
            {
                return Err(EngineError::new("notification.not_pending"));
            }
            next.steps[index].blocked_notification_queued = true;
        }
        Event::RetryScheduled {
            step_run_id,
            attempt,
            due_at_ms,
        } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(step_run_id)?;
            crate::retry::schedule(&mut next, index, *attempt, *due_at_ms, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::CatchActivated { selection } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = crate::failure::validate_selection(state, selection)?;
            next.steps[index].catch_source = Some(selection.failed_run_id.clone());
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::TimerFired {
            step_run_id,
            generation,
        } => {
            if !crate::deadline::open(state) {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(step_run_id)?;
            if state.steps[index].wait.is_some() {
                crate::wait::fire_timer(&mut next, index, *generation, envelope.at_ms)?;
            } else if state.steps[index].loop_state.is_some() {
                crate::loop_node::fire_timer(&mut next, index, *generation, envelope.at_ms)?;
            } else {
                crate::retry::fire_timer(&mut next, index, *generation, envelope.at_ms)?;
            }

            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::LoopIterationStarted { loop_run_id, index } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let parent = state.step_index(loop_run_id)?;
            if !crate::tree::is_frontier(state, &state.steps[parent].step_id)? {
                return Err(EngineError::new("loop.invalid_admission"));
            }
            crate::loop_node::admit(&mut next, parent, *index)?;
            crate::data::note_step_revision(&mut next, parent, envelope.seq);
        }
        Event::LoopChecked { step_run_id, check } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let parent = state.step_index(step_run_id)?;
            crate::loop_node::apply_check(&mut next, parent, check, envelope.at_ms)?;
            crate::data::note_step_revision(&mut next, parent, envelope.seq);
        }
        Event::ExecutionFaulted { fault } if fault.code == crate::QUIESCE_FAULT => {
            // The host could not settle this run within its bound (F16):
            // fail it instead of wedging every later commit. In cleanup the
            // cleanup is suppressed with the same reason.
            if !crate::deadline::open(state) || !fault.step_id.is_empty() {
                return Err(EngineError::new("execution.invalid_fault"));
            }
            next.runtime_errors.push(fault.clone());
            if state.phase == crate::ExecutionPhase::Cleanup {
                crate::cleanup::suppress(&mut next, &fault.code, envelope.at_ms, envelope.seq)?;
            } else {
                crate::cleanup::enter(
                    &mut next,
                    crate::TerminalCause::Failed,
                    Some(fault.code.clone()),
                    envelope.at_ms,
                    envelope.seq,
                )?;
            }
        }
        Event::ExecutionFaulted { fault }
            if fault.code == crate::RUN_SIZE_EXCEEDED
                && state.engine_version >= crate::version::RUN_CEILING =>
        {
            // The host found this run's log past its hard ceiling (engine
            // version 6, CF13b): it closes failed without its cleanup. The
            // fault is the record; replay never measures the log.
            if !crate::cleanup::run_ceiling_applies(state)
                || !fault.step_id.is_empty()
                || fault.step_run_id.is_some()
            {
                return Err(EngineError::new("execution.invalid_fault"));
            }
            next.runtime_errors.push(fault.clone());
            crate::cleanup::exceed_run_size(&mut next, envelope.at_ms, envelope.seq)?;
        }
        Event::ExecutionFaulted { fault } if fault.step_run_id.is_some() => {
            // A started wait whose `until` target is beyond the PRD §17 cap
            // (engine version 4): the wait fails, and so does the run, with
            // the limit as its reason; in cleanup only the step fails.
            let index = state.step_index(
                fault
                    .step_run_id
                    .as_ref()
                    .ok_or(EngineError::invariant("fault step run is absent"))?,
            )?;
            let record = &state.steps[index];
            if state.engine_version < crate::version::WAIT_CAP
                || fault.code != crate::wait::WAIT_EXCEEDED
                || record.step_id != fault.step_id
                || state.status != ExecutionStatus::Running
                || record.status != StepStatus::Ready
                || record.wait.is_some()
                || !crate::tree::is_frontier(state, &record.step_id)?
                || !matches!(
                    state.node(&record.step_id).map(|n| &n.kind),
                    Some(runbook_spec::ir::NodeKind::Wait(_))
                )
            {
                return Err(EngineError::new("execution.invalid_fault"));
            }
            next.runtime_errors.push(fault.clone());
            next.steps[index].status = StepStatus::Failed;
            next.steps[index].finished_at_ms = Some(envelope.at_ms);
            crate::data::note_step_revision(&mut next, index, envelope.seq);
            if state.phase != crate::ExecutionPhase::Cleanup {
                crate::cleanup::enter(
                    &mut next,
                    crate::TerminalCause::Failed,
                    Some(fault.code.clone()),
                    envelope.at_ms,
                    envelope.seq,
                )?;
            }
        }
        Event::ExecutionFaulted { fault } => {
            let record = state
                .steps
                .iter()
                .find(|s| s.step_id == fault.step_id)
                .ok_or(EngineError::new("step.not_found"))?;
            if state.status != ExecutionStatus::Running
                || record.status != StepStatus::Pending
                || !crate::tree::is_frontier(state, &fault.step_id)?
                || !matches!(
                    state.node(&record.step_id).map(|n| &n.kind),
                    Some(runbook_spec::ir::NodeKind::Foreach(_))
                )
            {
                return Err(EngineError::new("step.not_ready"));
            }
            if fault.code.is_empty() || fault.code.len() > 128 {
                return Err(EngineError::new("execution.invalid_fault"));
            }
            next.runtime_errors.push(fault.clone());
            if state.phase == crate::ExecutionPhase::Cleanup {
                let index = next
                    .steps
                    .iter()
                    .position(|s| s.step_id == fault.step_id)
                    .ok_or(EngineError::invariant("fault step is absent"))?;
                next.steps[index].status = StepStatus::Failed;
                next.steps[index].finished_at_ms = Some(envelope.at_ms);
                crate::data::note_step_revision(&mut next, index, envelope.seq);
            } else {
                crate::cleanup::enter(
                    &mut next,
                    crate::TerminalCause::Failed,
                    Some(fault.code.clone()),
                    envelope.at_ms,
                    envelope.seq,
                )?;
            }
        }
        Event::ForeachStarted {
            step_id,
            step_run_id,
            snapshot,
        } => {
            if state.status != ExecutionStatus::Running
                || !crate::tree::is_frontier(state, step_id)?
            {
                return Err(EngineError::new("step.not_ready"));
            }
            let index = state
                .steps
                .iter()
                .position(|s| s.step_id == *step_id)
                .ok_or(EngineError::new("step.not_found"))?;
            if state.steps[index].status != StepStatus::Pending {
                return Err(EngineError::new("step.not_ready"));
            }
            if state
                .steps
                .iter()
                .any(|s| s.id.as_ref() == Some(step_run_id))
            {
                return Err(EngineError::new("step.duplicate_run_id"));
            }
            crate::foreach::validate_snapshot(state, &state.steps[index], snapshot)?;
            next.steps[index].id = Some(step_run_id.clone());
            next.steps[index].status = StepStatus::Ready;
            next.steps[index].foreach = Some(*snapshot.clone());
            next.steps[index].reads = snapshot.reads.clone();
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::IterationAdmitted {
            foreach_run_id,
            index,
        } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let parent = state.step_index(foreach_run_id)?;
            if !crate::tree::is_frontier(state, &state.steps[parent].step_id)? {
                return Err(EngineError::new("foreach.invalid_admission"));
            }
            crate::foreach::admit(&mut next, parent, *index)?;
            crate::data::note_step_revision(&mut next, parent, envelope.seq);
        }
        Event::ExecutionCompleted { status, outputs } => {
            let frontier = crate::tree::frontier(state)?;
            let expected = if state.phase == crate::ExecutionPhase::Cleanup {
                if crate::cleanup::active_root(state)?.is_none() {
                    Some(crate::cleanup::status(state)?)
                } else {
                    None
                }
            } else if frontier.is_empty() {
                Some(ExecutionStatus::Succeeded)
            } else if frontier.len() == 1 && crate::tree::failed(frontier[0].status) {
                Some(ExecutionStatus::Failed)
            } else {
                None
            };
            if state.status != ExecutionStatus::Running || Some(*status) != expected {
                return Err(EngineError::new("execution.not_complete"));
            }
            if outputs
                .values
                .keys()
                .ne(state.compiled().ir().outputs.keys())
            {
                return Err(EngineError::new("execution.invalid_outputs"));
            }
            crate::data::guard_copy(&outputs.values)?;
            if crate::data::outputs_too_large(state, outputs) {
                return Err(EngineError::new(crate::OUTPUTS_TOO_LARGE));
            }
            next.outputs = outputs.values.clone();
            next.output_reads = outputs.reads.clone();
            next.output_revision = envelope.seq;
            next.result = outputs
                .values
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            next.status = *status;
            next.pending_terminal_cause = Some(match status {
                ExecutionStatus::Succeeded => crate::TerminalCause::Succeeded,
                ExecutionStatus::Failed => crate::TerminalCause::Failed,
                ExecutionStatus::Cancelled => crate::TerminalCause::Cancelled,
                _ => return Err(EngineError::new("execution.invalid_outcome")),
            });
        }
        Event::BranchSelected { selection } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = crate::branch::validate_selection(state, selection)?;
            next.steps[index].branch = Some(selection.choice);
            crate::data::append_reads(
                &mut next.steps[index].reads,
                selection.reads.iter().cloned(),
            );
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::SiblingsCancelled {
            parallel_run_id,
            step_run_ids,
        } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(parallel_run_id)?;
            let record = &state.steps[index];
            if record.status != StepStatus::Ready
                || !crate::tree::is_frontier(state, &record.step_id)?
            {
                return Err(EngineError::new("step.not_ready"));
            }
            let expected = crate::parallel::cancellation_targets(state, record)?;
            if expected.is_empty() || expected != *step_run_ids {
                return Err(EngineError::new("step.invalid_cancellation"));
            }
            for id in step_run_ids {
                let index = state.step_index(id)?;
                next.steps[index].status = StepStatus::Cancelled;
                next.steps[index].finished_at_ms = Some(envelope.at_ms);
                crate::data::note_step_revision(&mut next, index, envelope.seq);
            }
        }
        Event::StepCompleted {
            step_run_id,
            status,
            verdict,
        } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(step_run_id)?;
            let record = &state.steps[index];
            if record.status != StepStatus::Ready
                || !crate::tree::is_frontier(state, &record.step_id)?
            {
                return Err(EngineError::new("step.not_complete"));
            }
            if crate::branch::completed_outcome(state, record)? != (*status, *verdict) {
                return Err(EngineError::new("step.invalid_verdict"));
            }
            if record.group_finally.is_some() {
                next.steps[index].group_finally_done = true;
            }
            next.steps[index].status = *status;
            next.steps[index].verdict = *verdict;
            next.steps[index].finished_at_ms = Some(envelope.at_ms);
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::DataChanged { change } => {
            if state.status != ExecutionStatus::Running {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(&change.step_run_id)?;
            if state.steps[index].status != StepStatus::Ready {
                return Err(EngineError::new("step.not_ready"));
            }
            crate::data::validate_changes(
                state,
                state
                    .node(&state.steps[index].step_id)
                    .ok_or(EngineError::new("step.not_found"))?,
                change,
            )?;
            crate::data::apply_changes(&mut next, index, change, envelope.seq, envelope.at_ms);
        }
        Event::MemoryRead { step_run_id, refs } => {
            let index = state.step_index(step_run_id)?;
            next.steps[index].memory_read.extend(refs.iter().cloned());
        }
        Event::MemoryProposed {
            step_run_id,
            memory_id,
        } => {
            let index = state.step_index(step_run_id)?;
            next.steps[index].memory_proposed.push(memory_id.clone());
        }
        Event::MemorySaved {
            step_run_id,
            memory_id,
        } => {
            let index = state.step_index(step_run_id)?;
            next.steps[index].memory_saved.push(memory_id.clone());
        }
        Event::MemoryEdited {
            step_run_id,
            memory_id,
        } => {
            let index = state.step_index(step_run_id)?;
            next.steps[index].memory_edited.push(memory_id.clone());
        }
        Event::ChecklistUpdated { step_run_id, ticks } => {
            let index = crate::checklist::validate(state, step_run_id, ticks)?;
            next.steps[index].checklist = ticks.clone();
        }
        Event::ExecutionStarted {
            data,
            engine_version,
        } => {
            if state.sequence != 0 || state.status != ExecutionStatus::Queued {
                return Err(EngineError::new("execution.already_started"));
            }
            crate::version::check(*engine_version)?;
            next.engine_version = *engine_version;
            crate::decider::supported(state)?;
            crate::data::install_start(&mut next, data, envelope.seq, envelope.at_ms)?;
            next.status = ExecutionStatus::Running;
            next.execution_deadline_at_ms = state
                .compiled()
                .ir()
                .execution
                .timeout
                .as_ref()
                .map(|d| crate::deadline::add(envelope.at_ms, d.millis))
                .transpose()?;
            if let Some(hard) = next.limits.hard_deadline_at_ms {
                next.execution_deadline_at_ms = Some(
                    next.execution_deadline_at_ms
                        .map_or(hard, |deadline| deadline.min(hard)),
                );
            }
        }
        Event::AgentTargetResolved {
            step_id,
            step_run_id,
            target,
        } => crate::runner::target_resolved(&mut next, state, step_id, step_run_id, target)?,
        Event::StepWaiting {
            step_run_id,
            reason,
            detail,
        } => crate::runner::waiting(&mut next, state, step_run_id, reason, detail.as_deref())?,
        Event::CredentialReleased {
            step_run_id,
            credential_id,
            credential_version,
            delivery,
            runner_id,
            claim_id,
        } => crate::runner::released(
            &mut next,
            state,
            step_run_id,
            crate::CredentialRelease {
                credential_id: credential_id.clone(),
                credential_version: *credential_version,
                delivery: delivery.clone(),
                runner_id: runner_id.clone(),
                claim_id: claim_id.clone(),
                at_ms: envelope.at_ms,
                revoked: None,
            },
        )?,
        Event::CredentialRevoked {
            step_run_id,
            credential_id,
            reason,
        } => crate::runner::revoked(&mut next, state, step_run_id, credential_id, reason)?,
        Event::DecisionLeaseClaimed {
            step_run_id,
            lease,
            agent_version_id,
            ..
        } => {
            let i = state.step_index(step_run_id)?;
            crate::runner::admit_decision(state, &state.steps[i], envelope.at_ms)?;
            let approval = next.steps[i]
                .approval
                .as_mut()
                .ok_or(EngineError::new("step.not_approval"))?;
            if approval.decision_lease.is_some() || !approval.agent_notes.is_empty() {
                return Err(EngineError::new("step.not_claimable"));
            }
            approval.decision_lease = Some(lease.clone());
            let runner = &mut next.steps[i].runner;
            runner.agent_version_id = Some(agent_version_id.clone());
            runner.waiting_reason = None;
            runner.waiting_detail = None;
            runner.reported = false;
        }
        Event::DecisionLeaseEnded {
            step_run_id,
            lease_id,
            reason,
        } => {
            let i = state.step_index(step_run_id)?;
            let approval = next.steps[i]
                .approval
                .as_mut()
                .ok_or(EngineError::new("step.not_approval"))?;
            if approval
                .decision_lease
                .as_ref()
                .is_none_or(|lease| &lease.lease_id != lease_id)
            {
                return Err(EngineError::new("claim.stale_generation"));
            }
            approval.decision_lease = None;
            let runner = &mut next.steps[i].runner;
            runner.reported = matches!(reason.as_str(), "agent_decided" | "agent_abstained");
            for release in runner
                .releases
                .iter_mut()
                .filter(|r| r.claim_id.as_ref() == Some(lease_id) && r.revoked.is_none())
            {
                release.revoked = Some(reason.clone());
            }
            crate::runner::claim_ended(&mut next, i);
        }
        Event::ApprovalAgentAbstained { step_run_id, note } => {
            let i = state.step_index(step_run_id)?;
            crate::runner::admit_decision(state, &state.steps[i], envelope.at_ms)?;
            let approval = next.steps[i]
                .approval
                .as_mut()
                .ok_or(EngineError::new("step.not_approval"))?;
            if approval
                .decision_lease
                .as_ref()
                .is_none_or(|lease| lease.lease_id != note.lease_id)
                || !approval.agent_notes.is_empty()
            {
                return Err(EngineError::new("claim.stale_generation"));
            }
            approval.agent_notes.push(note.clone());
        }
        Event::StepCostReported {
            step_run_id, cost, ..
        } => crate::runner::cost_reported(&mut next, state, step_run_id, cost)?,
        Event::StepClaimed {
            step_run_id,
            agent_version_id,
            ..
        } => {
            if !crate::deadline::open(state) {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(step_run_id)?;
            if state.steps[index].status != StepStatus::Ready
                || !matches!(
                    state.node(&state.steps[index].step_id).map(|n| &n.kind),
                    Some(runbook_spec::ir::NodeKind::Task(_))
                )
            {
                return Err(EngineError::new("step.not_claimable"));
            }
            next.steps[index].status = StepStatus::Claimed;
            next.steps[index].actor = Some(envelope.actor.clone());
            let runner = &mut next.steps[index].runner;
            runner.waiting_reason = None;
            runner.waiting_detail = None;
            if agent_version_id.is_some() {
                runner.agent_version_id = agent_version_id.clone();
            }
        }
        Event::StepClaimReleased { step_run_id, .. } | Event::StepClaimExpired { step_run_id } => {
            let index = state.step_index(step_run_id)?;
            if state.steps[index].status != StepStatus::Claimed {
                return Err(EngineError::new("step.not_claimed"));
            }
            next.steps[index].status = StepStatus::Ready;
            next.steps[index].actor = None;
            next.steps[index].taken_over = None;
            crate::runner::claim_ended(&mut next, index);
            if matches!(envelope.event, Event::StepClaimExpired { .. }) {
                // Expired work is offered again with the lease_expired reason.
                next.steps[index].agent_notified_attempt = None;
            }
        }
        Event::ExecutionCredentialsIssued { .. }
        | Event::ExecutionCredentialsRotated { .. }
        | Event::ExecutionTokenReuseDetected { .. }
        | Event::ExecutionCredentialsRevoked { .. } => {
            // Audit only; credential state lives in the credential store.
        }
        Event::StepOverridden {
            step_run_id,
            amendment,
        } => {
            // The recorded author is the actor that submits it, as for votes
            // (R51); core always appends with the amending principal.
            if envelope.actor != amendment.by {
                return Err(EngineError::new("amendment.actor_mismatch"));
            }
            let index = state.step_index(step_run_id)?;
            let from = crate::amendments::current_record(&state.steps[index]);
            if amendment.from != from {
                return Err(EngineError::new("amendment.from_mismatch"));
            }
            let expected = crate::amendments::validate_override(
                state,
                step_run_id,
                &amendment.to,
                &amendment.reason,
                amendment.by.clone(),
            )?;
            if &expected != amendment.as_ref() {
                return Err(EngineError::new("amendment.from_mismatch"));
            }
            if next.steps[index].original.is_none() {
                next.steps[index].original = Some(crate::OriginalStepRecord {
                    verdict: from.verdict,
                    outputs: from.outputs.clone(),
                    summary: from.summary.clone(),
                });
            }
            next.steps[index].verdict = amendment.to.verdict;
            next.steps[index].outputs = amendment.to.outputs.clone();
            next.steps[index].summary = amendment.to.summary.clone();
            next.steps[index].amended = true;
            crate::data::note_step_revision(&mut next, index, envelope.seq);
            next.steps[index].amendments.push((**amendment).clone());
            crate::amendments::mark_stale(&mut next, step_run_id);
        }
        Event::StepReported {
            step_run_id,
            report,
        } => {
            if !crate::deadline::open(state) {
                return Err(EngineError::new("execution.not_running"));
            }
            let index = state.step_index(step_run_id)?;
            if !matches!(
                state.steps[index].status,
                StepStatus::Ready | StepStatus::Claimed
            ) {
                return Err(EngineError::new("step.not_reportable"));
            }
            let report = crate::report::validate_work(
                state
                    .node(&state.steps[index].step_id)
                    .ok_or(EngineError::new("step.not_found"))?,
                &state.steps[index],
                report,
            )?;
            if state.steps[index].human_continuation
                && !matches!(envelope.actor, ActorRef::Human { .. })
            {
                return Err(EngineError::new("step.human_required"));
            }
            let status = match report.status {
                ReportStatus::Succeeded => StepStatus::Succeeded,
                ReportStatus::Skipped => StepStatus::Skipped,
                ReportStatus::Failed | ReportStatus::Blocked => StepStatus::Failed,
            };
            let step = &mut next.steps[index];
            step.status = status;
            step.attempt_history
                .push(std::rc::Rc::new(crate::AttemptRecord {
                    attempt: step.attempt,
                    actor: envelope.actor.clone(),
                    at_ms: envelope.at_ms,
                    report: report.clone(),
                    timed_out: false,
                }));
            step.verdict = report.verdict;
            step.outputs = report.outputs;
            step.summary = report.summary;
            step.evidence.extend(report.evidence);
            step.files.extend(report.files);
            step.logs.extend(report.logs);
            step.actor = Some(envelope.actor.clone());
            step.runner.reported = true;
            step.finished_at_ms = Some(envelope.at_ms);
            if status == StepStatus::Failed {
                crate::retry::recover(&mut next, index)?;
            }
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
        Event::StepReady {
            step_id,
            step_run_id,
            reads,
        }
        | Event::StepSkipped {
            step_id,
            step_run_id,
            reads,
            ..
        } => {
            if matches!(envelope.event, Event::StepReady { .. })
                && matches!(
                    state.node(step_id).map(|n| &n.kind),
                    Some(runbook_spec::ir::NodeKind::Foreach(_))
                )
            {
                return Err(EngineError::new("foreach.snapshot_required"));
            }
            if state.status != ExecutionStatus::Running
                || !crate::tree::is_frontier(state, step_id)?
                || state
                    .steps
                    .iter()
                    .find(|s| s.step_id == *step_id)
                    .is_none_or(|s| s.status != StepStatus::Pending)
            {
                return Err(EngineError::new("step.not_ready"));
            }
            if state
                .steps
                .iter()
                .any(|s| s.id.as_ref() == Some(step_run_id))
            {
                return Err(EngineError::new("step.duplicate_run_id"));
            }
            if crate::cleanup::counted(
                state
                    .node(step_id)
                    .ok_or(EngineError::invariant("materialized node is absent"))?,
            ) {
                if crate::cleanup::at_cap(state) {
                    return Err(EngineError::new("limit.step_runs_exceeded"));
                }
                next.materialized_steps = next
                    .materialized_steps
                    .checked_add(1)
                    .ok_or(EngineError::invariant("step count overflow"))?;
            }
            let step = next
                .steps
                .iter_mut()
                .find(|s| s.step_id == *step_id)
                .ok_or(EngineError::new("step.not_found"))?;
            step.id = Some(step_run_id.clone());
            step.status = StepStatus::Ready;
            step.reads = reads.clone();
            if let Event::StepSkipped { reason, .. } = &envelope.event {
                step.status = StepStatus::Skipped;
                step.skip_reason = Some(*reason);
                step.finished_at_ms = Some(envelope.at_ms);
            }
            let index = next.step_index(step_run_id)?;
            if matches!(envelope.event, Event::StepReady { .. }) {
                let duration = state.node(step_id).and_then(|node| match &node.kind {
                    runbook_spec::ir::NodeKind::Task(_) => node.timeout.as_ref(),
                    runbook_spec::ir::NodeKind::Approval(a) => a.timeout.as_ref(),
                    _ => None,
                });
                next.steps[index].deadline_at_ms = duration
                    .map(|d| crate::deadline::add(envelope.at_ms, d.millis))
                    .transpose()?;
            }
            if matches!(envelope.event, Event::StepReady { .. })
                && matches!(
                    state.node(step_id).map(|n| &n.kind),
                    Some(runbook_spec::ir::NodeKind::Approval(_))
                )
            {
                next.steps[index].approval = Some(crate::ApprovalState::default());
                next.steps[index].status = StepStatus::Waiting;
            }
            if matches!(envelope.event, Event::StepReady { .. })
                && matches!(
                    state.node(step_id).map(|n| &n.kind),
                    Some(runbook_spec::ir::NodeKind::Loop(_))
                )
            {
                next.steps[index].loop_state = Some(crate::LoopState::default());
            }
            if matches!(envelope.event, Event::StepSkipped { .. })
                && state
                    .node(&state.steps[index].step_id)
                    .ok_or(EngineError::new("step.not_found"))?
                    .guard
                    .is_none()
            {
                return Err(EngineError::new("step.invalid_skip"));
            }
            crate::data::note_step_revision(&mut next, index, envelope.seq);
        }
    }
    next.work_reason = if next.phase == crate::ExecutionPhase::Cleanup {
        crate::WorkReason::Cleanup
    } else {
        match envelope.event {
            Event::ApprovalDecided { .. } => crate::WorkReason::ApprovalDecided,
            Event::DecisionApplied {
                decision: crate::AppliedDecision::Automatic(_),
            } => crate::WorkReason::AutoDecision,
            Event::DecisionApplied {
                decision: crate::AppliedDecision::Agent(_),
            } => crate::WorkReason::ApprovalDecided,
            Event::SignalReceived { .. } => crate::WorkReason::Signal,
            Event::StepClaimExpired { .. } => crate::WorkReason::LeaseExpiry,
            Event::TimerFired { .. } => crate::WorkReason::Timer,
            _ => next.work_reason,
        }
    };
    crate::followup::note_open(state, &mut next, envelope.at_ms);
    next.sequence = envelope.seq;
    next.at_ms = envelope.at_ms;
    Ok(next)
}
