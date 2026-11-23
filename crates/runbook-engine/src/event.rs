use runbook_model::{
    ActorRef, ChecklistTick, ExecutionId, ExecutionStatus, MemoryId, MemoryReadRef, StepRunId,
};
use serde::{Deserialize, Serialize};

/// Time and actor are persisted facts supplied by the trusted core, never sampled
/// or authenticated inside the engine. Sequence is local to this execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub execution_id: ExecutionId,
    pub seq: u64,
    pub at_ms: i64,
    pub actor: ActorRef,
    pub event: Event,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    ApprovalRequested {
        snapshot: crate::ApprovalRequestSnapshot,
    },
    AgentWorkAvailable {
        work: crate::AgentAvailability,
    },
    EndSelected {
        selection: crate::EndSelected,
    },
    StepLimitExceeded {
        step_id: String,
    },
    GroupFinallyStarted {
        group_run_id: StepRunId,
        outcome: crate::GroupFinallyState,
    },
    CleanupStarted {
        cause: TerminalCause,
        reason: Option<String>,
    },
    CleanupRootCompleted {
        root: crate::CleanupRoot,
    },
    CleanupSuppressed {
        reason: String,
    },
    CancellationRequested {
        /// LANG3: the run that replaced this one (`on_conflict:
        /// cancel_previous`). Omitted otherwise, so older logs keep their
        /// bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        superseded_by: Option<ExecutionId>,
        /// The principal who started that run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_by: Option<String>,
    },
    ExecutionCancelledBeforeStart {
        data: Box<crate::InitialData>,
        /// As on `CancellationRequested` (a waiter evicted by
        /// `cancel_previous`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        superseded_by: Option<ExecutionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_by: Option<String>,
    },
    ExecutionMetadataUpdated {
        title: Option<String>,
        tags: Option<Vec<String>>,
        #[serde(with = "runbook_spec::stored_data")]
        fields: runbook_model::Data,
    },
    DecisionApplied {
        decision: crate::AppliedDecision,
    },
    ManualPauseChanged {
        paused: bool,
    },
    ExecutionDeadlineElapsed {
        deadline_at_ms: i64,
    },
    TaskDeadlineElapsed {
        step_run_id: StepRunId,
        deadline_at_ms: i64,
    },
    ApprovalEscalated {
        snapshot: crate::EscalationSnapshot,
    },
    /// An unfinished step reached its deadline relative to the run's start.
    StepOverdue {
        step_id: String,
        due_at_ms: i64,
    },
    /// LANG9: a reminder for a step left open, due at `due_at_ms`.
    StepReminded {
        step_run_id: StepRunId,
        due_at_ms: i64,
    },
    /// LANG9: a task's or wait's follow-up escalation, with its recipients
    /// as the directory resolved them.
    StepEscalated {
        snapshot: crate::EscalationSnapshot,
    },
    ApprovalExpired {
        step_run_id: StepRunId,
        deadline_at_ms: i64,
    },
    ApprovalNotificationQueued {
        notice: crate::ApprovalNotice,
    },
    /// An `on_timeout: decide` gate reached its deadline, but the automatic
    /// decision was refused when it was made (for example a policy rule now
    /// rejects it). The gate expires undecided and `on_expired` applies.
    AutomaticDecisionWithheld {
        step_run_id: StepRunId,
        deadline_at_ms: i64,
        reason: String,
    },
    SignalReceived {
        signal: Box<crate::SignalDelivery>,
    },
    /// LANG4: a call step asked for its child execution.
    CallRequested {
        start: Box<crate::CallStart>,
    },
    /// LANG4: the child's start committed.
    CallStarted {
        step_run_id: StepRunId,
        child_execution_id: ExecutionId,
    },
    /// LANG4: the child finished, or its start was refused.
    CallCompleted {
        outcome: Box<crate::CallOutcome>,
    },
    WaitStarted {
        start: crate::WaitStart,
    },
    ApprovalDecided {
        decision: crate::ApprovalDecision,
    },
    ApprovalVoted {
        vote: Box<crate::ResolvedVote>,
    },
    StepHandedOff {
        handoff: Box<crate::ResolvedHandoff>,
    },
    /// LANG7: the host resolved a strict task's assignee for its attempt.
    StepAssigneeResolved {
        resolution: Box<crate::ResolvedAssignee>,
    },
    /// LANG7: the person who just claimed an `actor: agent` task took it
    /// over; recorded right after its `StepClaimed`.
    StepTakenOver {
        step_run_id: StepRunId,
        previous_actor_kind: runbook_model::ActorKind,
    },
    StepBlockedNotificationQueued {
        notification: crate::BlockedNotification,
    },
    RetryScheduled {
        step_run_id: StepRunId,
        attempt: u32,
        due_at_ms: i64,
    },
    CatchActivated {
        selection: crate::CatchSelection,
    },
    TimerFired {
        step_run_id: StepRunId,
        generation: u32,
    },
    LoopIterationStarted {
        loop_run_id: StepRunId,
        index: u32,
    },
    LoopChecked {
        step_run_id: StepRunId,
        check: crate::LoopCheck,
    },
    ExecutionFaulted {
        fault: RuntimeFault,
    },
    ForeachStarted {
        step_id: String,
        step_run_id: StepRunId,
        snapshot: Box<crate::ForeachSnapshot>,
    },
    IterationAdmitted {
        foreach_run_id: StepRunId,
        index: u32,
    },
    ExecutionStarted {
        data: Box<crate::InitialData>,
        /// Engine semantics this run started under (F95); absent on logs
        /// recorded before the marker existed.
        #[serde(default = "crate::legacy_engine_version")]
        engine_version: u32,
    },
    BranchSelected {
        selection: crate::BranchSelected,
    },
    SiblingsCancelled {
        parallel_run_id: StepRunId,
        step_run_ids: Vec<StepRunId>,
    },
    StepCompleted {
        step_run_id: StepRunId,
        status: runbook_model::StepStatus,
        verdict: Option<runbook_model::Verdict>,
    },
    DataChanged {
        change: crate::DataChanged,
    },
    ChecklistUpdated {
        step_run_id: StepRunId,
        ticks: Vec<ChecklistTick>,
    },
    MemoryRead {
        step_run_id: StepRunId,
        refs: Vec<MemoryReadRef>,
    },
    MemoryProposed {
        step_run_id: StepRunId,
        memory_id: MemoryId,
    },
    /// MEM6: the step run saved a live memory entry (no review).
    MemorySaved {
        step_run_id: StepRunId,
        memory_id: MemoryId,
    },
    /// MEM6 review fix 2: the step run edits a memory entry. Recorded
    /// before the catalog write, so it counts toward the run's 20 memory
    /// writes even when two edits race.
    MemoryEdited {
        step_run_id: StepRunId,
        memory_id: MemoryId,
    },
    ExecutionCompleted {
        status: ExecutionStatus,
        outputs: crate::ExecutionOutputs,
    },
    /// An actor claimed a ready step run; claim fencing belongs to core.
    StepClaimed {
        step_run_id: StepRunId,
        /// R0-4: the runner agent version pinned by a runner's claim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_version_id: Option<String>,
        /// Stable claim ID for runner ledger reconciliation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_id: Option<runbook_model::ClaimId>,
    },
    /// R0-4: the agent a ready task targets, as its version resolved it.
    AgentTargetResolved {
        step_id: String,
        step_run_id: StepRunId,
        target: crate::AgentTargetRef,
    },
    /// R0-4: why a ready runner step is not being worked.
    StepWaiting {
        step_run_id: StepRunId,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// R0-4: the core granted a credential's release to the step's claim
    /// (server-observed, with its delivery mode). Never a secret value.
    CredentialReleased {
        step_run_id: StepRunId,
        credential_id: String,
        credential_version: u64,
        delivery: String,
        runner_id: String,
        /// The claim the release was granted to (review L11: one release
        /// per claim, credential, version and delivery; renewals add
        /// none). Absent on releases recorded before it existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_id: Option<runbook_model::ClaimId>,
    },
    /// R0-4: a released credential was revoked.
    CredentialRevoked {
        step_run_id: StepRunId,
        credential_id: String,
        reason: String,
    },
    /// R0-6: what a step's report said it cost, recorded with the report.
    DecisionLeaseClaimed {
        step_run_id: StepRunId,
        lease: runbook_model::DecisionLeaseView,
        agent_version_id: String,
    },
    DecisionLeaseEnded {
        step_run_id: StepRunId,
        lease_id: runbook_model::ClaimId,
        reason: String,
    },
    ApprovalAgentAbstained {
        step_run_id: StepRunId,
        note: runbook_model::ApprovalAgentNote,
    },
    StepCostReported {
        step_run_id: StepRunId,
        cost: crate::RecordedCost,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_id: Option<runbook_model::ClaimId>,
    },
    /// The claim owner released the step run back to ready.
    StepClaimReleased {
        step_run_id: StepRunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The claim lease expired before completion; the step run is ready again.
    StepClaimExpired {
        step_run_id: StepRunId,
    },
    /// Audit facts for execution credential families; secrets never appear.
    ExecutionCredentialsIssued {
        family_id: String,
        generation: u64,
        issuer: String,
    },
    ExecutionCredentialsRotated {
        family_id: String,
        generation: u64,
    },
    ExecutionTokenReuseDetected {
        family_id: String,
    },
    ExecutionCredentialsRevoked {
        family_id: String,
    },
    /// A finished step's record was amended; the original is preserved
    /// on the step and evaluated consumers become stale.
    StepOverridden {
        step_run_id: StepRunId,
        amendment: Box<runbook_model::Amendment>,
    },
    /// Already authenticated work report; claim validation belongs to core.
    StepReported {
        step_run_id: StepRunId,
        report: crate::WorkReport,
    },
    StepReady {
        step_id: String,
        step_run_id: StepRunId,
        #[serde(default)]
        reads: Vec<runbook_expr::ReadRef>,
    },
    StepSkipped {
        step_id: String,
        step_run_id: StepRunId,
        reason: SkipReason,
        reads: Vec<runbook_expr::ReadRef>,
    },
}
impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GroupFinallyStarted { .. } => "GroupFinallyStarted",
            Self::ApprovalRequested { .. } => "ApprovalRequested",
            Self::AgentWorkAvailable { .. } => "AgentWorkAvailable",
            Self::EndSelected { .. } => "EndSelected",
            Self::StepLimitExceeded { .. } => "StepLimitExceeded",
            Self::CleanupStarted { .. } => "CleanupStarted",
            Self::CleanupRootCompleted { .. } => "CleanupRootCompleted",
            Self::CleanupSuppressed { .. } => "CleanupSuppressed",
            Self::CancellationRequested { .. } => "CancellationRequested",
            Self::ExecutionCancelledBeforeStart { .. } => "ExecutionCancelledBeforeStart",
            Self::ExecutionMetadataUpdated { .. } => "ExecutionMetadataUpdated",
            Self::DecisionApplied { .. } => "DecisionApplied",
            Self::ManualPauseChanged { .. } => "ManualPauseChanged",
            Self::ExecutionDeadlineElapsed { .. } => "ExecutionDeadlineElapsed",
            Self::TaskDeadlineElapsed { .. } => "TaskDeadlineElapsed",
            Self::ApprovalEscalated { .. } => "ApprovalEscalated",
            Self::StepOverdue { .. } => "StepOverdue",
            Self::StepReminded { .. } => "StepReminded",
            Self::StepEscalated { .. } => "StepEscalated",
            Self::ApprovalExpired { .. } => "ApprovalExpired",
            Self::ApprovalNotificationQueued { .. } => "ApprovalNotificationQueued",
            Self::AutomaticDecisionWithheld { .. } => "AutomaticDecisionWithheld",
            Self::SignalReceived { .. } => "SignalReceived",
            Self::CallRequested { .. } => "CallRequested",
            Self::CallStarted { .. } => "CallStarted",
            Self::CallCompleted { .. } => "CallCompleted",
            Self::WaitStarted { .. } => "WaitStarted",
            Self::ApprovalDecided { .. } => "ApprovalDecided",
            Self::ApprovalVoted { .. } => "ApprovalVoted",
            Self::StepHandedOff { .. } => "StepHandedOff",
            Self::StepAssigneeResolved { .. } => "StepAssigneeResolved",
            Self::StepTakenOver { .. } => "StepTakenOver",
            Self::StepBlockedNotificationQueued { .. } => "StepBlockedNotificationQueued",
            Self::RetryScheduled { .. } => "RetryScheduled",
            Self::CatchActivated { .. } => "CatchActivated",
            Self::TimerFired { .. } => "TimerFired",
            Self::LoopIterationStarted { .. } => "LoopIterationStarted",
            Self::LoopChecked { .. } => "LoopChecked",
            Self::ExecutionFaulted { .. } => "ExecutionFaulted",
            Self::ForeachStarted { .. } => "ForeachStarted",
            Self::IterationAdmitted { .. } => "IterationAdmitted",
            Self::ExecutionStarted { .. } => "ExecutionStarted",
            Self::DataChanged { .. } => "DataChanged",
            Self::BranchSelected { .. } => "BranchSelected",
            Self::StepCompleted { .. } => "StepCompleted",
            Self::SiblingsCancelled { .. } => "SiblingsCancelled",
            Self::ChecklistUpdated { .. } => "ChecklistUpdated",
            Self::MemoryRead { .. } => "MemoryRead",
            Self::MemoryProposed { .. } => "MemoryProposed",
            Self::MemorySaved { .. } => "MemorySaved",
            Self::MemoryEdited { .. } => "MemoryEdited",
            Self::ExecutionCompleted { .. } => "ExecutionCompleted",
            Self::StepClaimed { .. } => "StepClaimed",
            Self::StepClaimReleased { .. } => "StepClaimReleased",
            Self::DecisionLeaseClaimed { .. } => "DecisionLeaseClaimed",
            Self::DecisionLeaseEnded { .. } => "DecisionLeaseEnded",
            Self::ApprovalAgentAbstained { .. } => "ApprovalAgentAbstained",
            Self::StepClaimExpired { .. } => "StepClaimExpired",
            Self::ExecutionCredentialsIssued { .. } => "ExecutionCredentialsIssued",
            Self::ExecutionCredentialsRotated { .. } => "ExecutionCredentialsRotated",
            Self::ExecutionTokenReuseDetected { .. } => "ExecutionTokenReuseDetected",
            Self::ExecutionCredentialsRevoked { .. } => "ExecutionCredentialsRevoked",
            Self::StepOverridden { .. } => "StepOverridden",
            Self::StepReported { .. } => "StepReported",
            Self::StepReady { .. } => "StepReady",
            Self::StepSkipped { .. } => "StepSkipped",
            Self::AgentTargetResolved { .. } => "AgentTargetResolved",
            Self::StepWaiting { .. } => "StepWaiting",
            Self::CredentialReleased { .. } => "CredentialReleased",
            Self::CredentialRevoked { .. } => "CredentialRevoked",
            Self::StepCostReported { .. } => "StepCostReported",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    ResolveApprovalRequest {
        request: crate::ApprovalRequest,
    },
    NotifyAgentWork {
        work: crate::AgentAvailability,
    },
    SelectEnd {
        selection: crate::EndSelected,
    },
    RecordStepLimit {
        step_id: String,
    },
    BeginGroupFinally {
        group_run_id: StepRunId,
        outcome: crate::GroupFinallyState,
    },
    BeginCleanup {
        cause: TerminalCause,
        reason: Option<String>,
    },
    CompleteCleanupRoot {
        root: crate::CleanupRoot,
    },
    SuppressCleanup {
        reason: String,
    },
    ApplyAutomaticDecision {
        policy: crate::TimeoutPolicy,
    },
    ExpireExecution {
        deadline_at_ms: i64,
    },
    ExpireTask {
        step_run_id: StepRunId,
        deadline_at_ms: i64,
    },
    ResolveEscalation {
        request: crate::EscalationRequest,
    },
    /// Record a missed run-relative deadline on a step instance.
    MarkStepOverdue {
        step_id: String,
        due_at_ms: i64,
    },
    /// LANG9: send the reminder due at `due_at_ms`.
    RemindStep {
        step_run_id: StepRunId,
        due_at_ms: i64,
    },
    /// LANG9: resolve a step's follow-up escalation recipients.
    ResolveStepEscalation {
        request: crate::EscalationRequest,
    },
    ExpireApproval {
        step_run_id: StepRunId,
        deadline_at_ms: i64,
    },
    NotifyApproval {
        notice: crate::ApprovalNotice,
    },
    ConsumeSignal {
        signal: Box<crate::SignalDelivery>,
    },
    StartWait {
        start: crate::WaitStart,
    },
    /// LANG4: the host supplies the child's execution ID and records
    /// `CallRequested`.
    RequestCall {
        request: crate::CallRequest,
    },
    FireWaitTimer {
        step_run_id: StepRunId,
        generation: u32,
    },
    DecideApproval {
        decision: crate::ApprovalDecision,
    },
    ResolveHandoff {
        request: Box<crate::HandoffRequest>,
    },
    /// LANG7: resolve a strict task's assignee against the directory.
    ResolveAssignee {
        request: Box<crate::AssigneeRequest>,
    },
    NotifyStepBlocked {
        notification: crate::BlockedNotification,
    },
    ScheduleRetry {
        step_run_id: StepRunId,
        attempt: u32,
        due_at_ms: i64,
    },
    ActivateCatch {
        selection: crate::CatchSelection,
    },
    StartLoopIteration {
        loop_run_id: StepRunId,
        index: u32,
    },
    RecordLoopCheck {
        step_run_id: StepRunId,
        check: crate::LoopCheck,
    },
    RecordFault {
        fault: RuntimeFault,
    },
    StartForeach {
        step_id: String,
        snapshot: Box<crate::ForeachSnapshot>,
    },
    AdmitIteration {
        foreach_run_id: StepRunId,
        index: u32,
    },
    SelectBranch {
        selection: crate::BranchSelected,
    },
    CancelSiblings {
        parallel_run_id: StepRunId,
        step_run_ids: Vec<StepRunId>,
    },
    CompleteStep {
        step_run_id: StepRunId,
        status: runbook_model::StepStatus,
        verdict: Option<runbook_model::Verdict>,
    },
    CompleteExecution {
        status: ExecutionStatus,
        outputs: crate::ExecutionOutputs,
    },
    ApplyData {
        change: crate::DataChanged,
    },
    /// The store supplies the run ID when it persists StepReady. Reissuing this
    /// intent before a commit is harmless under the expected-sequence contract.
    MakeStepReady {
        step_id: String,
        reads: Vec<runbook_expr::ReadRef>,
    },
    SkipStep {
        step_id: String,
        reason: SkipReason,
        reads: Vec<runbook_expr::ReadRef>,
    },
    /// R0-4: record a ready task's agent target.
    ResolveAgentTarget {
        step_id: String,
        step_run_id: StepRunId,
        target: crate::AgentTargetRef,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Condition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeFault {
    pub step_id: String,
    pub code: String,
    /// The faulted step run, for faults on a started step (a wait beyond
    /// the PRD §17 cap, engine version 4). Absent on older faults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_run_id: Option<StepRunId>,
}
/// Fault code a host records when a run does not quiesce within its settle
/// bound (F16); it names no step.
pub const QUIESCE_FAULT: &str = "engine.failed_to_quiesce";
/// Fault code a host records when an open run's event log passes its hard
/// ceiling (engine version 6, CF13b); it names no step. The run closes
/// failed without its cleanup.
pub const RUN_SIZE_EXCEEDED: &str = "limit.run_size_exceeded";
impl RuntimeFault {
    /// The run-level fault for a run whose log passed its ceiling.
    pub fn run_size_exceeded() -> Self {
        Self {
            step_id: String::new(),
            code: RUN_SIZE_EXCEEDED.into(),
            step_run_id: None,
        }
    }
    /// The run-level fault for a run that cannot quiesce.
    pub fn failed_to_quiesce() -> Self {
        Self {
            step_id: String::new(),
            code: QUIESCE_FAULT.into(),
            step_run_id: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalCause {
    Succeeded,
    Failed,
    Cancelled,
}
