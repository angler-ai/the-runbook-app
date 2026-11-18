//! SLACK1 stage 1 wire shapes. The webhook URL is write-only: reads
//! say only whether one is configured.
use crate::PublicRoute;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SLACK_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/slack",
        request: None,
        response: Some("ProjectSlackView"),
        status: 200,
    },
    PublicRoute {
        method: "PUT",
        path: "/v1/projects/{p}/slack",
        request: Some("ProjectSlackRequest"),
        response: Some("ProjectSlackView"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/projects/{p}/slack",
        request: None,
        response: None,
        status: 204,
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectSlackView {
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectSlackRequest {
    /// A Slack incoming-webhook URL (https://hooks.slack.com/…).
    pub url: String,
}
