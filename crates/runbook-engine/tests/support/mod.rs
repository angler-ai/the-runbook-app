#![allow(dead_code)]
use runbook_engine::*;
use runbook_model::{ActorRef, Data, ReportStatus, StepStatus, Verdict};
use runbook_spec::{CompileContext, compile, parse};
use std::rc::Rc;

/// The child execution ID fixtures give every `call` (LANG4).
pub const CHILD: &str = "exe_00000000000000000000000002";
pub struct Trace {
    pub state: ExecutionState,
    pub events: Vec<EventEnvelope>,
    pub commands: Vec<Command>,
    pub ready: Vec<String>,
    pub error_codes: ErrorCodes,
    pub initial: ExecutionState,
    directory: CompileContext,
    pub team_members: std::collections::BTreeMap<String, Vec<runbook_model::UserId>>,
}
impl Trace {
    pub fn step(&self, id: &str) -> &StepState {
        self.state.steps.iter().find(|s| s.step_id == id).unwrap()
    }
    pub fn count(&self, name: &str) -> usize {
        self.events
            .iter()
            .filter(|e| e.event.name() == name)
            .count()
    }
    pub fn event_names(&self) -> Vec<&'static str> {
        self.events.iter().map(|e| e.event.name()).collect()
    }
    /// Rebuilds the state the way storage does: the definition is restored
    /// from its sealed snapshot bytes and every event goes through its stored
    /// JSON form, which must re-encode to identical bytes. Folding the
    /// in-memory events again would only prove that `apply` is a function.
    pub fn replay(&self) -> ExecutionState {
        replay_stored(self.state.compiled(), &self.state.id, &self.events)
    }
    pub fn append(&mut self, event: Event, actor: ActorRef, at_ms: i64) {
        let envelope = EventEnvelope {
            execution_id: self.state.id.clone(),
            seq: self.state.sequence + 1,
            at_ms,
            actor,
            event,
        };
        match apply(&self.state, &envelope) {
            Ok(state) => {
                self.state = state;
                if let Event::ExecutionFaulted { fault } = &envelope.event {
                    self.error_codes.push(fault.code.clone());
                }
                self.events.push(envelope);
            }
            Err(e) => self.error_codes.push(e.code().into()),
        }
    }
    pub fn commit_command(&mut self, command: Command, at_ms: i64) {
        if let Command::ApplyAutomaticDecision { policy } = &command {
            let events = apply_automatic_decision(&self.state, policy).unwrap();
            self.commands.push(command);
            for event in events {
                let before = self.events.len();
                self.append(event, ActorRef::System, at_ms);
                assert_eq!(self.events.len(), before + 1, "{:?}", self.error_codes);
            }
            return;
        }
        let event = match &command {
            Command::ResolveApprovalRequest { request } => Event::ApprovalRequested {
                snapshot: ApprovalRequestSnapshot {
                    request: request.clone(),
                    recipients: request
                        .targets
                        .iter()
                        .flat_map(|r| match r {
                            runbook_model::RecipientRef::Team(id)
                                if self.team_members.contains_key(id) =>
                            {
                                self.team_members[id]
                                    .iter()
                                    .cloned()
                                    .map(runbook_model::RecipientRef::User)
                                    .collect::<Vec<_>>()
                            }
                            _ => vec![r.clone()],
                        })
                        .collect(),
                    directory_revision: 1,
                },
            },
            Command::NotifyAgentWork { work } => Event::AgentWorkAvailable { work: work.clone() },
            Command::SelectEnd { selection } => Event::EndSelected {
                selection: selection.clone(),
            },
            Command::RecordStepLimit { step_id } => Event::StepLimitExceeded {
                step_id: step_id.clone(),
            },
            Command::BeginGroupFinally {
                group_run_id,
                outcome,
            } => Event::GroupFinallyStarted {
                group_run_id: group_run_id.clone(),
                outcome: outcome.clone(),
            },
            Command::BeginCleanup { cause, reason } => Event::CleanupStarted {
                cause: *cause,
                reason: reason.clone(),
            },
            Command::CompleteCleanupRoot { root } => {
                Event::CleanupRootCompleted { root: root.clone() }
            }
            Command::SuppressCleanup { reason } => Event::CleanupSuppressed {
                reason: reason.clone(),
            },
            Command::ApplyAutomaticDecision { .. } => unreachable!("handled ordered batch"),
            Command::ExpireExecution { deadline_at_ms } => Event::ExecutionDeadlineElapsed {
                deadline_at_ms: *deadline_at_ms,
            },
            Command::ExpireTask {
                step_run_id,
                deadline_at_ms,
            } => Event::TaskDeadlineElapsed {
                step_run_id: step_run_id.clone(),
                deadline_at_ms: *deadline_at_ms,
            },
            Command::ResolveEscalation { request } => Event::ApprovalEscalated {
                snapshot: EscalationSnapshot {
                    request: request.clone(),
                    recipients: request.targets.clone(),
                    directory_revision: 1,
                },
            },
            Command::RemindStep {
                step_run_id,
                due_at_ms,
            } => Event::StepReminded {
                step_run_id: step_run_id.clone(),
                due_at_ms: *due_at_ms,
            },
            Command::ResolveStepEscalation { request } => Event::StepEscalated {
                snapshot: EscalationSnapshot {
                    request: request.clone(),
                    recipients: request.targets.clone(),
                    directory_revision: 1,
                },
            },
            Command::ExpireApproval {
                step_run_id,
                deadline_at_ms,
            } => Event::ApprovalExpired {
                step_run_id: step_run_id.clone(),
                deadline_at_ms: *deadline_at_ms,
            },
            Command::NotifyApproval { notice } => Event::ApprovalNotificationQueued {
                notice: notice.clone(),
            },
            Command::ConsumeSignal { signal } => Event::SignalReceived {
                signal: signal.clone(),
            },
            Command::StartWait { start } => Event::WaitStarted {
                start: start.clone(),
            },
            // LANG4: fixtures name the child after the parent's last digit + 1.
            Command::RequestCall { request } => Event::CallRequested {
                start: Box::new(CallStart {
                    base_sequence: request.base_sequence,
                    step_run_id: request.step_run_id.clone(),
                    child_execution_id: CHILD.parse().unwrap(),
                    inputs: request.inputs.clone(),
                    reads: request.reads.clone(),
                }),
            },
            Command::FireWaitTimer {
                step_run_id,
                generation,
            } => Event::TimerFired {
                step_run_id: step_run_id.clone(),
                generation: *generation,
            },
            Command::DecideApproval { decision } => Event::ApprovalDecided {
                decision: decision.clone(),
            },
            Command::ResolveHandoff { request } => {
                let resolve = |recipient: &runbook_spec::ir::Recipient| match recipient {
                    runbook_spec::ir::Recipient::User(name) => self
                        .directory
                        .users
                        .get(name)
                        .cloned()
                        .map(runbook_model::RecipientRef::User),
                    runbook_spec::ir::Recipient::Team(name) => self
                        .directory
                        .teams
                        .get(name)
                        .map(|id| runbook_model::RecipientRef::Team(id.to_string())),
                };
                let mut eligible: Vec<_> = request
                    .assignee
                    .as_ref()
                    .and_then(resolve)
                    .into_iter()
                    .collect();
                let mut source = HandoffSource::Assignee;
                if eligible.is_empty() && !request.strict {
                    eligible = request.owners.iter().filter_map(resolve).collect();
                    source = HandoffSource::Owners;
                }
                let notifications = if eligible.is_empty() {
                    source = HandoffSource::Unresolved;
                    vec![runbook_model::RecipientRef::User(
                        "usr_00000000000000000000000009".parse().unwrap(),
                    )]
                } else {
                    eligible.clone()
                };
                Event::StepHandedOff {
                    handoff: Box::new(ResolvedHandoff {
                        request: *request.clone(),
                        source,
                        eligible_recipients: eligible,
                        notification_recipients: notifications,
                        directory_revision: 1,
                    }),
                }
            }
            Command::ResolveAssignee { request } => {
                let eligible: Vec<_> = request
                    .assignee
                    .as_ref()
                    .and_then(|recipient| match recipient {
                        runbook_spec::ir::Recipient::User(name) => self
                            .directory
                            .users
                            .get(name)
                            .cloned()
                            .map(runbook_model::RecipientRef::User),
                        runbook_spec::ir::Recipient::Team(name) => self
                            .directory
                            .teams
                            .get(name)
                            .map(|id| runbook_model::RecipientRef::Team(id.to_string())),
                    })
                    .into_iter()
                    .collect();
                let notifications = if eligible.is_empty() {
                    vec![runbook_model::RecipientRef::User(
                        "usr_00000000000000000000000009".parse().unwrap(),
                    )]
                } else {
                    vec![]
                };
                Event::StepAssigneeResolved {
                    resolution: Box::new(ResolvedAssignee {
                        request: *request.clone(),
                        eligible_recipients: eligible,
                        notification_recipients: notifications,
                        directory_revision: 1,
                    }),
                }
            }
            Command::NotifyStepBlocked { notification } => Event::StepBlockedNotificationQueued {
                notification: notification.clone(),
            },
            Command::ScheduleRetry {
                step_run_id,
                attempt,
                due_at_ms,
            } => Event::RetryScheduled {
                step_run_id: step_run_id.clone(),
                attempt: *attempt,
                due_at_ms: *due_at_ms,
            },
            Command::ActivateCatch { selection } => Event::CatchActivated {
                selection: selection.clone(),
            },
            Command::StartLoopIteration { loop_run_id, index } => Event::LoopIterationStarted {
                loop_run_id: loop_run_id.clone(),
                index: *index,
            },
            Command::RecordLoopCheck { step_run_id, check } => Event::LoopChecked {
                step_run_id: step_run_id.clone(),
                check: check.clone(),
            },
            Command::RecordFault { fault } => Event::ExecutionFaulted {
                fault: fault.clone(),
            },
            Command::StartForeach { step_id, snapshot } => Event::ForeachStarted {
                step_id: step_id.clone(),
                snapshot: snapshot.clone(),
                step_run_id: format!("sr_{:026}", self.events.len()).parse().unwrap(),
            },
            Command::AdmitIteration {
                foreach_run_id,
                index,
            } => Event::IterationAdmitted {
                foreach_run_id: foreach_run_id.clone(),
                index: *index,
            },
            Command::SelectBranch { selection } => Event::BranchSelected {
                selection: selection.clone(),
            },
            Command::CancelSiblings {
                parallel_run_id,
                step_run_ids,
            } => Event::SiblingsCancelled {
                parallel_run_id: parallel_run_id.clone(),
                step_run_ids: step_run_ids.clone(),
            },
            Command::CompleteStep {
                step_run_id,
                status,
                verdict,
            } => Event::StepCompleted {
                step_run_id: step_run_id.clone(),
                status: *status,
                verdict: *verdict,
            },
            Command::CompleteExecution { status, outputs } => Event::ExecutionCompleted {
                status: *status,
                outputs: outputs.clone(),
            },
            Command::ApplyData { change } => Event::DataChanged {
                change: change.clone(),
            },
            Command::SkipStep {
                step_id,
                reason,
                reads,
            } => Event::StepSkipped {
                step_id: step_id.clone(),
                step_run_id: format!("sr_{:026}", self.events.len()).parse().unwrap(),
                reason: *reason,
                reads: reads.clone(),
            },
            Command::ResolveAgentTarget {
                step_id,
                step_run_id,
                target,
            } => Event::AgentTargetResolved {
                step_id: step_id.clone(),
                step_run_id: step_run_id.clone(),
                target: target.clone(),
            },
            Command::MakeStepReady { step_id, reads } => Event::StepReady {
                reads: reads.clone(),
                step_id: step_id.clone(),
                step_run_id: format!("sr_{:026}", self.events.len()).parse().unwrap(),
            },
        };
        self.commands.push(command);
        let count = self.events.len();
        self.append(event, ActorRef::System, at_ms);
        assert_eq!(
            count + 1,
            self.events.len(),
            "invalid engine command: {:?}",
            self.error_codes
        );
    }
    pub fn settle(&mut self) {
        // Finite fixture guard: a broken decider must fail, never spin forever.
        for _ in 0..1000 {
            let commands = match decide(&self.state) {
                Ok(commands) => commands,
                Err(e) => {
                    self.error_codes.push(e.code().into());
                    return;
                }
            };
            assert_eq!(commands, decide(&self.state).unwrap());
            if commands.is_empty() {
                self.ready = self
                    .state
                    .steps
                    .iter()
                    .filter(|s| {
                        s.status == StepStatus::Ready
                            && self.state.node(&s.step_id).is_some_and(|n| {
                                matches!(n.kind, runbook_spec::ir::NodeKind::Task(_))
                            })
                    })
                    .map(|s| s.step_id.clone())
                    .collect();
                return;
            }
            for command in commands {
                self.commit_command(command, self.state.at_ms);
            }
        }
        panic!("fixture failed to quiesce");
    }
}
pub fn run_case(spec: &str, inputs: &[Input]) -> Trace {
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test}}\n{spec}"
    );
    run_document(&source, inputs, CompileContext::default())
}
pub fn run_document(source: &str, inputs: &[Input], directory: CompileContext) -> Trace {
    run_document_with_start(source, inputs, directory, StartContext::default())
}
pub fn run_document_with_start(
    source: &str,
    inputs: &[Input],
    directory: CompileContext,
    start: StartContext,
) -> Trace {
    let definition = Rc::new(compile(&parse(source).unwrap(), &directory).unwrap());
    let initial = ExecutionState::new(
        "exe_00000000000000000000000001".parse().unwrap(),
        definition,
    );
    let mut trace = Trace {
        state: initial.clone(),
        initial,
        directory,
        team_members: Default::default(),
        events: vec![],
        commands: vec![],
        ready: vec![],
        error_codes: ErrorCodes::default(),
    };
    match prepare_start(&trace.state, &start, 0) {
        Ok(event) => trace.append(event, ActorRef::System, 0),
        Err(e) => trace.error_codes.push(e.code().into()),
    }
    trace.settle();
    for input in inputs {
        match input {
            Input::Tick { at_ms } => {
                for _ in 0..1000 {
                    let commands = advance_time(&trace.state, *at_ms).unwrap();
                    if commands.is_empty() {
                        break;
                    }
                    for command in commands {
                        trace.commit_command(command, *at_ms);
                    }
                }
            }
            Input::Report {
                step,
                status,
                verdict,
                outputs,
                actor,
                at_ms,
            } => {
                let id = trace
                    .step(step)
                    .id
                    .clone()
                    .expect("report needs a materialized step");
                trace.append(
                    Event::StepReported {
                        step_run_id: id,
                        report: runbook_engine::WorkReport {
                            appended_logs: false,
                            files: vec![],
                            status: *status,
                            verdict: *verdict,
                            outputs: outputs.clone(),
                            summary: None,
                            evidence: vec![],
                            logs: vec![],
                        },
                    },
                    actor.clone(),
                    at_ms.unwrap_or(trace.state.at_ms),
                );
            }
        }
        trace.settle();
    }
    trace
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Input {
    Tick {
        at_ms: i64,
    },
    Report {
        step: String,
        status: ReportStatus,
        verdict: Option<Verdict>,
        outputs: Data,
        actor: ActorRef,
        #[serde(default)]
        at_ms: Option<i64>,
    },
}
pub fn succeeded(step: &str) -> Input {
    Input::Report {
        step: step.into(),
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::new(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        at_ms: None,
    }
}

pub fn send_signal(trace: &mut Trace, name: &str, id: &str, at_ms: i64) {
    let input = WaitInput::Signal {
        execution_id: trace.state.id.clone(),
        signal_id: id.into(),
        name: name.into(),
        payload: Default::default(),
        at_ms,
    };
    match advance_wait(&trace.state, &input) {
        Ok(commands) => {
            for command in commands {
                let Command::ConsumeSignal { signal } = &command else {
                    panic!("expected signal command")
                };
                trace.append(
                    Event::SignalReceived {
                        signal: signal.clone(),
                    },
                    ActorRef::Human {
                        user_id: "usr_00000000000000000000000001".parse().unwrap(),
                    },
                    at_ms,
                );
                trace.commands.push(command);
            }
        }
        Err(error) => trace.error_codes.push(error.code().into()),
    }
    trace.settle();
}

/// Stored-form replay: restore the pinned snapshot, round-trip each envelope
/// through JSON (byte-identical on re-encode, equal on decode) and fold.
pub fn replay_stored(
    compiled: &runbook_spec::CompiledRunbook,
    id: &runbook_model::ExecutionId,
    events: &[EventEnvelope],
) -> ExecutionState {
    let restored = runbook_spec::restore(compiled.snapshot_bytes()).expect("snapshot restores");
    let mut state = ExecutionState::new(id.clone(), Rc::new(restored));
    for (index, envelope) in events.iter().enumerate() {
        let bytes = serde_json::to_vec(envelope).expect("event encodes");
        let stored: EventEnvelope = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("event {index} does not decode: {e}"));
        assert_eq!(
            serde_json::to_vec(&stored).unwrap(),
            bytes,
            "event {index} ({}) re-encodes differently",
            envelope.event.name()
        );
        assert_eq!(&stored, envelope, "event {index} is lossy in stored form");
        state = apply(&state, &stored).unwrap_or_else(|e| {
            panic!(
                "stored event {index} ({}) is rejected on replay: {e}",
                envelope.event.name()
            )
        });
    }
    state
}

/// Rejection codes collected by the harness. A trace whose rejections were
/// never read fails when it is dropped, so a test cannot pass while silently
/// swallowing a rejected input. Any read (deref, comparison, serialization)
/// marks every code recorded so far as inspected.
#[derive(Default)]
pub struct ErrorCodes {
    codes: Vec<String>,
    seen: std::cell::Cell<usize>,
}
impl ErrorCodes {
    /// Recording a code is not a read.
    pub fn push(&mut self, code: String) {
        self.codes.push(code);
    }
    fn read(&self) -> &Vec<String> {
        self.seen.set(self.codes.len());
        &self.codes
    }
}
impl std::ops::Deref for ErrorCodes {
    type Target = Vec<String>;
    fn deref(&self) -> &Vec<String> {
        self.read()
    }
}
impl std::fmt::Debug for ErrorCodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.read().fmt(f)
    }
}
impl<const N: usize> PartialEq<[&str; N]> for ErrorCodes {
    fn eq(&self, other: &[&str; N]) -> bool {
        self.read() == other
    }
}
impl PartialEq<Vec<String>> for ErrorCodes {
    fn eq(&self, other: &Vec<String>) -> bool {
        self.read() == other
    }
}
impl serde::Serialize for ErrorCodes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.read().serialize(serializer)
    }
}
impl Drop for ErrorCodes {
    fn drop(&mut self) {
        let unread = &self.codes[self.seen.get()..];
        if !unread.is_empty() && !std::thread::panicking() {
            panic!("trace dropped with uninspected rejections: {unread:?}");
        }
    }
}

pub mod flagship;
