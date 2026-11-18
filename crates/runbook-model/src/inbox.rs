//! Cross-project human inbox (B8f). Summaries disclose no step instructions.
use crate::{ExecutionId, ProjectId, PublicRoute, StepRunId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InboxTaskKind {
    /// Includes checklists and work handed off to a human.
    HumanTask,
    Approval,
}

/// B8f: the caller's cross-project inbox.
pub const INBOX_ROUTES: &[PublicRoute] = &[PublicRoute {
    method: "GET",
    path: "/v1/me/tasks",
    request: None,
    response: Some("InboxPage"),
    status: 200,
}];

/// One open step assigned to the caller or a team they belong to. Multiple
/// matching assignments produce one item, identified by step_run_id. Assignment
/// visibility does not grant claim or approval authority; mutations reauthorize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxTaskView {
    pub project_id: ProjectId,
    pub execution_id: ExecutionId,
    pub step_run_id: StepRunId,
    pub path: String,
    pub title: String,
    /// The execution's title (B1); `title` stays the step title.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub execution_title: String,
    pub kind: InboxTaskKind,
    /// UI-2: when this task last changed state (became ready, was handed
    /// off or re-assigned), from its assignment's update time (epoch ms).
    /// Absent when the store has no time for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxPage {
    pub items: Vec<InboxTaskView>,
    pub next_cursor: Option<String>,
}
