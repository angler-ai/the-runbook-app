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
    // Stage 2: Slack's signed form posts (404 without the Slack app).
    PublicRoute {
        method: "POST",
        path: "/v1/slack/interactions",
        request: None,
        response: None,
        status: 200,
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectSlackView {
    pub configured: bool,
    /// Post to the configured Slack destination when a run starts.
    #[serde(default)]
    pub notify_run_started: bool,
    /// SLACK1 stage 2: the channel the server's Slack app posts to, when
    /// the project uses one (channel IDs are not secrets; a webhook URL is
    /// never read back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Whether this server has a Slack app (bot token and signing secret),
    /// which channels and approval buttons need.
    #[serde(default)]
    pub app: bool,
}

/// Either a Slack incoming-webhook URL (stage 1: plain messages), or a
/// channel ID for the server's Slack app (stage 2: approve and reject
/// buttons). Omit both to change only the preference on a configured target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectSlackRequest {
    /// Change the run-start setting. Omit to retain the previous value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_run_started: Option<bool>,
    /// A Slack incoming-webhook URL (https://hooks.slack.com/…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A Slack channel ID (`C…` or `G…`, from the channel's details).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// `POST /v1/slack/interactions`: the hosts pass Slack's form body
/// verbatim with its signature headers, so the API checks the signature
/// over the exact bytes Slack signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackInteractionEnvelope {
    pub raw: String,
    pub timestamp: Option<String>,
    pub signature: Option<String>,
}
pub const SLACK_INTERACTIONS: &str = "/v1/slack/interactions";
