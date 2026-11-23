//! Shared wire types for the draft pilot contract; no execution or I/O behavior.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroU64};

pub mod document;
pub mod inbox;
pub mod memory;
pub use document::*;
pub use inbox::*;
pub use memory::*;

/// The canonical API address for a run on this origin.
pub fn execution_api_url(origin: &str, id: &ExecutionId) -> String {
    format!("{}/v1/executions/{id}", origin.trim_end_matches('/'))
}

/// Derive the browser link from a run's stored API address.
pub fn execution_web_url(url: &str, id: &ExecutionId) -> String {
    let api_path = format!("/v1/executions/{id}");
    let web_path = format!("/x/{id}");
    let origin = url
        .strip_suffix(&api_path)
        .or_else(|| url.strip_suffix(&web_path))
        .unwrap_or_default();
    format!("{origin}{web_path}")
}

pub type Data = BTreeMap<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Succeeded,
    Failed,
    Blocked,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimRef {
    pub id: ClaimId,
    #[schemars(range(max = 18446744073709551615_u64))]
    pub generation: NonZeroU64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReportRequest {
    pub step_run_id: StepRunId,
    pub claim: ClaimRef,
    pub status: ReportStatus,
    pub verdict: Option<Verdict>,
    #[serde(default)]
    pub outputs: Data,
    pub summary: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub logs: Vec<String>,
    /// R0-6: what the step cost (runner agents report it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<StepCost>,
}

/// R0-6: a step's cost: model and compute spend in US dollars and the
/// model tokens. Finite and never negative.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepCost {
    #[serde(default)]
    pub model_usd: f64,
    #[serde(default)]
    pub compute_usd: f64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
}
impl StepCost {
    /// Every amount finite and at least zero.
    pub fn valid(&self) -> bool {
        [self.model_usd, self.compute_usd]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
    }
    pub fn add(&mut self, other: &StepCost) {
        self.model_usd += other.model_usd;
        self.compute_usd += other.compute_usd;
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(other.cached_tokens);
    }
}

macro_rules! domain_id {
    ($name:ident, $prefix:literal, $pattern:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(#[schemars(regex(pattern = $pattern))] String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl std::str::FromStr for $name {
            type Err = Diagnostic;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let valid = value.strip_prefix($prefix).is_some_and(|suffix| {
                    suffix.len() == 26
                        && matches!(suffix.as_bytes()[0], b'0'..=b'7')
                        && suffix
                            .bytes()
                            .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
                });
                if valid {
                    Ok(Self(value.to_owned()))
                } else {
                    Err(Diagnostic {
                        path: String::new(),
                        code: "id.invalid".into(),
                        message: concat!("Expected a ", $prefix, "-prefixed ULID.").into(),
                        line: None,
                        column: None,
                    })
                }
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

domain_id!(TokenId, "tok_", "^tok_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(OrganizationId, "org_", "^org_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(MemoryId, "mem_", "^mem_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(RunbookId, "rb_", "^rb_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(VersionId, "rbv_", "^rbv_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(ExecutionId, "exe_", "^exe_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(StepRunId, "sr_", "^sr_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(ClaimId, "clm_", "^clm_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(UserId, "usr_", "^usr_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(TeamId, "team_", "^team_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(ProjectId, "prj_", "^prj_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
// An evidence upload slot and its finalized file (PRD §14, D49).
domain_id!(EvidenceId, "evd_", "^evd_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
// A pending organization invitation (D45).
domain_id!(InvitationId, "inv_", "^inv_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
// R0-2: a runner install (control plane), a runner group and a runner.
domain_id!(RunnerInstallId, "ri_", "^ri_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(RunnerGroupId, "rg_", "^rg_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
domain_id!(RunnerId, "rn_", "^rn_[0-7][0-9A-HJKMNP-TV-Z]{25}$");

/// CLI diagnostics have no YAML source span. Parser diagnostics remain separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    pub path: String,
    pub code: String,
    pub message: String,
    /// 1-based source span when the compiler knows it (B1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Diagnostic {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VersionSelector {
    Number(NonZeroU32),
    Id(VersionId),
}

impl JsonSchema for VersionSelector {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VersionSelector".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"oneOf":[
            {"type":"integer","minimum":1,"maximum":u32::MAX},
            {"type":"string","pattern":"^rbv_[0-7][0-9A-HJKMNP-TV-Z]{25}$"}
        ]})
    }
}

/// Parse a CLI reference without resolving a runbook or consulting storage.
pub fn parse_runbook_ref(s: &str) -> Result<(String, Option<VersionSelector>), Diagnostic> {
    let (name, raw) = s
        .split_once('@')
        .map_or((s, None), |(name, raw)| (name, Some(raw)));
    if !name.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    }) {
        return Err(Diagnostic {
            path: "runbook".into(),
            code: "runbook.invalid_reference".into(),
            message: "Runbook name must be a kebab-case URL slug.".into(),
            line: None,
            column: None,
        });
    }
    let Some(raw) = raw else {
        return Ok((name.to_owned(), None));
    };
    let invalid = || Diagnostic {
        path: "version".into(),
        code: "runbook.invalid_version_selector".into(),
        message: "Version must be a positive integer or an rbv_-prefixed ULID.".into(),
        line: None,
        column: None,
    };
    let selector = if !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()) {
        VersionSelector::Number(raw.parse::<NonZeroU32>().map_err(|_| invalid())?)
    } else {
        VersionSelector::Id(raw.parse().map_err(|_| invalid())?)
    };
    Ok((name.to_owned(), Some(selector)))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StartRequest {
    pub runbook: String,
    /// Omitted selects the latest published version when the start is accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionSelector>,
    #[serde(default)]
    pub inputs: Data,
    #[serde(default)]
    pub fields: Data,
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RerunRequest {
    /// Omitted reuses the original run's version, including a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionSelector>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Ready,
    Claimed,
    Waiting,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
    TimedOut,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Queued,
    Running,
    Waiting,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Ordinary,
    Cleanup,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FieldError {
    pub path: String,
    pub code: String,
    pub message: String,
}

/// RFC 9457 problem details plus stable application and field codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Problem {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    #[schemars(range(min = 400, max = 599))]
    pub status: u16,
    pub detail: String,
    pub instance: String,
    pub code: String,
    pub errors: Vec<FieldError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<NextAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "reference", rename_all = "snake_case")]
pub enum RecipientRef {
    User(UserId),
    Team(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActorRef {
    Human {
        user_id: UserId,
    },
    Agent {
        principal_id: String,
        session_id: String,
        name: String,
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service_attribution: Option<Box<ServiceTokenAttribution>>,
    },
    System,
}

/// Service credential provenance recorded when work is performed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceTokenAttribution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<UserId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<UserId>,
}

impl ServiceTokenAttribution {
    pub fn users(&self) -> impl Iterator<Item = &UserId> {
        self.creator.iter().chain(&self.owner)
    }
}

impl ActorRef {
    /// Claim ownership is tied to the principal and session. Attribution can
    /// change between the claim and a subsequent report.
    pub fn same_claim_actor(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Agent {
                    principal_id,
                    session_id,
                    name,
                    model,
                    ..
                },
                Self::Agent {
                    principal_id: their_id,
                    session_id: their_session,
                    name: their_name,
                    model: their_model,
                    ..
                },
            ) => {
                (principal_id, session_id, name, model)
                    == (their_id, their_session, their_name, their_model)
            }
            _ => self == other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChecklistTick {
    pub title: String,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepRecord {
    pub status: StepStatus,
    pub verdict: Option<Verdict>,
    pub outputs: Data,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Amendment {
    pub step_run_id: StepRunId,
    pub from: StepRecord,
    pub to: StepRecord,
    pub reason: String,
    pub by: ActorRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepView {
    #[schemars(range(max = 4294967295_u64))]
    pub schema_revision: NonZeroU32,
    pub id: StepRunId,
    pub path: String,
    pub status: StepStatus,
    pub verdict: Option<Verdict>,
    pub outputs: Data,
    pub summary: Option<String>,
    pub actor: Option<ActorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub claim: Option<ClaimRef>,
    #[schemars(range(max = 4294967295_u64))]
    pub attempt: NonZeroU32,
    pub satisfied: bool,
    pub exhausted: bool,
    pub checklist: Vec<ChecklistTick>,
    pub amended: bool,
    pub amendments: Vec<Amendment>,
    pub stale: bool,
    pub eligible_recipients: Vec<RecipientRef>,
    /// LANG4: a `call` step's child execution. Omitted for other steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<StepCallView>,
    /// UI-2: what a `wait` step waits for, while it waits. Omitted for
    /// other steps and once the wait ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<StepWaitView>,
    /// UI-2: a task's declared actor (`agent`, `human` or `any`). Omitted
    /// for other steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_kind: Option<StepActorKind>,
    /// UI-2: a task's assignee as declared and resolved (a person or a
    /// team), the recipients its inbox item goes to. Empty when it names
    /// none, and for other steps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<RecipientRef>,
    /// UI-3: the branch a `switch` took, once it chose one. Omitted for
    /// other steps and before the choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<StepBranchView>,
    /// UI-3: a started `foreach` or `loop`'s iterations. Omitted for other
    /// steps and before it starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<StepIterationsView>,
    /// UI-3: why the engine skipped the step (`condition`: its `if` was
    /// false). Omitted otherwise, including a step an actor reported as
    /// skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<StepSkipReason>,
    /// LANG9: reminders sent for this step's current attempt and its
    /// follow-up escalation, once either has happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followup: Option<StepFollowupView>,
    /// R0: the runner agent an agent step targets, and what the runner
    /// did with it. Omitted for steps open to external agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<StepRunnerView>,
    /// R0-6: what the step's reports said it cost (all attempts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<StepCost>,
}

/// R0: a runner agent's step on the run page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepRunnerView {
    pub agent_id: String,
    pub agent_name: String,
    /// The model profile the step asked for, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The agent version the current (or last) claim pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version_id: Option<String>,
    /// Why the step waits for its runner, until it is claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<String>,
    /// The runner's words for people about the wait, if it gave any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_detail: Option<String>,
    /// Credentials released to the step's claims (never values).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub releases: Vec<StepCredentialRelease>,
    /// Credentials were released to a claim that ended without a report.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub released_never_reported: bool,
    /// R1-D2: on an approval the agent may decide, when its decision work
    /// becomes ready (`agent.after` past the approval opening; epoch ms).
    /// Omitted for task steps and before the approval is requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_ready_at_ms: Option<i64>,
    /// R1-D2: the runner agent reviewing the approval now (a decision
    /// lease is held). People can still decide while it reviews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_lease: Option<DecisionLeaseView>,
    /// R1-D2: the agent's abstentions on the approval (agent-written,
    /// untrusted text).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_notes: Vec<ApprovalAgentNote>,
}
/// R0: one credential release in a step's record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepCredentialRelease {
    pub credential_id: String,
    pub credential_version: u64,
    pub delivery: String,
    pub runner_id: String,
    /// Epoch milliseconds.
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<String>,
}

/// LANG9: follow-ups that have already happened for a step's current
/// attempt. An escalation can happen before any reminder is due.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepFollowupView {
    #[schemars(range(max = 4294967295_u64))]
    pub reminders_sent: u32,
    pub escalated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalated_to: Vec<RecipientRef>,
}

/// UI-3: the branch a `switch` took: a case by its index among the
/// `cases` (from 0, as written), or the `default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepBranchView {
    Case { index: u32 },
    Default,
}

/// UI-3: how far a `foreach` or `loop` is through its iterations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepIterationsView {
    /// Iterations admitted so far (each has its own step runs).
    pub started: u32,
    /// A `foreach`'s item count, fixed when it starts. Absent for a
    /// `loop`, whose cap is its `max_iterations`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    /// A `foreach`'s items as text, in order, when every item is a string,
    /// number or boolean (each cut to 80 characters), so a view can name an
    /// iteration ("us-west") whose key is its index. Omitted otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// UI-3: why the engine skipped a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepSkipReason {
    /// The step's `if` was false.
    Condition,
}

/// UI-2: who a task is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepActorKind {
    Human,
    Agent,
    Any,
}

/// UI-2: a waiting `wait` step's target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepWaitView {
    /// The awaited signal's name (`wait: {signal: …}`); absent for a timer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// When a timer fires, or when a signal wait times out (store clock,
    /// epoch ms). Absent for a signal wait with no timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
}

/// UI-2: how far a run is through its top-level steps (`spec.steps`, not
/// `finally`). A container counts once, when it finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionProgressView {
    /// Top-level steps that reached a terminal status (skipped included).
    pub done: u32,
    /// Top-level steps the run's version declares.
    pub total: u32,
}

/// LANG4: a `call` step's link to the child execution it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepCallView {
    /// The child run. It exists once `started` is true; a refused start
    /// (see `error`) never creates it.
    pub child_execution_id: ExecutionId,
    pub started: bool,
    /// Why the step failed: the code that refused the child's start (for
    /// example `limit.executions_monthly`), `call.child_failed`,
    /// `call.child_cancelled` or `call.outputs_invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// LANG4: the `call` step that started a child execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionParentView {
    pub execution_id: ExecutionId,
    pub step_run_id: StepRunId,
    /// The calling step's path in the parent run.
    pub step: String,
    /// Calls from the top-level run (its child is 1; at most 3).
    #[schemars(range(min = 1, max = 3))]
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionView {
    /// Milestones that passed their run-relative deadline and are still unfinished.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overdue_steps: Vec<OverdueStepView>,
    /// The run's API address.
    #[serde(default)]
    pub url: String,
    /// The run page to share with people.
    #[serde(default)]
    pub web_url: String,
    #[schemars(range(max = 4294967295_u64))]
    pub schema_revision: NonZeroU32,
    pub id: ExecutionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_id: Option<RunbookId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub runbook_version_id: VersionId,
    /// Always emitted; null for preview versions. Identity is the rbv_ ID.
    #[serde(deserialize_with = "required_version_number")]
    #[schemars(with = "RequiredNullable<u32>")]
    #[schemars(range(min = 1, max = 4294967295_u64))]
    pub runbook_version: Option<u32>,
    pub title: String,
    pub status: ExecutionStatus,
    pub phase: ExecutionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<ExecutionAdmissionView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_deadline_at_ms: Option<i64>,
    pub result: Option<Value>,
    pub pending_terminal_status: Option<ExecutionStatus>,
    /// Why a run that failed or was cancelled ended, when the engine
    /// recorded a cause: `timeout`, `cancelled`,
    /// `limit.step_runs_exceeded` (the server's step cap),
    /// `limit.run_size_exceeded`, `execution.outputs_too_large`, … A run
    /// failed by one of its steps carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    pub inputs: Data,
    pub fields: Data,
    pub tags: Vec<String>,
    pub steps: Vec<StepView>,
    pub amended: bool,
    pub amendments: Vec<Amendment>,
    pub stale_step_ids: Vec<StepRunId>,
    pub rerun_of: Option<ExecutionId>,
    pub resume_brief: Option<ResumeBrief>,
    /// Human notifications that reached no subscriber (C1). Recording an
    /// event alone never counts as notifying anyone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notifications_not_delivered: Vec<UndeliveredNotification>,
    /// LANG3: the run that replaced this one (`on_conflict:
    /// cancel_previous`), and who started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<SupersededBy>,
    /// LANG4: the `call` step that started this run, when it is a child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ExecutionParentView>,
    /// UI-2: top-level step progress. Absent before the run starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ExecutionProgressView>,
    /// R0-6: what the run's steps reported they cost, summed. Absent when
    /// no step reported a cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<StepCost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OverdueStepView {
    pub path: String,
    pub title: String,
    pub due_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_run_id: Option<StepRunId>,
}

/// LANG3: a run cancelled because a newer run took its concurrency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SupersededBy {
    pub execution_id: ExecutionId,
    /// The principal who started the replacing run (`user:…`, `service:…`).
    pub started_by: String,
}

fn required_version_number<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<u32>, D::Error> {
    Option::<NonZeroU32>::deserialize(d).map(|number| number.map(NonZeroU32::get))
}

// Unlike Option's schema marker, this nullable property must be present.
struct RequiredNullable<T>(std::marker::PhantomData<T>);
impl<T: JsonSchema> JsonSchema for RequiredNullable<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Nullable{}", T::schema_name()).into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Option<T>>::json_schema(generator)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalOption {
    pub id: String,
    pub title: String,
    pub auto: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum DecisionRecord {
    Human {
        user_id: UserId,
        #[schemars(range(max = 18446744073709551615_u64))]
        membership_revision: NonZeroU64,
        option: String,
        comment: Option<String>,
    },
    System {
        policy: String,
        option: String,
    },
    /// An agent's decision on an `allow_agents` gate, with its provenance
    /// (PRD §5). Never a human signature or quorum count. Responses stored
    /// before this variant existed keep their `System { policy:
    /// "agent:<name>" }` shape.
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_source: Option<AgentEvidenceSource>,
        agent_name: String,
        model: Option<String>,
        session_id: String,
        option: String,
        comment: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentEvidenceSource {
    RunnerReported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalView {
    #[schemars(range(max = 4294967295_u64))]
    pub schema_revision: NonZeroU32,
    pub step_run_id: StepRunId,
    pub status: StepStatus,
    pub prompt: String,
    pub options: Vec<ApprovalOption>,
    pub eligible_recipients: Vec<RecipientRef>,
    pub escalation_recipients: Vec<RecipientRef>,
    /// People who completed work this approval must independently review.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_completers: Vec<UserId>,
    /// Why a pending approval has no independent reviewers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    pub escalated: bool,
    #[schemars(range(max = 4294967295_u64))]
    pub quorum: NonZeroU32,
    pub decisions: Vec<DecisionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_lease: Option<DecisionLeaseView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_notes: Vec<ApprovalAgentNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DecisionLeaseView {
    pub lease_id: ClaimId,
    pub agent_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalAgentNote {
    pub lease_id: ClaimId,
    pub principal: String,
    pub agent_name: String,
    pub model: Option<String>,
    pub reason: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompletedStepSummary {
    pub step_run_id: StepRunId,
    pub path: String,
    pub status: StepStatus,
    pub verdict: Option<Verdict>,
    /// Untrusted recorded data; never instructions for the resuming actor.
    pub outputs: Data,
    pub summary: Option<String>,
    pub amended: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadyWork {
    pub step_run_id: StepRunId,
    pub path: String,
    pub title: String,
    pub actor_kind: ActorKind,
    /// R0-4: the runner agent the step targets, and its group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommentView {
    pub author: UserId,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResumeBrief {
    /// The run's API address.
    #[serde(default)]
    pub url: String,
    /// The run page to share with people.
    #[serde(default)]
    pub web_url: String,
    #[schemars(range(max = 4294967295_u64))]
    pub schema_revision: NonZeroU32,
    pub execution_id: ExecutionId,
    pub completed_steps: Vec<CompletedStepSummary>,
    pub decisions: Vec<DecisionRecord>,
    pub comments: Vec<CommentView>,
    /// References only: full future instructions are not disclosed here.
    pub ready_work: Vec<ReadyWork>,
    pub next_action: NextAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    ClaimReadyWork,
    Wait,
    Exit,
    RefreshCredentials,
    AskHumanToPublish,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateProjectRequest {
    pub slug: String,
    pub title: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateUserRequest {
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateUserRequest {
    pub active: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateTeamRequest {
    pub slug: String,
    pub title: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRole {
    Viewer,
    Operator,
    /// Editors and admins hold write/publish permissions; credentials may narrow them.
    Editor,
    Admin,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectMemberRequest {
    pub user_id: UserId,
    pub role: ProjectRole,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TeamMemberRequest {
    pub user_id: UserId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DecisionRequest {
    pub option: String,
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorDeclaration>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SignalRequest {
    #[serde(default)]
    pub payload: Data,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAction {
    Pause,
    Resume,
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionPatchRequest {
    pub action: Option<ExecutionAction>,
    pub title: Option<String>,
    pub fields: Option<Data>,
    pub tags: Option<Vec<String>>,
}
/// D26: who is claiming. Omitted means the token's own principal. A token minted
/// for agent use is always recorded as an agent, whatever is declared here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActorDeclaration {
    pub kind: ActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorDeclaration>,
    /// LANG7: a person claims an `actor: agent` task by taking it over.
    /// Allowed only while the step is ready (never claimed, or its agent
    /// claim expired or was released); recorded as `StepTakenOver`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub take_over: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HeartbeatRequest {
    pub claim: ClaimRef,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AdminReleaseRequest {
    #[schemars(regex(pattern = "\\S"))]
    pub reason: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ReleaseRequest {
    Owner(HeartbeatRequest),
    Admin(AdminReleaseRequest),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogsRequest {
    pub claim: ClaimRef,
    pub logs: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChecklistRequest {
    pub claim: ClaimRef,
    pub checklist: Vec<ChecklistTick>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MintTokenRequest {
    pub session_id: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OverrideRequest {
    pub to: StepRecord,
    pub reason: String,
}

/// Source YAML is kept verbatim, including comments and ordering. Creating does not publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateRunbookRequest {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

/// Save only; the HTTP If-Match header selects the expected revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SaveDraftRequest {
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftView {
    pub runbook_id: RunbookId,
    pub source: String,
    #[schemars(range(max = 18446744073709551615_u64))]
    pub revision: NonZeroU64,
}

/// Only published versions have a number. Field names match ExecutionView identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublishedVersionView {
    pub runbook_id: RunbookId,
    pub runbook_version_id: VersionId,
    #[schemars(range(max = 4294967295_u64))]
    pub runbook_version: NonZeroU32,
    /// Nonblocking diagnostics emitted when publishing this version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Diagnostic>,
}

/// Any stored version: preview versions have an rbv_ ID but no number, and
/// never count as latest. Branch and commit identify Git previews.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VersionView {
    pub runbook_id: RunbookId,
    pub runbook_version_id: VersionId,
    #[schemars(range(max = 4294967295_u64))]
    pub runbook_version: Option<NonZeroU32>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidateRequest {
    pub source: String,
    /// Project slug or ID to resolve skills, users, teams and variables against (D18).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Compilation diagnostics use the shared wire shape; parser source spans stay separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidateResponse {
    pub diagnostics: Vec<Diagnostic>,
    /// Null when compilation cannot determine a budget. Validation never saves source.
    #[serde(deserialize_with = "required_step_budget")]
    #[schemars(with = "RequiredNullable<u64>")]
    #[schemars(range(min = 0, max = 18446744073709551615_u64))]
    pub step_budget: Option<u64>,
}

fn required_step_budget<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(d)
}

pub mod directory_api;
pub use directory_api::*;

pub mod authoring_api;
pub mod me_api;
pub mod mfa_api;
pub use authoring_api::*;
pub use me_api::*;
pub use mfa_api::MFA_ROUTES;
pub mod pictures_api;
pub mod search_api;
pub mod slack_api;
pub use pictures_api::*;
pub use search_api::*;
pub use slack_api::*;

pub mod execution_api;
pub use execution_api::*;

pub mod webhook_api;
pub use webhook_api::*;

pub mod token_api;
pub use token_api::*;
// R0: the runner's core API.
pub mod runner_api;
pub use runner_api::*;

// T0 contract freeze (plan 2026-11-12): planned post-pilot operations.
pub mod archive_api;
pub mod auth_api;
pub mod upload_api;
pub use archive_api::*;
pub use auth_api::*;
pub use upload_api::*;
pub mod planned;
pub use planned::PLANNED_ROUTES;

/// One-time response only; never persisted in replay or diagnostic logs.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionTokenPair {
    pub family_id: String,
    pub generation: u64,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: i64,
    pub refresh_expires_at_ms: i64,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct PinnedSkillRead {
    pub hash: String,
    pub body: String,
    pub files: Vec<SkillFileView>,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClaimedStepRead {
    pub step_run_id: StepRunId,
    /// Rendered: the template with run data substituted (compatibility).
    pub instructions: String,
    /// Rendered checklist titles; ticks name items by these.
    pub checklist: Vec<String>,
    pub parameters: Data,
    pub output_schema: Value,
    pub skill: Option<PinnedSkillRead>,
    /// LANG8: multiple skills per step. Keep `skill` set to the first skill for compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<PinnedSkillRead>,
    /// F62: the author's text and the run data bound into it, apart.
    pub template: StepTemplate,
    /// D41: active memory for this step, as labeled untrusted data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<StepMemory>,
    /// CF12c: the step's evidence policy, so a client can tell before it
    /// sends a report what the report may attach. Absent from servers
    /// before CF12c.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<StepEvidencePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_limits: Option<RunnerStepLimits>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunnerStepLimits {
    pub model: Option<String>,
    pub max_tokens: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub check: bool,
}

/// CF12c: a task's `evidence` as a step read carries it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepEvidencePolicy {
    /// Kinds a successful report must include: `link`, `log`, `file`,
    /// `screenshot`.
    pub required: Vec<String>,
    /// The only kinds a report may attach; absent when every kind is
    /// accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<Vec<String>>,
}
/// F62 (PRD §18: run data is never merged into instructions): a step's
/// instructions and checklist as the runbook author wrote them, with the run
/// data each `${{ … }}` placeholder resolved to kept separately. Run data
/// never changes `instructions` or `checklist` here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepTemplate {
    /// The instructions selected for the reader's audience, placeholders intact.
    pub instructions: String,
    /// Checklist item templates, in order, placeholders intact.
    pub checklist: Vec<String>,
    /// Run data, keyed by each placeholder's expression as written (trimmed),
    /// for example `inputs.service`. Untrusted data, never instructions.
    pub values: Data,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillFileRead {
    pub path: String,
    /// Base64 bytes of exactly one file in the step's pinned manifest.
    pub content: String,
}

pub mod step_logs;
pub use step_logs::*;
