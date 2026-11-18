//! Pure event reduction and deterministic decisions. No effects or generated IDs.
//! This incremental kernel admits tasks, assignments, guards, switches and join-all
//! parallel branches and bounded foreach instances. Later Gate A tasks add controls and recovery
//! policies. Core must authenticate actor facts beforehand.
mod decider;
mod event;
mod reducer;
mod state;
mod status;
mod version;
pub use decider::decide;
pub use event::{
    Command, Event, EventEnvelope, QUIESCE_FAULT, RUN_SIZE_EXCEEDED, RuntimeFault, TerminalCause,
};
pub use reducer::apply;
pub use state::{ExecutionState, ParentLink, StepState, Supersession};
pub use status::reported_status;
pub use version::{ENGINE_VERSION, LEGACY_ENGINE_VERSION, legacy_engine_version};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    Rejected {
        code: &'static str,
        errors: Vec<runbook_model::FieldError>,
    },
    Invariant {
        context: &'static str,
    },
}
impl EngineError {
    pub(crate) fn new(code: &'static str) -> Self {
        Self::Rejected {
            code,
            errors: vec![],
        }
    }
    pub(crate) fn invariant(context: &'static str) -> Self {
        Self::Invariant { context }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Rejected { code, .. } => code,
            Self::Invariant { .. } => "engine.invariant",
        }
    }
    pub fn errors(&self) -> &[runbook_model::FieldError] {
        match self {
            Self::Rejected { errors, .. } => errors,
            Self::Invariant { .. } => &[],
        }
    }
}
impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invariant { context } => write!(f, "engine.invariant: {context}"),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for EngineError {}

mod amendments;
mod report;
pub use amendments::{OriginalStepRecord, current_record, validate_override};
pub use report::{
    ValidatedReport, WorkReport, rollup_verdict, validate_report, validate_report_with_files,
    validate_report_with_logs,
};

mod data;
mod trigger;
pub use data::{
    DataChanged, ExecutionOutputs, InitialData, MAX_OUTPUTS_BYTES, OUTPUTS_TOO_LARGE, StartContext,
    evaluate_assignments, evaluate_concurrency_key, evaluate_execution_title, evaluate_outputs,
    outputs_bytes, prepare_start,
};
pub use trigger::{WebhookError, WebhookOutcome, webhook_start};

mod branch;
pub use event::SkipReason;

mod tree;
pub use branch::{BranchChoice, BranchSelected, choose_branch};

mod parallel;
pub use parallel::advance_parallel;

mod foreach;
pub use foreach::{ForeachSnapshot, IterationRef, advance_foreach};

mod loop_node;
pub use loop_node::{LoopCheck, LoopState, LoopTimer, advance_loop, due_timers};

mod failure;
pub use failure::{CatchSelection, GroupFinallyState, failure_type, route_failure};

mod retry;
pub use retry::{AttemptRecord, FailureAction, RetryTimer, failure_action};

mod handoff;
pub use handoff::{BlockedNotification, HandoffRequest, HandoffSource, ResolvedHandoff};

mod assignment;
pub use assignment::{AssigneeRequest, ResolvedAssignee, TakenOver, agents_may_claim};

mod approval;
pub use approval::{
    ApprovalDecision, ApprovalSignature, ApprovalState, ResolvedVote, VoteEligibility, apply_vote,
};

mod wait;
pub use wait::{SignalDelivery, WaitInput, WaitStart, WaitState, WaitTarget, advance_wait};

mod call;
pub use call::{CallOutcome, CallRequest, CallStart, CallState, awaits_outcome, awaits_start};

mod deadline;
mod followup;
/// Whether the run is open (running or paused) and can still change.
pub fn deadline_open(state: &ExecutionState) -> bool {
    deadline::open(state)
}
pub use deadline::{
    ApprovalNotice, ApprovalNoticeKind, EscalationRequest, EscalationSnapshot, PauseReason,
    advance_time, authorize_at_deadline, next_wakeup,
};

mod automatic;
pub use automatic::{
    AutomaticDecision, DecisionAuthor, DecisionPolicy, TimeoutPolicy, apply_automatic_decision,
};

mod cleanup;
pub use cleanup::{
    CleanupRoot, CleanupState, ExecutionLimits, ExecutionPhase, begin_cleanup, run_ceiling_applies,
};

mod end_node;
pub use end_node::{EndSelected, prepare_end};

mod presentation;
pub use presentation::{
    RenderedMarkdown, RenderedStep, approval_prompt, render_report, render_step, step_template,
    step_title,
};

mod checklist;
pub use checklist::update_checklist;

mod notification;
pub use notification::{NotificationIntent, notification_for, notifications_for};
mod work_notifications;
pub use work_notifications::{
    AgentAvailability, ApprovalRequest, ApprovalRequestSnapshot, WorkReason,
};

mod agent_approval;
pub use agent_approval::{AgentDecision, AppliedDecision, apply_agent_decision};
