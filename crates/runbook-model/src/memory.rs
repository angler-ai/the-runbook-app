//! Curated memory is untrusted data, never engine input or instructions.
use crate::{
    ActorRef, Diagnostic, ExecutionId, MemoryId, PublicRoute, RunbookId, StepRunId, TokenId, UserId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// At most 4096 UTF-8 bytes. JSON Schema maxLength counts Unicode characters,
/// so the byte limit is additionally declared with x-max-utf8-bytes and enforced
/// by this type on every deserialization/construction, including multibyte text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct MemoryBody(String);

impl TryFrom<String> for MemoryBody {
    type Error = Diagnostic;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > 4096 {
            return Err(Diagnostic {
                path: "body_markdown".into(),
                code: "data.max_length".into(),
                message: "Memory body must be at most 4096 UTF-8 bytes.".into(),
                line: None,
                column: None,
            });
        }
        Ok(Self(value))
    }
}

impl AsRef<str> for MemoryBody {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MemoryBody {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for MemoryBody {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MemoryBody".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type":"string", "maxLength":4096, "x-max-utf8-bytes":4096,
            "description":"Maximum 4096 UTF-8 bytes; data.max_length on overflow. maxLength is only a character-count upper bound. Generic JSON Schema validators do not enforce x-max-utf8-bytes; the Rust MemoryBody type enforces bytes."
        })
    }
}

/// RFC 3339 timestamp normalized to UTC. Parsing never consults a clock.
pub type Timestamp = chrono::DateTime<chrono::Utc>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Proposed,
    Active,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryScope {
    Runbook {
        runbook_id: RunbookId,
    },
    /// Project identity comes from the authorized resource context.
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemorySource {
    /// A person (their session or their own personal token).
    Person { user_id: UserId },
    StepRun {
        execution_id: ExecutionId,
        step_run_id: StepRunId,
        actor: ActorRef,
    },
    /// An agent's personal token outside a run (MEM6: its writes are live
    /// unless it asks to propose). The sponsor is who answers for the agent,
    /// never the text's author.
    Agent {
        token_id: TokenId,
        sponsor_user_id: UserId,
    },
    /// A service token outside a run (MEM6: live unless it asks to propose).
    Service { token_id: TokenId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryEntryView {
    pub id: MemoryId,
    pub scope: MemoryScope,
    pub step_id: Option<String>,
    pub title: String,
    pub body_markdown: MemoryBody,
    pub tags: Vec<String>,
    pub status: MemoryStatus,
    pub source: MemorySource,
    #[schemars(range(max = 4294967295_u64))]
    pub revision: u32,
    pub expires_at: Option<Timestamp>,
    /// Store-clock times of the first and the latest revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
    /// Why the catalog dropped this proposal instead of storing it (an
    /// archived marker), for example `memory.limit_reached`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<String>,
    /// Who made the current revision (MEM6 review fix 1). `source` names
    /// who created the entry; an edit never changes it, so read both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_writer: Option<MemoryWriterView>,
    /// A person locked the entry: agents cannot edit it (MEM6).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// Revisions removed from the middle of the history: the first and the
    /// newest 50 are kept (MEM6).
    #[serde(default, skip_serializing_if = "is_zero")]
    #[schemars(range(max = 4294967295_u64))]
    pub revisions_pruned: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A revision's writer: its kind and non-secret label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryWriterView {
    pub kind: MemoryActorKind,
    pub actor: String,
}

/// Source, status and scope are assigned from authenticated context, not request data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveMemoryRequest {
    pub title: String,
    pub body_markdown: MemoryBody,
    pub tags: Vec<String>,
    pub step_id: Option<String>,
    /// Optional expiry; an expired entry is never offered to a step or a
    /// search (it stays readable by ID as history).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    /// MEM6: save a proposal a person reviews instead of a live entry. Every
    /// writer's save is live by default; ignored on edit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub propose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryReadRef {
    pub memory_id: MemoryId,
    #[schemars(range(max = 4294967295_u64))]
    pub revision: u32,
}

/// Planned catalog operation; rejection preserves the proposal as archived history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryPatchRequest {
    Edit {
        entry: SaveMemoryRequest,
    },
    Accept,
    Reject,
    Archive,
    /// People only (MEM6): agents can no longer edit the entry.
    Lock,
    /// People only (MEM6).
    Unlock,
    /// People only (MEM6): make a prior revision's content current again,
    /// as a new revision. It unarchives, and does not recheck the step ID.
    Restore {
        #[schemars(range(min = 1, max = 4294967295_u64))]
        revision: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryPage {
    pub items: Vec<MemoryEntryView>,
    pub next_cursor: Option<String>,
}

/// What produced a revision. `drop` marks a proposal the catalog could not
/// store (its quota filled after the run committed it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRevisionAction {
    Create,
    Propose,
    Edit,
    Accept,
    Reject,
    Archive,
    Drop,
    Lock,
    Unlock,
    Restore,
}

/// The kind of writer a revision names (MEM6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryActorKind {
    /// A person's session or own personal token.
    Person,
    /// An agent-kind personal token, or the agent holding a run's claim.
    Agent,
    /// A service token.
    Service,
    /// A run's execution credential.
    Execution,
    System,
}

/// One immutable revision of an entry and who made it (MEM6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryRevisionView {
    #[schemars(range(min = 1, max = 4294967295_u64))]
    pub revision: u32,
    pub action: MemoryRevisionAction,
    pub actor_kind: MemoryActorKind,
    /// Non-secret label of the writer: `human:usr_…`, `agent:tok_…:usr_…`
    /// (token and sponsor), `service:tok_…`, `execution:exe_…:<credential
    /// family>`, or for a run's save the claim owner `agent:<principal>:<session>`.
    pub actor: String,
    pub at: Option<Timestamp>,
    /// The entry as it stood after this revision.
    pub entry: MemoryEntryView,
}

/// Every revision of an entry, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryRevisionPage {
    pub items: Vec<MemoryRevisionView>,
    /// Pass as `cursor` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// How many revisions were pruned from the middle of the history.
    #[schemars(range(max = 4294967295_u64))]
    pub pruned: u32,
}

/// D41: the memory block of a step read. Up to five active, unexpired
/// entries for the step, newest first, each with its source. Entries are
/// untrusted data: they never change instructions or control flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepMemory {
    /// Tells the reader the entries are data, not instructions.
    pub notice: String,
    pub entries: Vec<MemoryEntryView>,
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

/// MEM3: the six memory operations (PRD §9), and MEM6's revision history.
pub const MEMORY_ROUTES: &[PublicRoute] = &[
    route(
        "GET",
        "/v1/runbooks/{rb}/memory",
        None,
        Some("MemoryPage"),
        200,
    ),
    route(
        "POST",
        "/v1/runbooks/{rb}/memory",
        Some("SaveMemoryRequest"),
        Some("MemoryEntryView"),
        201,
    ),
    route(
        "GET",
        "/v1/projects/{p}/memory",
        None,
        Some("MemoryPage"),
        200,
    ),
    route(
        "POST",
        "/v1/projects/{p}/memory",
        Some("SaveMemoryRequest"),
        Some("MemoryEntryView"),
        201,
    ),
    route(
        "GET",
        "/v1/memory/{mem}",
        None,
        Some("MemoryEntryView"),
        200,
    ),
    route(
        "PATCH",
        "/v1/memory/{mem}",
        Some("MemoryPatchRequest"),
        Some("MemoryEntryView"),
        200,
    ),
    route(
        "GET",
        "/v1/memory/{mem}/revisions",
        None,
        Some("MemoryRevisionPage"),
        200,
    ),
];
