//! One shared view for the live execution document and its JSON report.
use crate::{
    ActorKind, ActorRef, Amendment, ApprovalView, Data, ExecutionView, MemoryId, MemoryReadRef,
    StepFollowupView, StepRunId, StepStatus, Verdict,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionDocumentView {
    pub header: ExecutionView,
    /// In execution step-path order, including iteration/attempt paths; not a map.
    pub sections: Vec<StepSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepSection {
    pub step_run_id: StepRunId,
    pub path: String,
    pub title: String,
    pub status: StepStatus,
    pub verdict: Option<Verdict>,
    pub actor: Option<ActorRef>,
    pub summary: Option<String>,
    pub outputs: Data,
    pub evidence: Vec<String>,
    /// Includes pending actions, human signatures or an explicit system-policy decision.
    pub approval: Option<ApprovalView>,
    pub memory_read: Vec<MemoryReadRef>,
    /// Memory this step proposed (D40), in proposal order. Omitted when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_proposed: Vec<MemoryProposedRef>,
    /// Live memory this step saved (MEM6), in save order. Omitted when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_saved: Vec<MemoryProposedRef>,
    /// Memory this step edited (MEM6), in edit order. Omitted when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_edited: Vec<MemoryProposedRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub amended: bool,
    pub amendments: Vec<Amendment>,
    pub stale: bool,
    /// LANG4: a `call` step's child execution. Omitted for other steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<crate::StepCallView>,
    /// LANG7: a task's assignee, and whether it is strict. Omitted for
    /// steps without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<StepAssigneeView>,
    /// LANG7: the person who took over this agent task. Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_over: Option<TakenOverView>,
    /// LANG7: a task's declared `actor` (`agent` tasks are what a person
    /// may take over). Omitted for other steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_actor: Option<TaskActor>,
    /// LANG9: the step's labels from its definition (`audience: customer`).
    /// Omitted when the step has none.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
    /// LANG9: what kind of failure a failed step had (`timeout`, `failed`,
    /// `call_failed`), as `retry.on` and `catch_on` see it. Omitted unless
    /// the step failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_type: Option<String>,
    /// LANG9: reminders sent for this step's current attempt and its
    /// follow-up escalation, once either has happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followup: Option<StepFollowupView>,
    /// Log bodies are fetched separately; large logs do not inflate this document.
    pub logs: PagedLogRef,
}

/// LANG7: a task's assignee as the run shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepAssigneeView {
    /// `user:<name>` or `team:<name>`: as written, or what an expression
    /// assignee resolved to. Omitted while an expression is unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// True for `assignment: strict` (only the assignee may claim and
    /// complete the step); false when the assignee is guidance.
    pub strict: bool,
    /// `handoff.no_recipient` when a strict assignee resolved to nobody
    /// eligible (the project admins were notified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_error: Option<String>,
    /// A strict assignee as resolved for the current attempt (user and
    /// team IDs): who may claim. Empty for guidance or while unresolved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<crate::RecipientRef>,
}

/// LANG7: a task's declared actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskActor {
    Human,
    Agent,
    Any,
}

/// LANG7: a person took over an agent task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TakenOverView {
    pub by: ActorRef,
    pub at_ms: i64,
    pub previous_actor_kind: ActorKind,
}

/// One proposal (or, on `memory_saved`, one live save) a step made.
/// `dropped` is the reason code when the catalog could not store it (for
/// example `memory.limit_reached`: on Cloudflare a write that raced a full
/// quota is kept only as a marker).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryProposedRef {
    pub memory_id: MemoryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PagedLogRef {
    pub href: String,
    pub next_cursor: Option<String>,
}
