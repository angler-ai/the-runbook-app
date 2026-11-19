//! Public directory and discovery response contract.
use crate::*;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiscoveryView {
    pub api_base_url: String,
    pub auth_base_url: String,
    pub edition: String,
    pub minimum_cli_version: String,
    /// Sign-in methods and the device endpoint (D48); defaulted so older
    /// discovery documents still decode.
    #[serde(default)]
    pub auth: AuthDiscovery,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiLimits {
    pub max_request_bytes: u64,
    pub source_bytes: Option<u64>,
    pub skill_bundle_bytes: Option<u64>,
    /// Upload limits (PRD §14). Advertised while `capabilities.uploads` is
    /// false, so clients can size their resizing before UP1 lands.
    pub uploads: Option<UploadLimits>,
    /// The most step runs one run may open on this server
    /// (`RUNBOOK_LIMIT_STEP_RUNS`); absent: no cap. A run that reaches it
    /// fails with `limit.step_runs_exceeded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_runs: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MetaView {
    pub edition: String,
    /// UPD1: the release this server runs (the workspace version; the
    /// number `runbook-server --version` and `runbook --version` print).
    /// Defaulted so older meta documents still decode.
    #[serde(default)]
    pub version: String,
    /// UPD1: the storage schema it runs on, by store: on the native server
    /// `postgres` (the applied migration number), on Workers the heads of
    /// `d1`, `execution_do`, `org_do` and `webhook_do` its code applies.
    #[serde(default)]
    pub schema: BTreeMap<String, u64>,
    pub capabilities: BTreeMap<String, bool>,
    pub limits: ApiLimits,
    pub usage: Option<BTreeMap<String, u64>>,
    /// UI-1: the operator's theme colour override (`RUNBOOK_THEME_*`);
    /// absent when no theme variable is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeView>,
}
/// UI-1: accent colours per theme, from the server's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThemeView {
    pub light: ThemeColors,
    pub dark: ThemeColors,
}
/// UI-1: one theme's colours, each `#rrggbb` (lowercase); absent when not
/// configured (the web app keeps its own).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThemeColors {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_secondary: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationRole {
    Owner,
    Admin,
    Member,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OrganizationView {
    pub id: OrganizationId,
    pub title: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryUserView {
    pub id: UserId,
    pub name: String,
    pub active: bool,
    pub role: OrganizationRole,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectView {
    pub id: ProjectId,
    pub org_id: OrganizationId,
    pub slug: String,
    pub title: String,
    /// Archived (2026-11-18): hidden in the app and read-only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TeamView {
    pub id: TeamId,
    pub org_id: OrganizationId,
    pub slug: String,
    pub title: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "DirectoryPage_{T}")]
pub struct DirectoryPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectVariables {
    pub declarations: BTreeMap<String, Value>,
    pub values: Data,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectVariable {
    pub declaration: Value,
    pub value: Value,
}
/// One manifest drives the contract and native transport's supported route set.
pub struct PublicRoute {
    pub method: &'static str,
    pub path: &'static str,
    pub request: Option<&'static str>,
    pub response: Option<&'static str>,
    pub status: u16,
}
pub const DIRECTORY_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/.well-known/runbook",
        request: None,
        response: Some("DiscoveryView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/meta",
        request: None,
        response: Some("MetaView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/orgs",
        request: None,
        response: Some("DirectoryPage_OrganizationView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/orgs/{org}/projects",
        request: None,
        response: Some("DirectoryPage_ProjectView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/orgs/{org}/projects",
        request: Some("CreateProjectRequest"),
        response: Some("ProjectView"),
        status: 201,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/orgs/{org}/users",
        request: None,
        response: Some("DirectoryPage_DirectoryUserView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/orgs/{org}/users",
        request: Some("CreateUserRequest"),
        response: Some("DirectoryUserView"),
        status: 201,
    },
    PublicRoute {
        method: "PATCH",
        path: "/v1/orgs/{org}/users/{user}",
        request: Some("UpdateUserRequest"),
        response: Some("DirectoryUserView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/orgs/{org}/teams",
        request: None,
        response: Some("DirectoryPage_TeamView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/orgs/{org}/teams",
        request: Some("CreateTeamRequest"),
        response: Some("TeamView"),
        status: 201,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/orgs/{org}/teams/{team}/members",
        request: None,
        response: Some("DirectoryPage_TeamMemberRequest"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/orgs/{org}/teams/{team}/members",
        request: Some("TeamMemberRequest"),
        response: Some("TeamMemberRequest"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/orgs/{org}/teams/{team}/members/{user}",
        request: None,
        response: None,
        status: 204,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/members",
        request: None,
        response: Some("DirectoryPage_ProjectMemberRequest"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/members",
        request: Some("ProjectMemberRequest"),
        response: Some("ProjectMemberRequest"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/projects/{p}/members/{user}",
        request: None,
        response: None,
        status: 204,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/vars",
        request: None,
        response: Some("ProjectVariables"),
        status: 200,
    },
    PublicRoute {
        method: "PUT",
        path: "/v1/projects/{p}/vars",
        request: Some("ProjectVariables"),
        response: Some("ProjectVariables"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/projects/{p}/vars",
        request: None,
        response: None,
        status: 204,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/vars/{name}",
        request: None,
        response: Some("ProjectVariable"),
        status: 200,
    },
    PublicRoute {
        method: "PUT",
        path: "/v1/projects/{p}/vars/{name}",
        request: Some("ProjectVariable"),
        response: Some("ProjectVariable"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/projects/{p}/vars/{name}",
        request: None,
        response: None,
        status: 204,
    },
];
