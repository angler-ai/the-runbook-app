//! F46: a step run's log lines, paged by sequence (D33).
use crate::PublicRoute;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `GET /v1/step-runs/{sr}/logs?cursor=<seq>&limit=<n>`.
pub const STEP_LOG_ROUTES: &[PublicRoute] = &[PublicRoute {
    method: "GET",
    path: "/v1/step-runs/{sr}/logs",
    request: None,
    response: Some("StepLogPage"),
    status: 200,
}];

/// One persisted (already redacted) log line and its sequence, from 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepLogLine {
    pub seq: u64,
    pub line: String,
}

/// Lines after the request cursor in sequence order. `next_cursor` is the
/// last sequence read (the request cursor when no line followed it), so a
/// reader can keep polling a running step; `has_more` says another page is
/// already available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepLogPage {
    pub lines: Vec<StepLogLine>,
    pub next_cursor: String,
    pub has_more: bool,
}
