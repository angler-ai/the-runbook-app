//! The runner's core API (runner design §12, plan §Shared contracts).
use crate::*;

/// One published Ed25519 key (RFC 8037).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Jwk {
    /// Always `OKP`.
    pub kty: String,
    /// Always `Ed25519`.
    pub crv: String,
    /// The base64url public key.
    pub x: String,
    pub kid: String,
    /// Always `sig`.
    #[serde(rename = "use")]
    pub key_use: String,
    /// Always `EdDSA`.
    pub alg: String,
}
/// `GET /.well-known/jwks.json`: the keys that sign runner grants
/// (`rb-grant+jwt`) and user assertions (`rb-user+jwt`), current and
/// retired in the last 24 hours.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

/// The runner contract versions this server accepts at registration.
pub const RUNNER_CONTRACT_VERSIONS: &[u32] = &[1, 2, 3, 4];
/// Heartbeat protocol for decision-capable components with read-only enforcement.
pub const RUNNER_DECISION_CONTRACT: u32 = 1;

/// `POST /v1/orgs/{org}/runner/bootstrap-tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerBootstrapRequest {
    /// The runner API's URL, set by the operator. The token registers only
    /// a runner at this URL.
    pub url: String,
}
/// The bootstrap token, shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerBootstrapView {
    pub token: String,
    /// Epoch milliseconds.
    pub expires_at: i64,
}
/// `POST /v1/runner-installs/register` with the bootstrap token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerRegisterRequest {
    pub url: String,
    /// The runner's release (same numbering as the server).
    pub version: String,
    /// The runner contract version (see `RUNNER_CONTRACT_VERSIONS`).
    pub contract_version: u32,
    /// Base64url raw Ed25519 key, obtained from the vault through a trusted channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_identity_key: Option<String>,
}
/// The install's tokens, shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerRegisteredView {
    pub install_id: RunnerInstallId,
    /// `runner:directory` (runner-api).
    pub directory_token: String,
    /// `runner:release` (vault).
    pub release_token: String,
    /// Joins runners to the install.
    pub join_token: String,
}
/// `POST /v1/runner-installs/{ri}/runners` with the join token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerJoinRequest {
    pub name: String,
    pub group_id: RunnerGroupId,
    pub group_name: String,
    pub version: String,
}
/// The runner's claim token, shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerJoinedView {
    pub runner_id: RunnerId,
    /// `runner:claim`, for this runner's group only.
    pub claim_token: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerGroupRef {
    pub id: RunnerGroupId,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerHeartbeatEntry {
    pub id: RunnerId,
    /// The runner's own state, e.g. `ready`, `draining`, `frozen`.
    pub status: String,
    pub running: u32,
    pub capacity: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_contract: Option<u32>,
}
/// `POST /v1/runner-installs/{ri}/heartbeat` with the directory token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerHeartbeatRequest {
    #[serde(default)]
    pub groups: Vec<RunnerGroupRef>,
    #[serde(default)]
    pub runners: Vec<RunnerHeartbeatEntry>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunnerInstallStatus {
    Online,
    Offline,
    /// No runner is registered.
    None,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerGroupView {
    pub id: RunnerGroupId,
    pub name: String,
    /// The projects allowed on this group.
    pub projects: Vec<ProjectId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerView {
    pub id: RunnerId,
    pub name: String,
    pub group_id: RunnerGroupId,
    pub version: String,
    pub status: String,
    pub running: u32,
    pub capacity: u32,
    /// Epoch milliseconds.
    pub last_heartbeat_at: Option<i64>,
}
/// `GET /v1/runner-installs/{ri}/groups` with the install's directory token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerGroupRegistryView {
    pub install_id: RunnerInstallId,
    pub groups: Vec<RunnerGroupView>,
}

/// `GET /v1/orgs/{org}/runner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerStatusView {
    pub install_id: Option<RunnerInstallId>,
    pub status: RunnerInstallStatus,
    pub version: Option<String>,
    pub contract_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_identity_key: Option<String>,
    pub groups: Vec<RunnerGroupView>,
    pub runners: Vec<RunnerView>,
    /// Epoch milliseconds.
    pub last_heartbeat_at: Option<i64>,
}
/// `PUT /v1/orgs/{org}/runner/vault-identity` (human organization owner).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerVaultIdentityRequest {
    pub public_key: String,
}

/// `PUT /v1/orgs/{org}/runner/groups/{g}/projects`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerGroupProjectsRequest {
    pub project_ids: Vec<ProjectId>,
}

/// What a tool, credential or host lets a step do (runner design §6.5).
/// Unlabeled tools and credentials are `destructive`; unlabeled hosts `read`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RunnerLabel {
    Read,
    Write,
    Destructive,
}
impl RunnerLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Destructive => "destructive",
        }
    }
}
/// How a released credential reaches a step (runner design §7.6).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Model,
    Mcp,
    Proxy,
    Token,
}
impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Mcp => "mcp",
            Self::Proxy => "proxy",
            Self::Token => "token",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCeilingCredential {
    pub id: String,
    pub name: String,
    pub delivery: Vec<Delivery>,
    pub label: RunnerLabel,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCeilingTool {
    pub server_id: String,
    pub name: String,
    pub label: RunnerLabel,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCeilingHost {
    pub host: String,
    pub label: RunnerLabel,
}
/// The most an agent version allows a step (runner design §6.3): allowed
/// model profiles, limits, and what it can reach with its labels. Not
/// secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCeiling {
    pub profiles: Vec<String>,
    /// Configured provider model IDs keyed by profile name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub profile_models: std::collections::BTreeMap<String, String>,
    pub max_cost_usd: f64,
    pub max_tokens: u64,
    pub timeout_s: u64,
    #[serde(default)]
    pub credentials: Vec<AgentCeilingCredential>,
    #[serde(default)]
    pub tools: Vec<AgentCeilingTool>,
    #[serde(default)]
    pub hosts: Vec<AgentCeilingHost>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Active,
    Paused,
    Archived,
}
/// `PUT /v1/runner-installs/{ri}/agents/{agent_id}`: one runner agent's
/// current version, synced by the runner. `(project, name)` is unique.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentDirectoryEntry {
    pub agent_id: String,
    pub name: String,
    pub project_id: ProjectId,
    pub group_id: RunnerGroupId,
    pub status: AgentStatus,
    pub version_id: String,
    pub ceiling: AgentCeiling,
}
/// The agent name that keeps a step open to external agents.
pub const EXTERNAL_AGENT: &str = "external";

/// The kind of grant: a step's capability, or one credential's release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    Capability,
    Credential,
}
/// `POST /v1/step-runs/{sr}/grants` (release token): a grant bound to the
/// runner's live claim of the step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantRequest {
    pub kind: GrantKind,
    pub claim_id: ClaimId,
    pub generation: u64,
    pub runner_id: RunnerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
}
/// A signed grant (`typ: rb-grant+jwt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GrantView {
    pub grant: String,
    /// Epoch milliseconds: the lease deadline (the JWT's `exp`, in seconds).
    pub expires_at: i64,
}
/// `POST /v1/step-runs/{sr}/grants/revoked`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantRevokedRequest {
    pub credential_id: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerWaitingItem {
    pub step_run_id: StepRunId,
    /// `agent_paused`, `agent_outside_ceiling`, `runner_offline` or
    /// `limit:<level>`.
    pub reason: String,
    /// Optional words for people, shown on the run page after "Waiting:"
    /// (for example "project ops is at its 5 concurrent steps"). At most
    /// 200 characters; never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
/// `POST /v1/runner-installs/{ri}/waiting`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunnerWaitingRequest {
    pub items: Vec<RunnerWaitingItem>,
}
/// The events a runner reports for org webhooks.
pub const RUNNER_EVENT_KINDS: &[&str] = &[
    "runner.budget_threshold",
    "runner.probe_succeeded",
    "runner.secret_in_response",
    "runner.binding_mismatch",
    "runner.release_denied",
];
/// `POST /v1/runner-installs/{ri}/events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerEventRequest {
    pub kind: String,
    #[serde(default)]
    pub data: serde_json::Value,
}
/// Payload of the runner directory's internal decision ledger event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerDecisionCost {
    pub step_run_id: StepRunId,
    pub lease_id: ClaimId,
    pub cost: StepCost,
}
/// Payload of the runner directory's internal task ledger event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerTaskCost {
    pub step_run_id: StepRunId,
    pub claim_id: ClaimId,
    pub runner_id: RunnerId,
    pub cost: StepCost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadyStepView {
    #[serde(default)]
    pub kind: RunnerWorkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ReadyDecision>,
    pub step_run_id: StepRunId,
    pub execution_id: ExecutionId,
    pub project_id: ProjectId,
    pub agent_id: String,
    /// Epoch milliseconds.
    pub ready_at: i64,
    pub max_cost_usd: Option<f64>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkKind {
    #[default]
    Step,
    Decision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadyDecision {
    pub prompt: String,
    pub options: Vec<ApprovalOption>,
    pub model_profile: Option<String>,
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimedDecisionRead {
    Decision {
        step_run_id: StepRunId,
        decision: ReadyDecision,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerDecideRequest {
    pub option: String,
    pub comment: String,
    pub evidence: Vec<String>,
    /// Legacy input. The core uses the model pinned when it claimed the approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Legacy input. Decision costs come from the runner ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<StepCost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerAbstainRequest {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Legacy input. Decision costs come from the runner ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<StepCost>,
}

/// `GET /v1/runner-installs/{ri}/ready?group={g}&limit={n}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadyStepPage {
    pub items: Vec<ReadyStepView>,
    pub next_cursor: Option<String>,
}

const fn route(
    method: &'static str,
    path: &'static str,
    request: Option<&'static str>,
    response: Option<&'static str>,
    status: u16,
) -> PublicRoute {
    PublicRoute {
        method,
        path,
        request,
        response,
        status,
    }
}

/// A runner can submit a proposal for human review from its live claim.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunnerMemoryProposal {
    pub title: String,
    pub body_markdown: MemoryBody,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The immutable folder hash and main instructions of a project skill.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct RunnerSkillPin {
    pub name: String,
    pub hash: String,
    pub body: String,
}

pub const RUNNER_ROUTES: &[PublicRoute] = &[
    route(
        "POST",
        "/v1/runner/decisions/{lease}/decide",
        Some("RunnerDecideRequest"),
        Some("DecisionRecord"),
        200,
    ),
    route(
        "POST",
        "/v1/runner/decisions/{lease}/abstain",
        Some("RunnerAbstainRequest"),
        Some("ApprovalAgentNote"),
        200,
    ),
    route("GET", "/.well-known/jwks.json", None, Some("Jwks"), 200),
    route(
        "POST",
        "/v1/orgs/{org}/runner/bootstrap-tokens",
        Some("RunnerBootstrapRequest"),
        Some("RunnerBootstrapView"),
        201,
    ),
    route(
        "GET",
        "/v1/orgs/{org}/runner",
        None,
        Some("RunnerStatusView"),
        200,
    ),
    route(
        "PUT",
        "/v1/orgs/{org}/runner/groups/{g}/projects",
        Some("RunnerGroupProjectsRequest"),
        None,
        204,
    ),
    route(
        "PUT",
        "/v1/orgs/{org}/runner/vault-identity",
        Some("RunnerVaultIdentityRequest"),
        None,
        204,
    ),
    route(
        "POST",
        "/v1/runner-installs/register",
        Some("RunnerRegisterRequest"),
        Some("RunnerRegisteredView"),
        201,
    ),
    route(
        "POST",
        "/v1/runner-installs/{ri}/runners",
        Some("RunnerJoinRequest"),
        Some("RunnerJoinedView"),
        201,
    ),
    route(
        "POST",
        "/v1/runner-installs/{ri}/heartbeat",
        Some("RunnerHeartbeatRequest"),
        None,
        204,
    ),
    route(
        "PUT",
        "/v1/runner-installs/{ri}/agents/{agent_id}",
        Some("AgentDirectoryEntry"),
        None,
        204,
    ),
    route(
        "DELETE",
        "/v1/runner-installs/{ri}/agents/{agent_id}",
        None,
        None,
        204,
    ),
    route(
        "POST",
        "/v1/runner-installs/{ri}/waiting",
        Some("RunnerWaitingRequest"),
        None,
        204,
    ),
    route(
        "POST",
        "/v1/runner-installs/{ri}/events",
        Some("RunnerEventRequest"),
        None,
        204,
    ),
    route(
        "GET",
        "/v1/runner-installs/{ri}/groups",
        None,
        Some("RunnerGroupRegistryView"),
        200,
    ),
    route(
        "GET",
        "/v1/runner-installs/{ri}/ready",
        None,
        Some("ReadyStepPage"),
        200,
    ),
    route(
        "GET",
        "/v1/runner-installs/{ri}/projects/{project_id}/skills/{name}",
        None,
        Some("RunnerSkillPin"),
        200,
    ),
    route("GET", "/v1/step-runs/{sr}/context/run", None, None, 200),
    route("GET", "/v1/step-runs/{sr}/context/step", None, None, 200),
    route("GET", "/v1/step-runs/{sr}/context/runs", None, None, 200),
    route(
        "GET",
        "/v1/step-runs/{sr}/context/past-run",
        None,
        None,
        200,
    ),
    route("GET", "/v1/step-runs/{sr}/context/memory", None, None, 200),
    route(
        "POST",
        "/v1/step-runs/{sr}/context/memory",
        Some("RunnerMemoryProposal"),
        None,
        201,
    ),
    route(
        "POST",
        "/v1/step-runs/{sr}/grants",
        Some("GrantRequest"),
        Some("GrantView"),
        200,
    ),
    route(
        "POST",
        "/v1/step-runs/{sr}/grants/revoked",
        Some("GrantRevokedRequest"),
        None,
        204,
    ),
    route("GET", RUNNER_PROXY_PATH, None, None, 200),
    route("POST", RUNNER_PROXY_PATH, None, None, 200),
    route("PUT", RUNNER_PROXY_PATH, None, None, 200),
    route("PATCH", RUNNER_PROXY_PATH, None, None, 200),
    route("DELETE", RUNNER_PROXY_PATH, None, None, 200),
];
/// R0-5: the proxy to the org's runner.
pub const RUNNER_PROXY_PATH: &str = "/v1/runners/{ri}/proxy/{*path}";
