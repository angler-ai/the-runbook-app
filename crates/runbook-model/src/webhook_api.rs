//! C1 webhook subscription wire types. Signing secrets appear only in the
//! one-time create and rotate responses; every other view omits them.
use crate::*;

domain_id!(WebhookId, "whk_", "^whk_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(DeliveryId, "whd_", "^whd_[0-7][0-9A-HJKMNP-TV-Z]{25}$");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWebhookRequest {
    pub name: String,
    /// HTTPS endpoint. Self-hosted operators may allow http and private targets.
    pub url: String,
    /// Catalog event types, or `["*"]` for every event type.
    pub events: Vec<String>,
    /// Only events of these runbooks; empty means every runbook.
    #[serde(default)]
    pub runbooks: Vec<RunbookId>,
    /// Only executions carrying at least one of these tags; empty means any.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
}
fn enabled_default() -> bool {
    true
}

/// Omitted fields are unchanged. The secret changes only through rotate-secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateWebhookRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub events: Option<Vec<String>>,
    pub runbooks: Option<Vec<RunbookId>>,
    pub tags: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebhookView {
    pub id: WebhookId,
    pub project_id: ProjectId,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub runbooks: Vec<RunbookId>,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub secret_rotated_at_ms: i64,
    /// While set, deliveries also carry a signature from the previous secret.
    pub previous_secret_expires_at_ms: Option<i64>,
    /// Start of the current run of failed attempts; cleared by a success.
    pub failing_since_ms: Option<i64>,
    /// Why the system disabled this subscription (`delivery_failing`).
    pub disabled_reason: Option<String>,
    pub disabled_at_ms: Option<i64>,
    /// Set when the system disabled this subscription; stays until an admin
    /// re-enables or deletes it.
    #[serde(default)]
    pub admin_notice: Option<WebhookNotice>,
}

/// A durable notice for project admins (PRD section 9: a subscription that
/// failed continuously for three days is disabled and admins are notified).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebhookNotice {
    /// `webhook.disabled`.
    pub kind: String,
    pub message: String,
    pub at_ms: i64,
    /// Start of the failing streak that disabled the subscription.
    pub failing_since_ms: i64,
    /// How the last attempt failed (`http_500`, `timeout`, …).
    pub last_failure: Option<String>,
}

/// Create and rotate responses: the only time the raw secret is shown.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebhookSecretView {
    pub webhook: WebhookView,
    /// `whsec_` signing secret, returned exactly once.
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebhookPage {
    pub items: Vec<WebhookView>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivered,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Delivered,
    Retry,
    Failed,
}

/// One delivery attempt. Never contains request headers, signatures or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryAttemptView {
    pub number: u32,
    pub at_ms: i64,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub outcome: AttemptOutcome,
    pub error: Option<String>,
    /// First bytes of the receiver's response, redacted.
    pub response_snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryView {
    pub id: DeliveryId,
    pub webhook_id: WebhookId,
    pub event_id: String,
    pub event_type: String,
    pub execution_id: Option<ExecutionId>,
    pub state: DeliveryState,
    pub created_at_ms: i64,
    pub next_attempt_at_ms: Option<i64>,
    pub redelivery_of: Option<DeliveryId>,
    pub failure: Option<String>,
    pub attempts: Vec<DeliveryAttemptView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryPage {
    pub items: Vec<DeliveryView>,
    pub next_cursor: Option<String>,
}

/// A human event (`approval.requested|escalated|expired`, `step.blocked`)
/// that reached no subscriber: none matched, or every delivery failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UndeliveredNotification {
    pub event_id: String,
    pub event_type: String,
    pub step_run_id: Option<StepRunId>,
    /// `no_subscriber` or `delivery_failed`.
    pub reason: String,
}

pub const WEBHOOK_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/webhooks",
        request: None,
        response: Some("WebhookPage"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/webhooks",
        request: Some("CreateWebhookRequest"),
        response: Some("WebhookSecretView"),
        status: 201,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/webhooks/{id}",
        request: None,
        response: Some("WebhookView"),
        status: 200,
    },
    PublicRoute {
        method: "PATCH",
        path: "/v1/projects/{p}/webhooks/{id}",
        request: Some("UpdateWebhookRequest"),
        response: Some("WebhookView"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/projects/{p}/webhooks/{id}",
        request: None,
        response: None,
        status: 204,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/webhooks/{id}/test",
        request: None,
        response: Some("DeliveryView"),
        status: 202,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/webhooks/{id}/deliveries",
        request: None,
        response: Some("DeliveryPage"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/webhooks/{id}/deliveries/{d}/redeliver",
        request: None,
        response: Some("DeliveryView"),
        status: 202,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/webhooks/{id}/rotate-secret",
        request: None,
        response: Some("WebhookSecretView"),
        status: 200,
    },
];
