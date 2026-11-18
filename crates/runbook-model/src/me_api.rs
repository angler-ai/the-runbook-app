//! Signed-in identity views (B1): who the presenting credential is.
use crate::{OrganizationId, ProjectId, PublicRoute, TeamId, UserId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const ME_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/me",
        request: None,
        response: Some("MeView"),
        status: 200,
    },
    PublicRoute {
        method: "PATCH",
        path: "/v1/me",
        request: Some("UpdateMeRequest"),
        response: Some("MeUser"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/me/token",
        request: None,
        response: None,
        status: 204,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/me/apps",
        request: None,
        response: Some("ConnectedAppPage"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/me/apps/{client}",
        request: None,
        response: None,
        status: 204,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/me/email-preferences",
        request: None,
        response: Some("EmailPreferencesView"),
        status: 200,
    },
    PublicRoute {
        method: "PUT",
        path: "/v1/me/email-preferences",
        request: Some("EmailPreferencesRequest"),
        response: Some("EmailPreferencesView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/unsubscribe",
        request: Some("UnsubscribeRequest"),
        response: Some("UnsubscribeView"),
        status: 200,
    },
];

/// MCP2: an OAuth client (a remote MCP client) the person has granted
/// access, from the auth service. The name is the client's own claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectedAppView {
    pub client_id: String,
    /// Self-registered and unverified.
    #[serde(default)]
    pub client_name: Option<String>,
    /// Where the client's redirect URIs point (`127.0.0.1:<port> (an app
    /// on this computer)` for a loopback client).
    pub redirect_hosts: Vec<String>,
    pub scopes: Vec<String>,
    pub created_at_ms: i64,
    /// When the client last received an access token.
    #[serde(default)]
    pub last_used_at_ms: Option<i64>,
    /// The grant's absolute end (90 days after the last consent).
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectedAppPage {
    pub items: Vec<ConnectedAppView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MeView {
    /// Absent for service tokens, which act for a project, not a person.
    #[serde(default)]
    pub user: Option<MeUser>,
    pub org: MeOrg,
    #[serde(default)]
    pub teams: Vec<MeTeam>,
    pub credential: MeCredential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MeUser {
    pub id: UserId,
    pub name: String,
}

/// `PATCH /v1/me`: the signed-in person's own profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateMeRequest {
    /// Display name, shown wherever the person appears (1 to 100 characters).
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MeOrg {
    pub id: OrganizationId,
    pub title: String,
    /// The user's organization role; null for service tokens.
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MeTeam {
    pub id: TeamId,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MeCredential {
    /// Token family as written in the secret prefix (`personal`,
    /// `service`, `execution`, `refresh`; execution/refresh are refused),
    /// or `session` for a browser session cookie (D46).
    pub kind: String,
    /// `human` or `agent` for personal tokens (always `human` for a
    /// session); null for service tokens.
    #[serde(default)]
    pub actor_kind: Option<String>,
    pub scopes: Vec<String>,
    /// Project restriction; null means the whole organization.
    #[serde(default)]
    pub projects: Option<Vec<ProjectId>>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
}

/// NOTIFY1: how much notification email a person wants. Names match
/// `runbook_core::notify::EmailPreference` exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmailPreferenceView {
    #[default]
    All,
    ApprovalsOnly,
    DailySummary,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailPreferencesView {
    pub preference: EmailPreferenceView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailPreferencesRequest {
    pub preference: EmailPreferenceView,
}

/// One-click unsubscribe (RFC 8058 shape): the token from a mail's link
/// is the whole credential; it only ever turns mail off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeRequest {
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnsubscribeView {
    pub unsubscribed: bool,
}
