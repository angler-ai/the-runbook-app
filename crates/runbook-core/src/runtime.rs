//! The single shared runtime path: realize decider commands as events,
//! settle to quiescence, and derive timer changes and outbox intents.
//! Actor sessions (B6a) and the drain worker (B4.4) both drive runs
//! through here; step-run IDs come from the host's IdSource.
use crate::{
    StoredExecution, TimerChange, TimerRecord, actor::ActorError, catalog::TenantScope,
    directory::DirectorySnapshot,
};
use runbook_engine::{
    Command, Event, EventEnvelope, ExecutionState, apply, apply_automatic_decision, decide,
};
use runbook_model::{ActorRef, StepRunId, StepStatus};
use std::collections::BTreeMap;

/// Host-supplied step-run ID source; tests use deterministic sequences.
pub trait IdSource {
    fn next_step_run_id(&mut self) -> StepRunId;
}

/// Deterministic IDs derived from the starting sequence: each new step
/// run takes the next sequence number, formatted as a sortable ULID body.
pub struct SequenceIds {
    next: u64,
}
impl SequenceIds {
    pub fn new(after_sequence: u64) -> Self {
        Self {
            next: after_sequence + 1,
        }
    }
}
impl IdSource for SequenceIds {
    fn next_step_run_id(&mut self) -> StepRunId {
        let id = format!("sr_{:026}", self.next)
            .parse()
            .expect("sequence-derived step run id");
        self.next += 1;
        id
    }
}

/// One in-flight run being driven forward at a fixed event time.
pub struct Runtime {
    pub state: ExecutionState,
    pub directory: DirectorySnapshot,
    pub scope: TenantScope,
    pub at_ms: i64,
    ids: Box<dyn IdSource>,
    pending: Vec<EventEnvelope>,
    intents: Vec<runbook_engine::NotificationIntent>,
    ceiling: Option<Ceiling>,
}
/// CF13b: the hard ceiling a run's event log is held to while it settles
/// ([`crate::run_budget::run_ceiling`]).
struct Ceiling {
    max: u64,
    /// The stored log's bytes, counted once by the host.
    stored: u64,
    /// The pending events counted so far, and their bytes.
    counted: usize,
    pending: u64,
}
impl Runtime {
    pub fn new(
        state: ExecutionState,
        directory: DirectorySnapshot,
        scope: TenantScope,
        at_ms: i64,
        ids: impl IdSource + 'static,
    ) -> Self {
        Self {
            state,
            directory,
            scope,
            at_ms,
            ids: Box::new(ids),
            pending: Vec::new(),
            intents: Vec::new(),
            ceiling: None,
        }
    }
    /// Hold this run's event log to `max` bytes while it settles, its
    /// stored log being `stored` bytes (CF13b). At the start of each settle
    /// round, an engine version 6 run whose log (stored and pending) is past
    /// `max` is faulted with [`runbook_engine::RUN_SIZE_EXCEEDED`] and
    /// closes failed in the same settle; runs under earlier versions are
    /// not held to it.
    pub fn hold_to_ceiling(&mut self, stored: u64, max: u64) {
        self.ceiling = Some(Ceiling {
            max,
            stored,
            counted: 0,
            pending: 0,
        });
    }
    /// Whether the log, the pending events included, is past the ceiling.
    /// Each pending event is counted once.
    fn past_ceiling(&mut self) -> bool {
        let Some(ceiling) = &mut self.ceiling else {
            return false;
        };
        let fresh = &self.pending[ceiling.counted.min(self.pending.len())..];
        ceiling.pending = ceiling
            .pending
            .saturating_add(crate::run_budget::log_bytes(fresh));
        ceiling.counted = self.pending.len();
        ceiling.stored.saturating_add(ceiling.pending) > ceiling.max
    }
    pub fn step_index(&self, step_run_id: &StepRunId) -> Result<usize, crate::actor::ActorError> {
        self.state
            .steps
            .iter()
            .position(|step| step.id.as_ref() == Some(step_run_id))
            .ok_or(crate::actor::ActorError::rejected("step.not_found"))
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn pending_events(&self) -> &[EventEnvelope] {
        &self.pending
    }
    pub fn intents(&self) -> &[runbook_engine::NotificationIntent] {
        &self.intents
    }
    pub fn append(
        &mut self,
        event: Event,
        actor: ActorRef,
    ) -> Result<(), crate::actor::ActorError> {
        let envelope = EventEnvelope {
            execution_id: self.state.id.clone(),
            seq: self.state.sequence + 1,
            at_ms: self.at_ms,
            actor,
            event,
        };
        let next = apply(&self.state, &envelope)?;
        let before = std::mem::replace(&mut self.state, next);
        self.intents.extend(runbook_engine::notifications_for(
            &envelope,
            &before,
            &self.state,
        ));
        self.pending.push(envelope);
        Ok(())
    }
    fn run_id(&mut self) -> StepRunId {
        self.ids.next_step_run_id()
    }

    pub fn realize(&mut self, command: Command) -> Result<(), ActorError> {
        use runbook_engine::{ApprovalRequestSnapshot, EscalationSnapshot, Event as E};
        if let Command::ApplyAutomaticDecision { policy } = &command {
            for event in apply_automatic_decision(&self.state, policy)? {
                self.append(event, ActorRef::System)?;
            }
            return Ok(());
        }
        let event = match command {
            Command::ResolveApprovalRequest { request } => {
                let recipients = expand_teams(&self.directory, &request.targets);
                E::ApprovalRequested {
                    snapshot: ApprovalRequestSnapshot {
                        request,
                        recipients,
                        directory_revision: self.directory.revision,
                    },
                }
            }
            Command::NotifyAgentWork { work } => E::AgentWorkAvailable { work },
            Command::SelectEnd { selection } => E::EndSelected { selection },
            Command::RecordStepLimit { step_id } => E::StepLimitExceeded { step_id },
            Command::BeginGroupFinally {
                group_run_id,
                outcome,
            } => E::GroupFinallyStarted {
                group_run_id,
                outcome,
            },
            Command::BeginCleanup { cause, reason } => E::CleanupStarted { cause, reason },
            Command::CompleteCleanupRoot { root } => E::CleanupRootCompleted { root },
            Command::SuppressCleanup { reason } => E::CleanupSuppressed { reason },
            Command::ApplyAutomaticDecision { .. } => unreachable!("handled above"),
            Command::ExpireExecution { deadline_at_ms } => {
                E::ExecutionDeadlineElapsed { deadline_at_ms }
            }
            Command::ExpireTask {
                step_run_id,
                deadline_at_ms,
            } => E::TaskDeadlineElapsed {
                step_run_id,
                deadline_at_ms,
            },
            Command::ResolveEscalation { request } => E::ApprovalEscalated {
                snapshot: EscalationSnapshot {
                    recipients: request.targets.clone(),
                    directory_revision: self.directory.revision,
                    request,
                },
            },
            // LANG9: a reminder records itself; an escalation keeps its
            // targets (teams stay teams, as for approvals), stamped with the
            // directory revision it was resolved against.
            Command::RemindStep {
                step_run_id,
                due_at_ms,
            } => E::StepReminded {
                step_run_id,
                due_at_ms,
            },
            Command::ResolveStepEscalation { request } => E::StepEscalated {
                snapshot: EscalationSnapshot {
                    recipients: request.targets.clone(),
                    directory_revision: self.directory.revision,
                    request,
                },
            },
            Command::ExpireApproval {
                step_run_id,
                deadline_at_ms,
            } => E::ApprovalExpired {
                step_run_id,
                deadline_at_ms,
            },
            // Escalation and expiry notices name their teams' current
            // active users, as approval requests do; the pending notice in
            // state keeps the team targets (and eligibility stays per team).
            Command::NotifyApproval { mut notice } => {
                let recipients = expand_teams(&self.directory, &notice.recipients);
                if recipients != notice.recipients {
                    notice.recipients = recipients;
                    notice.directory_revision = Some(self.directory.revision);
                }
                E::ApprovalNotificationQueued { notice }
            }
            Command::ConsumeSignal { signal } => E::SignalReceived { signal },
            Command::StartWait { start } => E::WaitStarted { start },
            // LANG4: the child's ID is a function of this run and the step.
            Command::RequestCall { request } => E::CallRequested {
                start: Box::new(runbook_engine::CallStart {
                    child_execution_id: crate::calls::child_execution_id(
                        &self.state.id,
                        &request.step_run_id,
                    ),
                    base_sequence: request.base_sequence,
                    step_run_id: request.step_run_id,
                    inputs: request.inputs,
                    reads: request.reads,
                }),
            },
            Command::FireWaitTimer {
                step_run_id,
                generation,
            } => E::TimerFired {
                step_run_id,
                generation,
            },
            Command::DecideApproval { decision } => E::ApprovalDecided { decision },
            Command::ResolveHandoff { request } => E::StepHandedOff {
                handoff: Box::new(crate::identity::handoff_recipients(
                    &self.directory,
                    &self.scope.project_id,
                    *request,
                    self.state.compiled().users(),
                )),
            },
            Command::ResolveAssignee { request } => E::StepAssigneeResolved {
                resolution: Box::new(crate::identity::assignee_recipients(
                    &self.directory,
                    &self.scope.project_id,
                    *request,
                    self.state.compiled().users(),
                )),
            },
            Command::NotifyStepBlocked { notification } => {
                E::StepBlockedNotificationQueued { notification }
            }
            Command::ScheduleRetry {
                step_run_id,
                attempt,
                due_at_ms,
            } => E::RetryScheduled {
                step_run_id,
                attempt,
                due_at_ms,
            },
            Command::ActivateCatch { selection } => E::CatchActivated { selection },
            Command::StartLoopIteration { loop_run_id, index } => {
                E::LoopIterationStarted { loop_run_id, index }
            }
            Command::RecordLoopCheck { step_run_id, check } => {
                E::LoopChecked { step_run_id, check }
            }
            Command::RecordFault { fault } => E::ExecutionFaulted { fault },
            Command::StartForeach { step_id, snapshot } => E::ForeachStarted {
                step_id,
                snapshot,
                step_run_id: self.run_id(),
            },
            Command::AdmitIteration {
                foreach_run_id,
                index,
            } => E::IterationAdmitted {
                foreach_run_id,
                index,
            },
            Command::SelectBranch { selection } => E::BranchSelected { selection },
            Command::CancelSiblings {
                parallel_run_id,
                step_run_ids,
            } => E::SiblingsCancelled {
                parallel_run_id,
                step_run_ids,
            },
            Command::CompleteStep {
                step_run_id,
                status,
                verdict,
            } => E::StepCompleted {
                step_run_id,
                status,
                verdict,
            },
            Command::CompleteExecution { status, outputs } => {
                E::ExecutionCompleted { status, outputs }
            }
            Command::ApplyData { change } => E::DataChanged { change },
            Command::SkipStep {
                step_id,
                reason,
                reads,
            } => E::StepSkipped {
                step_id,
                step_run_id: self.run_id(),
                reason,
                reads,
            },
            Command::MakeStepReady { step_id, reads } => E::StepReady {
                step_id,
                step_run_id: self.run_id(),
                reads,
            },
        };
        self.append(event, ActorRef::System)
    }

    /// Run the decider to quiescence, translating every command. A run that
    /// does not quiesce within the bound (for example a set-only loop with
    /// no delay) is failed with a recorded fault, which cancels its work,
    /// instead of wedging every later commit (F16).
    pub fn settle(&mut self) -> Result<(), ActorError> {
        if self.settle_rounds()? {
            return Ok(());
        }
        if runbook_engine::deadline_open(&self.state) {
            self.append(
                Event::ExecutionFaulted {
                    fault: runbook_engine::RuntimeFault::failed_to_quiesce(),
                },
                ActorRef::System,
            )?;
            if self.settle_rounds()? {
                return Ok(());
            }
        }
        Err(ActorError::rejected("engine.failed_to_quiesce"))
    }
    fn settle_rounds(&mut self) -> Result<bool, ActorError> {
        for _ in 0..1000 {
            // CF13b: checked between rounds, so one round's events at most
            // pass the ceiling before the run is faulted; the fault and the
            // close are small (the closing outputs held to their budget).
            if self.ceiling.is_some()
                && runbook_engine::run_ceiling_applies(&self.state)
                && self.past_ceiling()
            {
                self.append(
                    Event::ExecutionFaulted {
                        fault: runbook_engine::RuntimeFault::run_size_exceeded(),
                    },
                    ActorRef::System,
                )?;
            }
            let commands = decide(&self.state)?;
            if commands.is_empty() {
                return Ok(true);
            }
            for command in commands {
                self.realize(command)?;
            }
        }
        Ok(false)
    }

    pub fn timer_changes(&self, previous: &StoredExecution) -> Vec<TimerChange> {
        timer_changes(previous, &self.state)
    }
}

/// Desired timers from `state`, diffed against the previously stored set
/// under the store generation rules: loop, retry and wait timers, task and
/// execution deadlines, plus one advance-time wakeup.
pub fn timer_changes(previous: &StoredExecution, state: &ExecutionState) -> Vec<TimerChange> {
    let id = &state.id;
    let mut desired: BTreeMap<String, (u64, i64)> = BTreeMap::new();
    if let Some(deadline) = state.execution_deadline_at_ms
        && !matches!(
            state.status,
            runbook_model::ExecutionStatus::Succeeded
                | runbook_model::ExecutionStatus::Failed
                | runbook_model::ExecutionStatus::Cancelled
        )
    {
        desired.insert("execution-deadline".into(), (1, deadline));
    }
    for step in &state.steps {
        let Some(step_run_id) = &step.id else {
            continue;
        };
        if let Some(deadline) = step.deadline_at_ms
            && step.status == StepStatus::Ready
        {
            desired.insert(format!("task-deadline:{step_run_id}"), (1, deadline));
        }
        if let Some(wait) = &step.wait
            && let Some(timeout) = wait.timeout_at_ms
        {
            desired.insert(
                format!("wait:{step_run_id}:{}", wait.generation),
                (1, timeout),
            );
        }
        if let Some(timer) = step
            .loop_state
            .as_ref()
            .and_then(|loop_state| loop_state.timer.as_ref())
        {
            desired.insert(
                format!("loop:{step_run_id}:{}", timer.generation),
                (1, timer.due_at_ms),
            );
        }
        if let Some(retry) = &step.retry_timer {
            desired.insert(
                format!("retry:{step_run_id}:{}", retry.generation),
                (1, retry.due_at_ms),
            );
        }
    }
    if let Ok(Some(wakeup)) = runbook_engine::next_wakeup(state) {
        desired.insert("advance-time".into(), (1, wakeup));
    }
    // LANG4: a call's child start, cancellation and result are work items.
    crate::calls::desired(state, &mut desired);
    let mut changes = Vec::new();
    for (key, (generation, due)) in &desired {
        let timer = TimerRecord {
            execution_id: id.clone(),
            key: key.clone(),
            generation: *generation,
            due_at_ms: *due,
        };
        match previous.timers.get(key) {
            Some(previous) if previous == &timer => {}
            Some(previous) => changes.push(TimerChange::Upsert(TimerRecord {
                generation: previous.generation + 1,
                ..timer
            })),
            None => {
                let generation = previous
                    .timer_history
                    .get(key)
                    .map_or(*generation, |prior| prior.generation + 1);
                changes.push(TimerChange::Upsert(TimerRecord {
                    generation,
                    ..timer
                }));
            }
        }
    }
    for key in previous.timers.keys() {
        if !desired.contains_key(key) {
            changes.push(TimerChange::Delete {
                execution_id: id.clone(),
                key: key.clone(),
            });
        }
    }
    changes
}

/// Notification recipients for approval events, against this session's
/// directory snapshot (D28): each team is replaced by its active members,
/// and a team with no active member stays named as the team, so the event
/// still shows where it was routed instead of reaching nobody silently.
/// Explicitly named users are kept only while active (D16). Order follows
/// the targets, without duplicates. Recipients never authorize a vote.
pub fn expand_teams(
    directory: &DirectorySnapshot,
    targets: &[runbook_model::RecipientRef],
) -> Vec<runbook_model::RecipientRef> {
    use runbook_model::RecipientRef;
    let mut out: Vec<RecipientRef> = Vec::new();
    for target in targets {
        let active = |user: &runbook_model::UserId| {
            directory
                .users
                .get(user)
                .is_some_and(|record| record.active)
        };
        let expanded = match target {
            RecipientRef::Team(id) => match id.parse::<runbook_model::TeamId>() {
                Ok(team) => {
                    let members: Vec<_> = directory
                        .team_members
                        .iter()
                        .filter(|(member_team, user)| *member_team == team && active(user))
                        .map(|(_, user)| RecipientRef::User(user.clone()))
                        .collect();
                    if members.is_empty() {
                        vec![target.clone()]
                    } else {
                        members
                    }
                }
                Err(_) => vec![target.clone()],
            },
            RecipientRef::User(user) if !active(user) => vec![],
            other => vec![other.clone()],
        };
        for recipient in expanded {
            if !out.contains(&recipient) {
                out.push(recipient);
            }
        }
    }
    out
}

/// Open human work derived from settled state (B8f inbox source): ready
/// or claimed tasks, undecided approvals, and handed-off steps. Claimed
/// agent work is not human inbox work. Assignments disappear when their
/// step leaves an open status, because the next commit replaces the set.
///
/// `previous` is the assignment set of the last commit: an assignment whose
/// step, kind and recipients are unchanged keeps its sequence and time, so
/// unrelated activity does not make it newer (D33 newest-assignment order).
/// Approval recipients stay the full eligible set (requested plus
/// escalation, PRD approval escalation); humans who voted are listed in
/// `voted_by` and the inbox hides the item from them.
pub fn assignments(
    state: &ExecutionState,
    claims: &std::collections::BTreeMap<runbook_model::StepRunId, crate::ClaimWrite>,
    sequence: u64,
    previous: &[crate::catalog::Assignment],
) -> Vec<crate::catalog::Assignment> {
    use crate::catalog::{Assignment, AssignmentKind};
    use runbook_model::RecipientRef;
    let mut out = Vec::new();
    for step in &state.steps {
        let Some(step_run_id) = &step.id else {
            continue;
        };
        let base = |kind, recipients: Vec<RecipientRef>| Assignment {
            execution_id: state.id.clone(),
            step_run_id: step_run_id.clone(),
            sequence,
            kind,
            recipients,
            updated_at_ms: state.at_ms,
            voted_by: vec![],
            path: crate::execution_document::path_of(step),
            title: runbook_engine::step_title(state, step).unwrap_or_else(|| step.step_id.clone()),
        };
        let push = |out: &mut Vec<Assignment>, mut assignment: Assignment| {
            if assignment.recipients.is_empty() {
                return;
            }
            if let Some(prior) = previous.iter().find(|prior| {
                prior.step_run_id == assignment.step_run_id
                    && prior.kind == assignment.kind
                    && prior.recipients == assignment.recipients
            }) {
                assignment.sequence = prior.sequence;
                assignment.updated_at_ms = prior.updated_at_ms;
            }
            out.push(assignment);
        };
        if step.status == StepStatus::Waiting
            && let Some(approval) = &step.approval
            && approval.decision.is_none()
        {
            let mut recipients: Vec<RecipientRef> = Vec::new();
            let requested = approval.requested.iter().flat_map(|r| r.recipients.iter());
            let escalation = approval.escalation.iter().flat_map(|e| e.recipients.iter());
            for recipient in requested.chain(escalation) {
                if !recipients.contains(recipient) {
                    recipients.push(recipient.clone());
                }
            }
            let mut voted_by: Vec<runbook_model::UserId> = Vec::new();
            for record in &approval.votes {
                if let runbook_model::ActorRef::Human { user_id } = &record.vote.actor
                    && !voted_by.contains(user_id)
                {
                    voted_by.push(user_id.clone());
                }
            }
            let mut assignment = base(AssignmentKind::Approval, recipients);
            assignment.voted_by = voted_by;
            push(&mut out, assignment);
            continue;
        }
        // Unresolved (waiting) or resolved and ready for a person.
        if (step.status == StepStatus::Waiting
            || (step.status == StepStatus::Ready && step.human_continuation))
            && let Some(handoff) = &step.handoff
        {
            push(
                &mut out,
                base(AssignmentKind::Handoff, handoff.eligible_recipients.clone()),
            );
            continue;
        }
        if !matches!(step.status, StepStatus::Ready | StepStatus::Claimed) {
            continue;
        }
        let Some(node) = state.node(&step.step_id) else {
            continue;
        };
        let runbook_spec::ir::NodeKind::Task(task) = &node.kind else {
            continue;
        };
        if step.status == StepStatus::Claimed {
            if let Some(runbook_model::ActorRef::Human { user_id }) =
                claims.get(step_run_id).map(|claim| &claim.owner)
            {
                push(
                    &mut out,
                    base(
                        AssignmentKind::Work,
                        vec![RecipientRef::User(user_id.clone())],
                    ),
                );
            }
            continue;
        }
        // LANG7: a strict task routes to its assignee as resolved for this
        // attempt (an expression assignee included).
        let mut recipients = match step
            .strict_assignee
            .as_ref()
            .filter(|r| r.request.attempt == step.attempt)
        {
            Some(resolved) => resolved.eligible_recipients.clone(),
            None => crate::execution_document::assignees_of(state, task),
        };
        // LANG9: an escalated task also routes to its escalation.
        for recipient in step.followup_escalation.iter().flat_map(|e| &e.recipients) {
            if !recipients.contains(recipient) {
                recipients.push(recipient.clone());
            }
        }
        push(&mut out, base(AssignmentKind::Work, recipients));
    }
    out
}
