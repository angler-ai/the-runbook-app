//! Typed normalization before expression checking, identity resolution and sealing.
//! These public intermediate values are not executable or publishable snapshots.
use crate::DataSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRunbook {
    pub metadata: RunbookMetadata,
    pub execution: ExecutionSettings,
    pub defaults: TaskDefaults,
    pub inputs: DataSchema,
    pub fields: DataSchema,
    #[serde(
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub variables: BTreeMap<String, Value>,
    pub skills: BTreeMap<String, String>,
    /// Opaque subscriber data, never interpreted as expressions or dispatch policy.
    #[serde(
        serialize_with = "crate::schema::storage::serialize_optional_object",
        deserialize_with = "crate::schema::storage::deserialize_optional_object"
    )]
    pub agent_params: Option<serde_json::Map<String, Value>>,
    pub steps: Vec<Node>,
    pub finally: Vec<Node>,
    #[serde(
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub outputs: BTreeMap<String, Value>,
    pub report: Option<String>,
    /// LANG9b: what starts this runbook on its own (schedules and webhooks).
    /// Left out of the stored form when there are none.
    #[serde(default, skip_serializing_if = "Triggers::is_empty")]
    pub triggers: Triggers,
    /// Original data preserves opaque extensions at every source location.
    #[serde(
        serialize_with = "crate::schema::storage::serialize_value",
        deserialize_with = "crate::schema::storage::deserialize_value"
    )]
    pub source_json: Value,
    pub source_yaml: String,
}
/// LANG9b: `spec.triggers`. Only the published version's triggers act.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Triggers {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedule: Vec<ScheduleTrigger>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webhook: Vec<WebhookTrigger>,
}
impl Triggers {
    pub fn is_empty(&self) -> bool {
        self.schedule.is_empty() && self.webhook.is_empty()
    }
}
/// Starts a run at each tick of `cron` in `timezone`, with these inputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleTrigger {
    pub id: String,
    pub cron: String,
    pub timezone: String,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub inputs: BTreeMap<String, Value>,
    /// How long after a tick a late pass may still start it (the server was
    /// down); without it, a tick missed by more than a few minutes is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<Duration>,
}
/// Starts a run when its URL is called: `when` filters, `inputs` maps the
/// payload to the runbook's inputs, `dedupe` keeps a retried call from
/// starting a second run. Expressions see only `payload`, typed by its
/// declared schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookTrigger {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<DataSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub inputs: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunbookMetadata {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub owners: Vec<Recipient>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Duration {
    pub millis: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Actor {
    Human,
    Agent,
    Any,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FailurePolicy {
    Fail,
    Continue,
    Handoff,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub max_attempts: u32,
    pub backoff: Option<Duration>,
    /// LANG9: retry only failures of these types; empty retries any failure
    /// (as before). Omitted when empty, so retries keep their stored bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<FailureType>,
}
/// LANG9: what kind of failure a step's failure is, derived from what the
/// engine records (`timeout` for a timed-out step or attempt, `call_failed`
/// for a failed child runbook, `failed` for any other failure).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureType {
    Timeout,
    Failed,
    CallFailed,
}
impl FailureType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Failed => "failed",
            Self::CallFailed => "call_failed",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "timeout" => Some(Self::Timeout),
            "failed" => Some(Self::Failed),
            "call_failed" => Some(Self::CallFailed),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefaults {
    pub actor: Actor,
    pub timeout: Option<Duration>,
    pub retry: Retry,
    pub on_failure: FailurePolicy,
    /// LANG9: reminders and escalation every task inherits, field by field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followup: Option<Followup>,
    /// R0-3: `spec.defaults.agent.name`, the runner agent of every agent
    /// step that names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}
/// LANG9: follow-up for a step left open: reminders after `remind_after`
/// (then every `remind_every`), and escalation to `escalate_to` after
/// `escalate_after`, counted from when the step opened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Followup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_after: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_every: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_after: Option<Duration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalate_to: Vec<Recipient>,
}
impl Followup {
    pub fn is_empty(&self) -> bool {
        self.remind_after.is_none()
            && self.remind_every.is_none()
            && self.escalate_after.is_none()
            && self.escalate_to.is_empty()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSettings {
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub timeout: Option<Duration>,
    pub cleanup_timeout: Duration,
    pub concurrency: Option<Concurrency>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Concurrency {
    pub key: String,
    pub limit: u32,
    pub on_conflict: ConcurrencyPolicy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ConcurrencyPolicy {
    /// Wait in FIFO order for a slot (D24/D25), the default.
    Queue,
    /// LANG3: refuse the start when the pool has no free slot.
    Reject,
    /// LANG3: when the pool has no free slot, cancel every run holding or
    /// waiting for the key and admit the new one.
    CancelPrevious,
}
impl ConcurrencyPolicy {
    /// The source spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Reject => "reject",
            Self::CancelPrevious => "cancel_previous",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Recipient {
    User(String),
    Team(String),
}
/// LANG7: what a task's `assignee` means. `guidance` (the default) only
/// routes the work (Inbox, notifications, handoff target); `strict` lets
/// only the assignee claim and complete it. Strict binds people: whether
/// an agent may claim is still decided by `actor` alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentMode {
    #[default]
    Guidance,
    Strict,
}
impl AssignmentMode {
    pub fn is_guidance(&self) -> bool {
        *self == Self::Guidance
    }
    pub fn is_strict(&self) -> bool {
        *self == Self::Strict
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Assignee {
    Recipient(Recipient),
    Expression(String),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub source_path: String,
    pub title: Option<String>,
    pub guard: Option<String>,
    pub on_failure: FailurePolicy,
    pub timeout: Option<Duration>,
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_after: Option<Duration>,
    /// LANG9: free-form labels (`audience: customer`). The engine ignores
    /// them; the API returns them with the step. Omitted when empty, so a
    /// step without labels keeps its stored bytes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// LANG9: reminders and escalation (tasks, waits; reminders only on
    /// approvals). Omitted when absent, so steps keep their stored bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followup: Option<Followup>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum NodeKind {
    Task(Box<Task>),
    Group(Group),
    Parallel {
        children: Vec<Node>,
        fail_fast: bool,
        /// LANG2. Omitted when `all`, so join-all parallels keep their
        /// stored bytes.
        #[serde(default, skip_serializing_if = "Join::is_all")]
        join: Join,
    },
    Foreach(Foreach),
    Loop(Loop),
    Switch(Switch),
    Wait(Wait),
    Approval(Approval),
    Set(Assignments),
    End(End),
    /// LANG4: run a pinned version of another runbook as a child execution.
    Call(Box<Call>),
}
/// PRD §17: `call` nests at most this deep (a run, its child, grandchild
/// and great-grandchild).
pub const MAX_CALL_DEPTH: u32 = 3;
/// LANG4 `call`: start a child execution of a pinned version of another
/// runbook in the same project, map inputs in and the child's outputs out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    /// The child runbook's slug (`metadata.id`), as written.
    pub runbook: String,
    /// The pinned version number as written; `None` pins the latest
    /// published version when this runbook is compiled for publishing.
    pub version: Option<u32>,
    /// Child inputs: literals or `${{ }}` expressions, evaluated when the
    /// step starts.
    #[serde(
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub with: BTreeMap<String, Value>,
    /// The child's outputs this step exposes (`steps.<id>.outputs`),
    /// checked against the child's execution outputs when it finishes.
    pub outputs: DataSchema,
    /// The resolved child version, filled in by compilation from the
    /// compile context. `None` only before compilation.
    pub target: Option<CallTarget>,
}
/// The child version a `call` pins (LANG4), with what the compiler needs
/// to check depth and cycles without reading the catalog again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallTarget {
    pub runbook_id: String,
    pub version_id: String,
    pub version: u32,
    /// Nesting below the caller through this call: 1 plus the child's own
    /// deepest `call`.
    pub depth: u32,
    /// Every runbook slug a run of this call can start, the child's own
    /// included, sorted and unique.
    pub reaches: Vec<String>,
    /// Whether the child can reach a destructive task (its own or through
    /// its calls): the destructive-path policy treats the call as one.
    pub destructive: bool,
    /// An agent decision anywhere in the child's call tree. Older pins
    /// lack this summary and are treated conservatively at publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_decidable: Option<bool>,
}
/// The shape of one of a child's execution outputs, as far as its compiled
/// definition fixes it (`Any` when it does not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Any,
    Boolean,
    Integer,
    Number,
    String,
    Timestamp,
    List,
    Object,
}
/// What a compile context knows about one callable runbook version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallResolution {
    pub runbook_id: String,
    pub version_id: String,
    pub version: u32,
    /// The child's own deepest `call` (0 when it calls nothing).
    pub depth: u32,
    /// Every runbook slug the child can start (not the child itself).
    pub reaches: Vec<String>,
    /// Whether the child can reach a destructive task.
    pub destructive: bool,
    /// An agent decision anywhere in the child's call tree. Older pins
    /// lack this summary and are treated conservatively at publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_decidable: Option<bool>,
    /// The child's declared inputs: name to whether a start must supply it
    /// (required with no default).
    pub inputs: BTreeMap<String, bool>,
    /// The child's declared input schemas, to check literal `with` values.
    #[serde(default)]
    pub input_schemas: BTreeMap<String, DataSchema>,
    /// The child's execution outputs (`spec.outputs`) and their shapes.
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputKind>,
}
/// How a `parallel` joins its branches (PRD §5). `any` is `AtLeast(1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Join {
    /// Wait for every branch (`fail_fast` may cancel the rest on a failure).
    #[default]
    All,
    /// Done once this many branches succeeded; failed when that can no
    /// longer happen. Either way the open branches are cancelled.
    AtLeast(u32),
}
impl Join {
    pub fn is_all(&self) -> bool {
        *self == Self::All
    }
}

fn less_than_two<T>(v: &[T]) -> bool {
    v.len() < 2
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub actor: Actor,
    pub assignee: Option<Assignee>,
    /// LANG7: absent from stored definitions written before it existed and
    /// from every guidance task, so their bytes and hashes are unchanged.
    #[serde(default, skip_serializing_if = "AssignmentMode::is_guidance")]
    pub assignment: AssignmentMode,
    pub retry: Retry,
    pub instructions: Option<Instructions>,
    pub skill: Option<String>,
    /// LANG8: multiple skills per step. Omitted when fewer than 2 skills so
    /// single-skill and pre-LANG8 stored IR bytes and digests remain identical.
    #[serde(default, skip_serializing_if = "less_than_two")]
    pub skills: Vec<String>,
    #[serde(
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub parameters: BTreeMap<String, Value>,
    pub checklist: Vec<String>,
    pub check: bool,
    pub skippable: bool,
    pub destructive: bool,
    pub allow_auto_decision: bool,
    pub outputs: DataSchema,
    pub evidence: Evidence,
    pub agent: AgentHints,
    /// R0-3: the agent this step targets, resolved at publish from the
    /// project's agent directory. `None`: open to external agents (no
    /// name anywhere), as before runner agents existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_target: Option<AgentTarget>,
    /// R0-3: the step's own model profile and limits (within the agent's
    /// ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_limits: Option<AgentLimits>,
    /// R0-3: step `connections`: the credentials the step may use, a
    /// narrowing of the agent's. Names in source; credential IDs once
    /// resolved at publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<Vec<String>>,
}

/// R0-3: who works an agent step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum AgentTarget {
    /// A runner agent, by its directory ID (the name and the runner group
    /// as resolved at publish).
    Runner {
        agent_id: String,
        name: String,
        group_id: String,
    },
    /// `agent.name: external`: left open to external agents.
    External,
    /// A name not yet resolved: exists only between normalization and the
    /// directory resolution in `compile`; never stored.
    Named { name: String },
}
/// R0-3: a step's model profile and limits for its runner agent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// `agent.network`: an explicitly empty list denies every host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Vec<String>>,
}
impl AgentLimits {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    pub actor: Actor,
    pub assignee: Option<Assignee>,
    #[serde(default)]
    pub assignment: AssignmentMode,
    pub retry: Retry,
    pub instructions: Option<Instructions>,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(deserialize_with = "crate::schema::storage::deserialize_map")]
    pub parameters: BTreeMap<String, Value>,
    pub checklist: Vec<String>,
    pub check: bool,
    pub skippable: bool,
    pub destructive: bool,
    pub allow_auto_decision: bool,
    pub outputs: DataSchema,
    pub evidence: Evidence,
    pub agent: AgentHints,
    #[serde(default)]
    pub agent_target: Option<AgentTarget>,
    #[serde(default)]
    pub agent_limits: Option<AgentLimits>,
    #[serde(default)]
    pub connections: Option<Vec<String>>,
}

impl<'de> Deserialize<'de> for Task {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut raw = RawTask::deserialize(deserializer)?;
        if raw.skills.is_empty() {
            if let Some(ref s) = raw.skill {
                raw.skills = vec![s.clone()];
            }
        } else if raw.skill.is_none() {
            raw.skill = raw.skills.first().cloned();
        }
        Ok(Task {
            actor: raw.actor,
            assignee: raw.assignee,
            assignment: raw.assignment,
            retry: raw.retry,
            instructions: raw.instructions,
            skill: raw.skill,
            skills: raw.skills,
            parameters: raw.parameters,
            checklist: raw.checklist,
            check: raw.check,
            skippable: raw.skippable,
            destructive: raw.destructive,
            allow_auto_decision: raw.allow_auto_decision,
            outputs: raw.outputs,
            evidence: raw.evidence,
            agent: raw.agent,
            agent_target: raw.agent_target,
            agent_limits: raw.agent_limits,
            connections: raw.connections,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Instructions {
    Markdown(String),
    Audience {
        default: Option<String>,
        human: Option<String>,
        agent: Option<String>,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHints {
    #[serde(
        serialize_with = "crate::schema::storage::serialize_optional_value",
        deserialize_with = "crate::schema::storage::deserialize_optional_value"
    )]
    pub tools: Option<Value>,
    #[serde(
        serialize_with = "crate::schema::storage::serialize_optional_value",
        deserialize_with = "crate::schema::storage::deserialize_optional_value"
    )]
    pub commands: Option<Value>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum EvidenceKind {
    Link,
    Log,
    /// A finalized upload of any accepted type (UP1, D49).
    File,
    /// A finalized image upload (UP1, D49).
    Screenshot,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub required: Vec<EvidenceKind>,
    pub accepted: Option<Vec<EvidenceKind>>,
}
impl EvidenceKind {
    /// The kind as a runbook names it (`evidence.required: [log]`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Log => "log",
            Self::File => "file",
            Self::Screenshot => "screenshot",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub children: Vec<Node>,
    pub catch: Vec<Node>,
    pub finally: Vec<Node>,
    /// LANG9: `catch` runs only for failures of these types; others
    /// propagate as if there were no catch. Empty catches any failure (as
    /// before). Omitted when empty, so groups keep their stored bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catch_on: Vec<FailureType>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Foreach {
    pub items: String,
    pub alias: String,
    pub key: Option<String>,
    pub concurrency: u32,
    pub max_items: u32,
    pub children: Vec<Node>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loop {
    /// Exactly one of `until` and `while` is set (LANG1). Both serialize
    /// only when present, so `until` loops keep their stored bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#while: Option<String>,
    pub max_iterations: u32,
    pub delay: Option<Duration>,
    pub children: Vec<Node>,
}
/// A loop's termination test. Both are checked after each iteration's
/// children finish (PRD §5): `until` stops when true, `while` stops when
/// false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopCondition<'a> {
    Until(&'a str),
    While(&'a str),
}
impl<'a> LoopCondition<'a> {
    /// The source key holding the expression.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Until(_) => "until",
            Self::While(_) => "while",
        }
    }
    pub fn source(&self) -> &'a str {
        match self {
            Self::Until(s) | Self::While(s) => s,
        }
    }
    /// Whether a condition value means the loop's termination condition
    /// is met (`satisfied`).
    pub fn satisfied_by(&self, value: bool) -> bool {
        match self {
            Self::Until(_) => value,
            Self::While(_) => !value,
        }
    }
}
impl Loop {
    /// The loop's one condition. Normalization and snapshot admission both
    /// refuse a loop with neither or both (`spec.invalid_shape`,
    /// `spec.invalid_ir`), so a loop that reached the IR has exactly one.
    ///
    /// # Panics
    /// On a hand-built `Loop` with neither or both conditions.
    pub fn condition(&self) -> LoopCondition<'_> {
        match (&self.until, &self.r#while) {
            (Some(u), None) => LoopCondition::Until(u),
            (None, Some(w)) => LoopCondition::While(w),
            _ => panic!("a loop has exactly one of `until` and `while`"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Switch {
    pub cases: Vec<Case>,
    pub default: Vec<Node>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub when: String,
    pub source_path: String,
    pub children: Vec<Node>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum WaitKind {
    For(Duration),
    Until(String),
    Signal(String),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wait {
    pub kind: WaitKind,
    pub timeout: Option<Duration>,
    /// LANG9: the schema a signal's payload must match (signal waits only).
    /// A valid payload becomes the step's outputs. Omitted when absent, so
    /// waits without one keep their stored bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<crate::DataSchema>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub prompt: String,
    pub options: Vec<ApprovalOption>,
    /// Human decisions that unlock destructive runner steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approves: Option<Vec<String>>,
    pub approvers: Vec<Recipient>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_completers_of: Vec<String>,
    pub min: u32,
    pub timeout: Option<Duration>,
    pub on_timeout: ApprovalTimeout,
    pub escalate_to: Option<Vec<Recipient>>,
    pub escalation_timeout: Option<Duration>,
    pub on_expired: ExpiredPolicy,
    pub allow_agents: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_target: Option<AgentTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_limits: Option<AgentLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_after: Option<Duration>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalOption {
    pub id: String,
    pub title: String,
    pub auto: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ApprovalTimeout {
    Escalate,
    Decide(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ExpiredPolicy {
    Pause,
    Fail,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignments {
    #[serde(
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub variables: BTreeMap<String, Value>,
    #[serde(
        serialize_with = "crate::schema::storage::serialize_map",
        deserialize_with = "crate::schema::storage::deserialize_map"
    )]
    pub fields: BTreeMap<String, Value>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum EndStatus {
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct End {
    pub status: EndStatus,
    pub message: Option<String>,
}
