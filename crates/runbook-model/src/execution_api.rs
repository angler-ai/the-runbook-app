use crate::*;
/// Admission is separate from engine status: an admitted run may still await
/// durable activation and therefore have status=queued and no actor steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAdmissionState {
    Queued,
    Admitted,
    Released,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionAdmissionView {
    pub state: ExecutionAdmissionState,
    /// Store-clock admission time; the deadline starts here, not at activation.
    pub admitted_at_ms: Option<i64>,
}
pub const EXECUTION_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/executions",
        request: None,
        response: Some("RunSearchPage"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/executions",
        request: Some("StartRequest"),
        response: Some("ExecutionView"),
        status: 201,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/executions/{exe}",
        request: None,
        response: Some("ExecutionView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/executions/{exe}/report",
        request: None,
        response: Some("ExecutionDocumentView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/executions/{exe}/rerun",
        request: Some("RerunRequest"),
        response: Some("ExecutionView"),
        status: 201,
    },
    // LANG9b: a webhook trigger. The body is the caller's own payload.
    PublicRoute {
        method: "POST",
        path: TRIGGER_PATH,
        request: None,
        response: Some("TriggerResponse"),
        status: 201,
    },
];
/// LANG9b: a runbook's webhook trigger URL.
pub const TRIGGER_PATH: &str = "/v1/projects/{p}/runbooks/{rb}/triggers/{trigger}";

/// LANG9b: what a webhook trigger's call did. `started` with the run; or not
/// started, because `when` was false (`reason: "when"`); or the call was a
/// duplicate of one that already started this run (`duplicate`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerResponse {
    pub started: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub duplicate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunSearchEntry {
    /// The run's API address.
    #[serde(default)]
    pub url: String,
    /// The run page to share with people.
    #[serde(default)]
    pub web_url: String,
    pub id: ExecutionId,
    pub runbook_id: RunbookId,
    pub status: ExecutionStatus,
    pub title: String,
    pub tags: Vec<String>,
    pub fields: Data,
    pub created_by: String,
    pub created_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunSearchPage {
    pub items: Vec<RunSearchEntry>,
    pub next_cursor: Option<String>,
}

pub const STREAM_ROUTES: &[PublicRoute] = &[PublicRoute {
    method: "GET",
    path: "/v1/executions/{exe}/events",
    request: None,
    response: None,
    status: 200,
}];
