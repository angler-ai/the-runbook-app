//! Runbook memory storage (PRD §7 "Runbook memory", §14; D40–D42). Named
//! `memory_entries` so it never clashes with the in-memory adapters
//! (`store/memory.rs`). T0 froze the trait and records; MEM1 adds the use
//! cases (create, patch, search, propose, read-for-step) and the in-memory
//! implementation (`data`), MEM2 the Postgres one.
//!
//! Memory is untrusted data. Nothing here feeds the engine or instructions:
//! the engine never reads an entry, and hosts hand entries to agents only in
//! a labeled data block ([`step_block`]). Every write path in this module
//! redacts titles, bodies and tags (`redact.rs`) before a store sees them.
pub(crate) mod data;
mod usecases;
use crate::{StoreError, catalog::TenantScope};
use runbook_model::{
    ExecutionId, MemoryBody, MemoryId, MemoryScope, MemorySource, MemoryStatus, RunbookId,
};
use serde::{Deserialize, Serialize};
use std::future::Future;
pub use usecases::*;

/// PRD §7: active entries per runbook (per project for project entries).
/// Proposals have their own quota and archived history counts toward
/// neither. Stores check both atomically.
pub const MEMORY_ENTRIES_PER_RUNBOOK: usize = 500;
/// Pending proposals per runbook (per project for project entries), apart
/// from the active quota, so proposals can never fill the active slots.
pub const MEMORY_PROPOSALS_PER_RUNBOOK: usize = 100;
/// The quota an entry of `status` counts against (archived: none).
pub fn quota_for(status: MemoryStatus) -> usize {
    match status {
        MemoryStatus::Active => MEMORY_ENTRIES_PER_RUNBOOK,
        MemoryStatus::Proposed => MEMORY_PROPOSALS_PER_RUNBOOK,
        MemoryStatus::Archived => usize::MAX,
    }
}
/// MEM6 review fix 6: the last active slots of a runbook (or project) that
/// only people can fill, so agents can never crowd a person's save out.
pub const MEMORY_PEOPLE_RESERVE: usize = 50;
/// The quota an agent's write of `status` counts against: the active quota
/// less the people's reserve; proposals as for anyone.
pub fn agent_quota(status: MemoryStatus) -> usize {
    match status {
        MemoryStatus::Active => MEMORY_ENTRIES_PER_RUNBOOK - MEMORY_PEOPLE_RESERVE,
        other => quota_for(other),
    }
}
/// MEM6 re-check fix 1: edits an agent, a service or a run may make to one
/// entry per UTC day of the store clock, all of them together. With 50
/// revisions kept, agents alone cannot push a day's history out.
pub const MEMORY_AGENT_EDITS_PER_DAY: u32 = 20;
const DAY_MS: i64 = 86_400_000;
/// MEM6 review fix 2: revisions kept per entry besides the first. A write
/// past them removes the oldest of the middle and counts it in
/// `revisions_pruned`.
pub const MEMORY_REVISIONS_KEPT: u32 = 50;
/// PRD §7 and D40: memory writes per execution (proposals, live saves and,
/// since MEM6 review fix 2, edits), counted from execution events inside
/// the execution transaction, never by the catalog.
pub const MEMORY_PROPOSALS_PER_EXECUTION: usize = 20;
/// D41: active entries `get_step` returns for one step ID.
pub const MEMORY_ENTRIES_PER_STEP: usize = 5;

/// The current state of one entry. `revision` starts at 1 and grows by one on
/// every change; it is the `If-Match` value and what reads record (D41).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntryRecord {
    pub id: MemoryId,
    pub scope: TenantScope,
    /// One runbook, or the whole project.
    pub memory_scope: MemoryScope,
    /// Relevance filter for `get_step`; never identity or authorization.
    pub step_id: Option<String>,
    pub title: String,
    /// Already redacted; at most 4 KB (the type enforces it).
    pub body: MemoryBody,
    pub tags: Vec<String>,
    pub status: MemoryStatus,
    pub source: MemorySource,
    pub revision: u32,
    /// Store-clock expiry; an expired entry is never returned to agents.
    pub expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Set on the archived marker a catalog writes for a proposal it could
    /// not store (D1's applier racing a full quota): the reason code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<String>,
    /// MEM6 review fix 1: the label of whoever made the current revision
    /// (an edit leaves `source`, the creator, as it was).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_writer: Option<String>,
    /// A person locked it: agents cannot edit it (MEM6 review fix 3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// Revisions pruned from the middle of the history (review fix 2).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revisions_pruned: u32,
    /// The newest revision whose content a person wrote (an edit, restore
    /// or accept); pruning always keeps it (re-check fix 1). 0: none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub person_revision: u32,
    /// The store-clock UTC day of `agent_edits` (re-check fix 1).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub agent_edit_day: i64,
    /// Edits by agents, services and runs on `agent_edit_day`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub agent_edits: u32,
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What produced a revision. Reject keeps the proposal as archived history
/// (the model's `MemoryPatchRequest` contract), so it is its own action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    Create,
    Propose,
    Edit,
    Accept,
    Reject,
    Archive,
    /// A proposal the catalog could not store (its quota filled after the
    /// execution committed it); recorded as an archived marker.
    Drop,
    /// MEM6 review fixes 3 and 6 (people only).
    Lock,
    Unlock,
    Restore,
}

/// One immutable prior state. Edits never rewrite history (PRD §7 "Safety").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRevisionRecord {
    pub action: MemoryAction,
    /// Non-secret principal label (`human:usr_…`, `agent:usr_…`, `service:tok_…`).
    pub actor: String,
    pub at_ms: i64,
    /// The entry as it stood after this revision.
    pub entry: MemoryEntryRecord,
}

/// A change fenced by the expected revision. People send any of them; an
/// agent only edits (MEM6, [`MemoryWriter`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryPatch {
    Edit {
        title: String,
        body: MemoryBody,
        tags: Vec<String>,
        step_id: Option<String>,
        expires_at_ms: Option<i64>,
    },
    /// Proposed to active.
    Accept,
    /// Proposed to archived, recorded as a rejection.
    Reject,
    /// Proposed or active to archived.
    Archive,
    /// People only: agents can no longer edit it.
    Lock,
    Unlock,
    /// People only: a prior revision's content, current again; active or
    /// archived to active. The step ID is not checked again.
    Restore {
        title: String,
        body: MemoryBody,
        tags: Vec<String>,
        step_id: Option<String>,
        expires_at_ms: Option<i64>,
    },
}

/// D40: a run's memory write travels as a derived row of the execution
/// commit that recorded `MemoryProposed` (a proposal) or `MemorySaved` (a
/// live save, MEM6). The catalog applies it idempotently by (execution,
/// sequence); a replay or an older sequence writes nothing. The name is
/// D40's; the row carries either kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposal {
    pub execution_id: ExecutionId,
    pub sequence: u64,
    /// Status `proposed` or `active`, source the writing step run,
    /// revision 1.
    pub entry: MemoryEntryRecord,
}

/// The revision action that stores a run's derived row: `create` for a live
/// save, `propose` for a proposal.
pub fn run_action(entry: &MemoryEntryRecord) -> MemoryAction {
    match entry.status {
        MemoryStatus::Active => MemoryAction::Create,
        _ => MemoryAction::Propose,
    }
}

/// Search filters. Every filter narrows; `within` lists which scopes to read
/// (an execution credential reads its own runbook plus its project). An
/// empty `within` reads every scope of the tenant (the project's runbooks and
/// the project itself), which only project readers may ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub within: Vec<MemoryScope>,
    /// Full-text match over title and body (`tsvector` on Postgres, FTS5 on
    /// D1): every word of the query must be a word of the entry, compared
    /// case-insensitively without stemming ([`text_words`]).
    pub text: Option<String>,
    /// Every tag must be present.
    pub tags: Vec<String>,
    pub step_id: Option<String>,
    pub status: Option<MemoryStatus>,
    /// False hides entries expired at the store clock.
    pub include_expired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecordPage {
    /// Newest first (updated time, then ID).
    pub items: Vec<MemoryEntryRecord>,
    /// Opaque; bound to scope and filters.
    pub next_cursor: Option<String>,
}

/// Tenant-scoped memory storage. Every call filters by `scope`: a foreign
/// entry reads as absent (`None` or `NotFound`), never as forbidden.
/// Implementations keep each call atomic; futures need not be Send.
pub trait MemoryStore {
    /// The store clock that stamps entries and decides expiry.
    fn memory_time_ms(&self) -> impl Future<Output = Result<i64, StoreError>>;
    /// Insert an active entry or a proposed one (written outside a run) at
    /// revision 1, stamped with the store clock, unless its
    /// runbook (or the project, for project entries) already holds `limit`
    /// entries of the same status (`InvalidBatch("memory.limit_reached")`).
    /// The ID is host-supplied; a duplicate ID is `InvalidBatch("memory.duplicate")`.
    fn create_memory(
        &self,
        scope: &TenantScope,
        entry: MemoryEntryRecord,
        actor: &str,
        limit: usize,
    ) -> impl Future<Output = Result<MemoryEntryRecord, StoreError>>;
    /// Apply one D40 derived row (a proposal or a live save). True when
    /// applied now; false for a replay of the same (execution, sequence) or
    /// an older sequence of that execution. The per-runbook `limit` of the
    /// entry's status ([`quota_for`]) applies as in `create_memory`.
    fn apply_memory_proposal(
        &self,
        scope: &TenantScope,
        proposal: MemoryProposal,
        limit: usize,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn get_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> impl Future<Output = Result<Option<MemoryEntryRecord>, StoreError>>;
    /// Every kept revision, oldest first. Empty for an absent or foreign
    /// entry. At most `MEMORY_REVISIONS_KEPT + 1` rows.
    fn memory_revisions(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> impl Future<Output = Result<Vec<MemoryRevisionRecord>, StoreError>>;
    /// One revision by key; `None` when absent, foreign or pruned.
    fn memory_revision(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        revision: u32,
    ) -> impl Future<Output = Result<Option<MemoryRevisionRecord>, StoreError>>;
    /// Up to `limit` revisions after revision `after` (exclusive), oldest
    /// first.
    fn memory_revision_page(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        after: u32,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<MemoryRevisionRecord>, StoreError>>;
    /// Apply a patch only if the stored revision equals `expected_revision`
    /// (`InvalidBatch("memory.conflict")`, the route's 412); an illegal status
    /// change is `InvalidBatch("memory.invalid_transition")`; absent is NotFound.
    /// The new state comes from [`next_revision`]. Accepting or restoring
    /// checks the active quota (`MEMORY_ENTRIES_PER_RUNBOOK`) as a create
    /// does (`memory.limit_reached`). Revisions [`pruned_through`] names are
    /// removed, and [`notice_for`]'s event commits with the write.
    fn patch_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        expected_revision: u32,
        patch: MemoryPatch,
        actor: &str,
    ) -> impl Future<Output = Result<MemoryEntryRecord, StoreError>>;
    fn search_memory(
        &self,
        scope: &TenantScope,
        query: &MemoryQuery,
        cursor: Option<String>,
        limit: usize,
    ) -> impl Future<Output = Result<MemoryRecordPage, StoreError>>;
    /// D41: up to `limit` active, unexpired entries for `step_id` in this
    /// runbook and its project, in [`step_order`]: those a person wrote
    /// last first (MEM6 review fix 5), then newest first.
    fn memory_for_step(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        step_id: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<MemoryEntryRecord>, StoreError>>;
}

/// MEM6 review fix 5: the order of a step's memory block. Entries a person
/// wrote last come first, so agents' writes can never push them out; then
/// newest first.
pub fn step_order(a: &MemoryEntryRecord, b: &MemoryEntryRecord) -> std::cmp::Ordering {
    by_person(b)
        .cmp(&by_person(a))
        .then_with(|| newest_first(a, b))
}

/// Whether a person made the entry's current revision.
pub fn by_person(entry: &MemoryEntryRecord) -> bool {
    entry
        .last_writer
        .as_deref()
        .is_some_and(|w| w.starts_with("human:"))
}

/// The quota a run's derived row counts against: an agent's (review fix 6).
pub fn run_quota(status: MemoryStatus) -> usize {
    agent_quota(status)
}

/// The state after one patch, and its revision action: the one rule every
/// store applies (the status edge, the content, the lock, the writer and
/// the pruning count). The stores add the fence, the quota and the writes.
pub fn next_revision(
    stored: &MemoryEntryRecord,
    patch: MemoryPatch,
    actor: &str,
    now: i64,
) -> Result<(MemoryEntryRecord, MemoryAction), StoreError> {
    use runbook_model::MemoryActorKind as Kind;
    let kind = actor_kind(actor);
    let status = transition(stored.status, &patch)
        .ok_or(StoreError::InvalidBatch("memory.invalid_transition"))?;
    // Re-check fix 6: a drop marker records a refused write; it is never
    // restored (a person saves its content anew instead).
    if matches!(patch, MemoryPatch::Restore { .. }) && stored.dropped.is_some() {
        return Err(StoreError::InvalidBatch("memory.invalid_transition"));
    }
    let action = action_of(&patch);
    let mut next = stored.clone();
    if action == MemoryAction::Edit && kind != Kind::Person {
        // Re-check fix 2: inside every store's revision fence, so an edit
        // fenced on the revision a lock produced is refused too.
        if stored.locked {
            return Err(StoreError::InvalidBatch("memory.locked"));
        }
        if matches!(kind, Kind::Agent | Kind::Service | Kind::Execution) {
            if !agent_edit_allowed(stored, now) {
                return Err(StoreError::InvalidBatch("memory.edit_limit"));
            }
            let day = now.div_euclid(DAY_MS);
            next.agent_edits = if stored.agent_edit_day == day {
                stored.agent_edits + 1
            } else {
                1
            };
            next.agent_edit_day = day;
        }
    }
    match patch {
        MemoryPatch::Edit {
            title,
            body,
            tags,
            step_id,
            expires_at_ms,
        }
        | MemoryPatch::Restore {
            title,
            body,
            tags,
            step_id,
            expires_at_ms,
        } => {
            next.title = title;
            next.body = body;
            next.tags = tags;
            next.step_id = step_id;
            next.expires_at_ms = expires_at_ms;
        }
        MemoryPatch::Lock => next.locked = true,
        MemoryPatch::Unlock => next.locked = false,
        MemoryPatch::Accept | MemoryPatch::Reject | MemoryPatch::Archive => {}
    }
    next.status = status;
    next.revision = stored
        .revision
        .checked_add(1)
        .ok_or(StoreError::InvalidBatch("memory.revision_exhausted"))?;
    next.updated_at_ms = now;
    // Re-check fix 5: only content a writer produced names them; locking,
    // unlocking, rejecting and archiving do not.
    if matches!(
        action,
        MemoryAction::Edit | MemoryAction::Restore | MemoryAction::Accept
    ) {
        next.last_writer = Some(actor.to_owned());
        if kind == Kind::Person {
            next.person_revision = next.revision;
        }
    }
    // Revisions 2..=through go, except the person's (re-check fix 1).
    let through = next.revision.saturating_sub(MEMORY_REVISIONS_KEPT);
    let kept_person = (2..=through).contains(&next.person_revision);
    next.revisions_pruned = through.saturating_sub(1) - u32::from(kept_person);
    Ok((next, action))
}

/// Whether agents, services and runs may still edit `entry` today.
pub fn agent_edit_allowed(entry: &MemoryEntryRecord, now: i64) -> bool {
    entry.agent_edit_day != now.div_euclid(DAY_MS) || entry.agent_edits < MEMORY_AGENT_EDITS_PER_DAY
}

/// The last revision a write of `next` prunes: every revision after the
/// first up to and including it goes, except `next.person_revision`.
/// `None` while nothing needs pruning.
pub fn pruned_through(next: &MemoryEntryRecord) -> Option<u32> {
    let last = next.revision.checked_sub(MEMORY_REVISIONS_KEPT)?;
    (last > 1).then_some(last)
}

/// MEM6 review fix 3: the webhook a catalog write by an agent (an agent-kind
/// or service token) commits with: `memory.saved` or `memory.proposed` for
/// a save, `memory.edited` for an edit. Identifiers only. A person's write
/// notifies nobody, and a run's writes notify through its own events.
pub fn notice_for(
    entry: &MemoryEntryRecord,
    action: MemoryAction,
    actor: &str,
) -> Option<crate::webhooks::ProjectEvent> {
    use runbook_model::MemoryActorKind as Kind;
    if !matches!(actor_kind(actor), Kind::Agent | Kind::Service) {
        return None;
    }
    let event_type = match action {
        MemoryAction::Create => "memory.saved",
        MemoryAction::Propose => "memory.proposed",
        MemoryAction::Edit => "memory.edited",
        _ => return None,
    };
    let runbook_id = match &entry.memory_scope {
        MemoryScope::Runbook { runbook_id } => Some(runbook_id.clone()),
        MemoryScope::Project => None,
    };
    let revision = entry.revision.to_string();
    Some(crate::webhooks::ProjectEvent {
        event_id: crate::webhooks::derived_id(
            "evt_",
            entry.updated_at_ms,
            &[entry.id.as_str(), &revision, event_type],
        ),
        event_type: event_type.into(),
        at_ms: entry.updated_at_ms,
        runbook_id: runbook_id.clone(),
        data: serde_json::json!({
            "memory_id": entry.id,
            "revision": entry.revision,
            "runbook": runbook_id,
            "actor_kind": actor_kind(actor),
            "actor": actor,
        }),
    })
}

#[cfg(test)]
mod revision_rules {
    use super::*;
    use runbook_model::MemorySource;

    fn archived_marker() -> MemoryEntryRecord {
        MemoryEntryRecord {
            id: "mem_00000000000000000000000001".parse().unwrap(),
            scope: TenantScope {
                org_id: "org_00000000000000000000000001".parse().unwrap(),
                project_id: "prj_00000000000000000000000001".parse().unwrap(),
            },
            memory_scope: MemoryScope::Project,
            step_id: None,
            title: "Dropped".into(),
            body: "body".to_owned().try_into().unwrap(),
            tags: vec![],
            status: MemoryStatus::Archived,
            source: MemorySource::Service {
                token_id: "tok_00000000000000000000000001".parse().unwrap(),
            },
            revision: 1,
            expires_at_ms: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            dropped: Some("memory.limit_reached".into()),
            last_writer: Some("agent:x".into()),
            locked: false,
            revisions_pruned: 0,
            person_revision: 0,
            agent_edit_day: 0,
            agent_edits: 0,
        }
    }
    fn restore() -> MemoryPatch {
        MemoryPatch::Restore {
            title: "t".into(),
            body: "b".to_owned().try_into().unwrap(),
            tags: vec![],
            step_id: None,
            expires_at_ms: None,
        }
    }

    /// Re-check fix 6: a drop marker records a refused write; restoring it
    /// is refused (a person saves its content anew instead).
    #[test]
    fn a_drop_marker_is_never_restored() {
        assert_eq!(
            next_revision(&archived_marker(), restore(), "human:usr_1", 1),
            Err(StoreError::InvalidBatch("memory.invalid_transition"))
        );
        let mut archived = archived_marker();
        archived.dropped = None;
        assert!(next_revision(&archived, restore(), "human:usr_1", 1).is_ok());
    }

    /// Re-check fix 2: an agent's edit applies only to the revision its
    /// checks read; naming a later one (a lock, an archive or a person's
    /// expiry may have landed there) is a conflict before anything is
    /// written.
    #[test]
    fn an_agent_edit_names_the_revision_it_checked() {
        let mut active = archived_marker();
        active.dropped = None;
        active.status = MemoryStatus::Active;
        let request = runbook_model::MemoryPatchRequest::Edit {
            entry: runbook_model::SaveMemoryRequest {
                title: "t".into(),
                body_markdown: "b".to_owned().try_into().unwrap(),
                tags: vec![],
                step_id: None,
                expires_at: None,
                propose: false,
            },
        };
        let refused = agent_edit(&active, &request, None, 2, 1).unwrap_err();
        assert_eq!(refused.code(), "memory.conflict");
        assert!(agent_edit(&active, &request, None, 1, 1).is_ok());
    }

    /// Re-check fix 5: only content a writer produced moves `last_writer`
    /// and the person's kept revision; locking does not.
    #[test]
    fn locks_leave_the_last_writer() {
        let mut active = archived_marker();
        active.dropped = None;
        active.status = MemoryStatus::Active;
        let (locked, _) = next_revision(&active, MemoryPatch::Lock, "human:usr_1", 1).unwrap();
        assert_eq!(
            (locked.last_writer.as_deref(), locked.person_revision),
            (Some("agent:x"), 0)
        );
        let (restored, _) = next_revision(&locked, restore(), "human:usr_1", 2).unwrap();
        assert_eq!(
            (restored.last_writer.as_deref(), restored.person_revision),
            (Some("human:usr_1"), 3)
        );
    }
}
