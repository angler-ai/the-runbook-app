//! B4.1/C2 token management wire types (`/v1/tokens`). The secret appears
//! only in the one-time create response; views never carry it or its hash.
use crate::*;

/// Kinds a person can manage through `/v1/tokens`. Execution credentials
/// are minted per run (`/v1/executions/{exe}/tokens`), never here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedTokenKind {
    /// `trb_pat_`: the caller's own permissions, optionally narrowed.
    Personal,
    /// `trb_svc_`: one project, explicit scopes and role, not tied to a person.
    Service,
}

/// Every field narrows the caller's credential; nothing can widen it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    /// Default `personal`. Service tokens need an org owner or admin (D17).
    #[serde(default)]
    pub kind: Option<ManagedTokenKind>,
    /// Display name, at most 100 characters.
    #[serde(default)]
    pub name: Option<String>,
    /// PRD §9 scope names, each held by the calling credential. Omitted on
    /// a personal token: the caller's scopes. Required on a service token.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// Project IDs or slugs within the caller's projects. Omitted on a
    /// personal token: the caller's own project restriction. Exactly one on
    /// a service token.
    #[serde(default)]
    pub projects: Option<Vec<String>>,
    /// UTC epoch milliseconds. Personal tokens: at most one year ahead and
    /// never after the calling personal token's expiry (default 90 days,
    /// capped the same way). Service tokens: optional.
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    /// Personal tokens only (D26): `agent` records agent provenance with the
    /// caller as sponsor. Agent-kind tokens never carry `tokens:manage`.
    #[serde(default)]
    pub actor_kind: Option<ActorKind>,
    /// Service tokens only: the project role granted, at most the caller's.
    #[serde(default)]
    pub role: Option<ProjectRole>,
    /// Must be the caller when present: the API never issues tokens for
    /// another user (local bootstrap is not an API feature).
    #[serde(default)]
    pub user_id: Option<UserId>,
}

/// Token metadata. Never the secret or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenView {
    /// Whether issuance was authorized with a fresh allowed second factor.
    #[serde(default)]
    pub fresh_mfa_at_issue: bool,
    pub id: TokenId,
    pub kind: ManagedTokenKind,
    pub name: String,
    pub scopes: Vec<String>,
    /// Null: every project the principal may access.
    pub projects: Option<Vec<ProjectId>>,
    /// Personal tokens: the owning user.
    pub user_id: Option<UserId>,
    /// Personal tokens: `human` or `agent` (D26).
    pub actor_kind: Option<ActorKind>,
    /// Service tokens: the project and its granted role.
    pub project_id: Option<ProjectId>,
    pub role: Option<ProjectRole>,
    /// Who issued it through `/v1/tokens`; null for bootstrap credentials.
    pub created_by: Option<UserId>,
    pub created_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub last_used_at_ms: Option<i64>,
}

/// The one-time create response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenSecretView {
    pub token: TokenView,
    /// Shown once; only its SHA-256 is stored.
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenPage {
    pub items: Vec<TokenView>,
    pub next_cursor: Option<String>,
}

pub const TOKEN_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/tokens",
        request: None,
        response: Some("TokenPage"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/tokens",
        request: Some("CreateTokenRequest"),
        response: Some("TokenSecretView"),
        status: 201,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/tokens/{id}",
        request: None,
        response: None,
        status: 204,
    },
];
