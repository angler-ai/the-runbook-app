//! The D1 half of `CatalogStore` (B2.2a, B3.1–B3.3): drafts, runbook
//! identity and publication, skill versions, run summaries and the inbox,
//! routing reads and the project outbox. Every handle is bound to one
//! tenant at construction; every query filters by it.
//!
//! Run summaries and assignments are written only by the derived-row
//! applier (`applier.rs`); admission lives in `admission.rs`, run search in
//! `search.rs`. Execution state itself is the ExecutionDO's (CF2).
use crate::sql::{Columns, D1, D1Error, Row, Statement, guard, json, prepare, uint};
use runbook_core::{
    CommitResult, StoreError,
    catalog::{
        Assignment, DraftRevision, RunSummary, RunbookOrigin, RunbookRecord, SkillVersion,
        TenantScope, VersionRecord,
    },
    publish::PreparedPublication,
    webhooks::{ProjectEvent, ProjectOutbox},
};
use runbook_model::{
    DraftView, ExecutionId, OrganizationId, ProjectId, PublishedVersionView, RunbookId, StepRunId,
    VersionId, VersionSelector, VersionView,
};
use std::num::NonZeroU32;

/// One tenant's catalog on D1. Clones share the database.
#[derive(Clone)]
pub struct D1Catalog<D: D1> {
    pub(crate) db: D,
    pub(crate) scope: TenantScope,
    /// CF11: where runbook source goes instead of D1 (`sources`); none
    /// keeps it inline.
    pub(crate) sources: Option<std::rc::Rc<dyn crate::sources::SourceBlobs>>,
}
impl<D: D1> D1Catalog<D> {
    pub fn new(db: D, scope: TenantScope) -> Self {
        Self {
            db,
            scope,
            sources: None,
        }
    }
    /// Keep runbook source in `sources` (the project's content-addressed
    /// blobs), with only its digest and size in D1 (CF11).
    #[must_use]
    pub fn with_sources(mut self, sources: std::rc::Rc<dyn crate::sources::SourceBlobs>) -> Self {
        self.sources = Some(sources);
        self
    }
    pub fn sources(&self) -> Option<&dyn crate::sources::SourceBlobs> {
        self.sources.as_deref()
    }
    async fn store_source(&self, text: &str) -> Result<crate::sources::StoredSource, StoreError> {
        crate::sources::store_source(self.sources(), text).await
    }
    async fn stitch(&self, inline: String, digest: Option<Vec<u8>>) -> Result<String, StoreError> {
        crate::sources::stitch_source(self.sources(), inline, digest).await
    }
    pub fn scope(&self) -> &TenantScope {
        &self.scope
    }
    pub fn db(&self) -> &D {
        &self.db
    }
    /// The same database seen by another tenant.
    #[must_use]
    pub fn scoped(&self, scope: TenantScope) -> Self {
        Self {
            db: self.db.clone(),
            sources: (scope == self.scope)
                .then(|| self.sources.clone())
                .flatten(),
            scope,
        }
    }
    pub(crate) fn org(&self) -> &str {
        self.scope.org_id.as_str()
    }
    pub(crate) fn project(&self) -> &str {
        self.scope.project_id.as_str()
    }
}

pub(crate) fn err(error: D1Error) -> StoreError {
    crate::store_error(error)
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    row: &Row,
    index: usize,
) -> Result<T, StoreError> {
    row.json(index).map_err(err)
}
pub(crate) fn label<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        _ => Err(StoreError::Backend("d1.encode")),
    }
}
pub(crate) fn count(n: usize) -> Result<crate::Value, StoreError> {
    uint(u64::try_from(n).unwrap_or(u64::MAX).min(1 << 40)).map_err(err)
}
fn origin_label(origin: RunbookOrigin) -> &'static str {
    match origin {
        RunbookOrigin::Ui => "ui",
        RunbookOrigin::Git => "git",
    }
}
fn origin_of(text: &str) -> Result<RunbookOrigin, StoreError> {
    match text {
        "ui" => Ok(RunbookOrigin::Ui),
        "git" => Ok(RunbookOrigin::Git),
        _ => Err(StoreError::Backend("d1.invalid_runbook_origin")),
    }
}
fn draft_source(row: &Row, index: usize) -> Result<String, StoreError> {
    String::from_utf8(row.blob(index).map_err(err)?.to_vec())
        .map_err(|_| StoreError::Backend("catalog.invalid_draft_utf8"))
}
fn draft_view(id: &RunbookId, source: String, revision: u64) -> Result<DraftView, StoreError> {
    Ok(DraftView {
        runbook_id: id.clone(),
        source,
        revision: revision
            .try_into()
            .map_err(|_| StoreError::Backend("d1.invalid_revision"))?,
    })
}

pub(crate) const VERSION_COLUMNS: &str = "version_id, number, branch, commit_sha, semantic_hash, \
     snapshot_digest, snapshot_size, source, pins";
/// A version row whose columns start at `at` (`VERSION_COLUMNS` order).
pub(crate) fn version_at(row: &Row, at: usize) -> Result<VersionRecord, StoreError> {
    Ok(VersionRecord {
        version_id: row.parse(at).map_err(err)?,
        number: row
            .optional_integer(at + 1)
            .map_err(err)?
            .map(|n| {
                u32::try_from(n)
                    .ok()
                    .and_then(NonZeroU32::new)
                    .ok_or(StoreError::Backend("d1.invalid_version_number"))
            })
            .transpose()?,
        branch: row.optional_text(at + 2).map_err(err)?,
        commit_sha: row.optional_text(at + 3).map_err(err)?,
        semantic_hash: row.digest(at + 4).map_err(err)?,
        snapshot_digest: row.digest(at + 5).map_err(err)?,
        snapshot_size: row.unsigned(at + 6).map_err(err)?,
        source: row.string(at + 7).map_err(err)?,
        pins: decode(row, at + 8)?,
    })
}

const SKILL_COLUMNS: &str = "name, description, hash, manifest, provenance";
fn skill_of(row: &Row) -> Result<SkillVersion, StoreError> {
    Ok(SkillVersion {
        name: row.string(0).map_err(err)?,
        description: row.string(1).map_err(err)?,
        hash: row.digest(2).map_err(err)?,
        manifest: decode(row, 3)?,
        provenance: decode(row, 4)?,
    })
}

/// The runbook row a draft save or publication checks.
struct RunbookRow {
    origin: RunbookOrigin,
    draft_source: Vec<u8>,
    draft_revision: u64,
    slug: String,
    /// CF11: set when the draft's text is in the source blobs.
    draft_digest: Option<Vec<u8>>,
}
impl RunbookRow {
    /// Whether the draft is `source` (by digest when external).
    fn draft_is(&self, source: &str) -> bool {
        match &self.draft_digest {
            Some(digest) => digest.as_slice() == crate::sources::digest(source),
            None => self.draft_source == source.as_bytes(),
        }
    }
}

impl<D: D1> D1Catalog<D> {
    async fn runbook_row(&self, id: &RunbookId) -> Result<RunbookRow, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT origin, draft_source, draft_revision, slug, draft_digest FROM runbook \
                     WHERE org_id = ? AND project_id = ? AND runbook_id = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?;
        let row = rows.first().ok_or(StoreError::NotFound)?;
        Ok(RunbookRow {
            origin: origin_of(row.text(0).map_err(err)?)?,
            draft_source: row.blob(1).map_err(err)?.to_vec(),
            draft_revision: row.unsigned(2).map_err(err)?,
            slug: row.string(3).map_err(err)?,
            draft_digest: row.optional_blob(4).map_err(err)?,
        })
    }
    async fn runbook_exists(&self, id: &RunbookId) -> Result<(), StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT 1 FROM runbook WHERE org_id = ? AND project_id = ? AND runbook_id = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?;
        if rows.is_empty() {
            Err(StoreError::NotFound)
        } else {
            Ok(())
        }
    }
    fn insert_version(
        &self,
        id: &RunbookId,
        prepared: &PreparedPublication,
        number: Option<NonZeroU32>,
        branch: Option<String>,
        commit_sha: Option<String>,
        source: &crate::sources::StoredSource,
    ) -> Result<Statement, StoreError> {
        Ok(prepare(format!(
            "INSERT INTO runbook_version (org_id, project_id, runbook_id, {VERSION_COLUMNS}, \
             source_digest, source_size) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(self.org())
        .bind(self.project())
        .bind(id.as_str())
        .bind(prepared.version_id.as_str())
        .bind(number.map(NonZeroU32::get))
        .bind(branch)
        .bind(commit_sha)
        .bind(prepared.semantic_hash.to_vec())
        .bind(prepared.snapshot_digest.to_vec())
        .bind(uint(prepared.snapshot_size).map_err(err)?)
        .bind(source.inline.as_str())
        .bind(json(&prepared.pins).map_err(err)?)
        .bind(source.digest_value())
        .bind(source.size))
    }
    async fn version_where(
        &self,
        id: &RunbookId,
        condition: &str,
        param: Option<crate::Value>,
    ) -> Result<Option<VersionRecord>, StoreError> {
        let mut statement = prepare(format!(
            "SELECT {VERSION_COLUMNS}, source_digest FROM runbook_version WHERE org_id = ? \
             AND project_id = ? AND runbook_id = ? AND {condition}"
        ))
        .bind(self.org())
        .bind(self.project())
        .bind(id.as_str());
        if let Some(param) = param {
            statement = statement.bind(param);
        }
        let rows = self.db.all(statement).await.map_err(err)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let mut record = version_at(row, 0)?;
        record.source = self
            .stitch(
                std::mem::take(&mut record.source),
                row.optional_blob(9).map_err(err)?,
            )
            .await?;
        Ok(Some(record))
    }

    // ---- CatalogStore (the D1 half) ----

    pub async fn read_draft(&self, key: &str) -> Result<Option<DraftRevision>, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT revision, source, source_digest FROM draft WHERE org_id = ? \
                     AND project_id = ? AND key = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(key),
            )
            .await
            .map_err(err)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(DraftRevision {
            key: key.into(),
            revision: row.unsigned(0).map_err(err)?,
            source: self
                .stitch(
                    row.string(1).map_err(err)?,
                    row.optional_blob(2).map_err(err)?,
                )
                .await?,
        }))
    }

    /// Compare-and-set in one statement; the row's revision is the precondition.
    pub async fn replace_draft(
        &self,
        key: &str,
        expected: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        if key.is_empty() {
            return Err(StoreError::InvalidBatch("catalog.empty_draft_key"));
        }
        let next = expected
            .checked_add(1)
            .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?;
        let stored = self.store_source(&source).await?;
        let statement = if expected == 0 {
            prepare(
                "INSERT INTO draft (org_id, project_id, key, revision, source, source_digest, \
                 source_size) VALUES (?, ?, ?, 1, ?, ?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(self.org())
            .bind(self.project())
            .bind(key)
            .bind(stored.inline.as_str())
            .bind(stored.digest_value())
            .bind(stored.size)
        } else {
            prepare(
                "UPDATE draft SET revision = ?, source = ?, source_digest = ?, source_size = ? \
                 WHERE org_id = ? AND project_id = ? AND key = ? AND revision = ?",
            )
            .bind(uint(next).map_err(err)?)
            .bind(stored.inline.as_str())
            .bind(stored.digest_value())
            .bind(stored.size)
            .bind(self.org())
            .bind(self.project())
            .bind(key)
            .bind(uint(expected).map_err(err)?)
        };
        Ok(if self.db.run(statement).await.map_err(err)? == 1 {
            CommitResult::Committed(next)
        } else {
            CommitResult::Conflict
        })
    }

    pub async fn list_runs(&self) -> Result<Vec<RunSummary>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT r.body FROM run_summary r JOIN execution_owner o \
                     ON o.execution_id = r.execution_id \
                     WHERE o.org_id = ? AND o.project_id = ? ORDER BY r.execution_id LIMIT ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(count(LIST_LIMIT)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }

    pub async fn get_run(&self, id: &ExecutionId) -> Result<Option<RunSummary>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT r.body FROM run_summary r JOIN execution_owner o \
                     ON o.execution_id = r.execution_id \
                     WHERE r.execution_id = ? AND o.org_id = ? AND o.project_id = ?",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }

    pub async fn list_assignments(&self) -> Result<Vec<Assignment>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT a.body FROM assignment a JOIN execution_owner o \
                     ON o.execution_id = a.execution_id \
                     WHERE o.org_id = ? AND o.project_id = ? ORDER BY a.execution_id, a.ordinal \
                     LIMIT ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(count(LIST_LIMIT)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }

    pub async fn create_runbook(
        &self,
        record: RunbookRecord,
        source: String,
    ) -> Result<DraftView, StoreError> {
        if runbook_model::parse_runbook_ref(&record.slug).is_err() || record.slug.contains('@') {
            return Err(StoreError::InvalidBatch("catalog.invalid_slug"));
        }
        let stored = self.store_source(&source).await?;
        // UI-1b: the list's fields beside the draft, in the same statement.
        let listing = crate::listing::StoredListing::of(&record.slug, &source)?;
        let statement = prepare(format!(
            "INSERT INTO runbook (org_id, project_id, runbook_id, slug, origin, archived, \
             draft_source, draft_revision, draft_digest, draft_size, {}) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (runbook_id) DO NOTHING",
            crate::listing::LISTING_COLUMNS
        ))
        .bind(self.org())
        .bind(self.project())
        .bind(record.runbook_id.as_str())
        .bind(&record.slug)
        .bind(origin_label(record.origin))
        .bind(record.archived)
        .bind(stored.inline.as_bytes())
        .bind(stored.digest_value())
        .bind(stored.size);
        let inserted = self.db.run(listing.bind(statement, 1)?).await;
        match inserted {
            Ok(1) => draft_view(&record.runbook_id, source, 1),
            Ok(_) => Err(StoreError::InvalidBatch("catalog.runbook_exists")),
            Err(D1Error::Unique) => Err(StoreError::InvalidBatch("catalog.runbook_slug_conflict")),
            Err(error) => Err(err(error)),
        }
    }

    pub async fn read_runbook(&self, id: &RunbookId) -> Result<Option<RunbookRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT slug, origin, archived FROM runbook WHERE org_id = ? \
                     AND project_id = ? AND runbook_id = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok(RunbookRecord {
                    runbook_id: id.clone(),
                    slug: row.string(0).map_err(err)?,
                    origin: origin_of(row.text(1).map_err(err)?)?,
                    archived: row.flag(2).map_err(err)?,
                })
            })
            .transpose()
    }

    pub async fn get_draft(&self, id: &RunbookId) -> Result<Option<DraftView>, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT draft_source, draft_revision, draft_digest FROM runbook \
                     WHERE org_id = ? AND project_id = ? AND runbook_id = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let source = self
            .stitch(draft_source(row, 0)?, row.optional_blob(2).map_err(err)?)
            .await?;
        draft_view(id, source, row.unsigned(1).map_err(err)?).map(Some)
    }

    pub async fn save_draft(
        &self,
        id: &RunbookId,
        expected_revision: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        let runbook = self.runbook_row(id).await?;
        if runbook.origin == RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.git_managed"));
        }
        if runbook.draft_revision != expected_revision {
            return Ok(CommitResult::Conflict);
        }
        let next = expected_revision
            .checked_add(1)
            .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?;
        let stored = self.store_source(&source).await?;
        // UI-1b: the list's fields beside the draft, in the same statement.
        let listing = crate::listing::StoredListing::of(&runbook.slug, &source)?;
        let statement = prepare(format!(
            "UPDATE runbook SET draft_source = ?, draft_revision = ?, draft_digest = ?, \
             draft_size = ?, {} WHERE org_id = ? AND project_id = ? AND runbook_id = ? \
             AND draft_revision = ?",
            crate::listing::LISTING_SET
        ))
        .bind(stored.inline.as_bytes())
        .bind(uint(next).map_err(err)?)
        .bind(stored.digest_value())
        .bind(stored.size);
        let changed = self
            .db
            .run(
                listing
                    .bind(statement, next)?
                    .bind(self.org())
                    .bind(self.project())
                    .bind(id.as_str())
                    .bind(uint(expected_revision).map_err(err)?),
            )
            .await
            .map_err(err)?;
        Ok(if changed == 1 {
            CommitResult::Committed(next)
        } else {
            CommitResult::Conflict
        })
    }

    /// Rechecks the reviewed revision and source and numbers the version
    /// after the latest one read; the batch guards both, so a racing save
    /// is a draft conflict and a racing publish is retried.
    pub async fn publish_revision(
        &self,
        id: &RunbookId,
        expected_revision: u64,
        prepared: PreparedPublication,
    ) -> Result<PublishedVersionView, StoreError> {
        let mut stored = None;
        for _ in 0..PUBLISH_ATTEMPTS {
            let runbook = self.runbook_row(id).await?;
            if runbook.origin == RunbookOrigin::Git {
                return Err(StoreError::InvalidBatch("runbook.git_managed"));
            }
            if runbook.draft_revision != expected_revision || !runbook.draft_is(&prepared.source) {
                return Err(StoreError::InvalidBatch("catalog.draft_conflict"));
            }
            // The draft's own blob when it is external (content-addressed).
            if stored.is_none() {
                stored = Some(self.store_source(&prepared.source).await?);
            }
            let stored = stored.clone().ok_or(StoreError::Backend("d1.source"))?;
            let latest = self
                .db
                .all(
                    prepare(
                        "SELECT version_id, number, semantic_hash FROM runbook_version \
                         WHERE runbook_id = ? AND number IS NOT NULL ORDER BY number DESC LIMIT 1",
                    )
                    .bind(id.as_str()),
                )
                .await
                .map_err(err)?;
            let mut next = 1u32;
            let mut latest_number = 0i64;
            if let Some(row) = latest.first() {
                latest_number = row.integer(1).map_err(err)?;
                let number = u32::try_from(latest_number)
                    .ok()
                    .and_then(NonZeroU32::new)
                    .ok_or(StoreError::Backend("d1.invalid_version_number"))?;
                if row.digest(2).map_err(err)? == prepared.semantic_hash {
                    return Ok(PublishedVersionView {
                        warnings: prepared.warnings.clone(),
                        runbook_id: id.clone(),
                        runbook_version_id: row.parse(0).map_err(err)?,
                        runbook_version: number,
                    });
                }
                next = number
                    .get()
                    .checked_add(1)
                    .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
            }
            let number = NonZeroU32::new(next)
                .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
            let view = PublishedVersionView {
                warnings: prepared.warnings.clone(),
                runbook_id: id.clone(),
                runbook_version_id: prepared.version_id.clone(),
                runbook_version: number,
            };
            // C1: `runbook.published` commits with the version it reports.
            let event =
                runbook_core::webhooks::published_event(id, &runbook.slug, &view, self.db.now_ms());
            let statements = vec![
                guard(
                    "catalog.draft_conflict",
                    "EXISTS (SELECT 1 FROM runbook WHERE runbook_id = ? AND draft_revision = ? \
                     AND (draft_digest = ? OR (draft_digest IS NULL AND draft_source = ?)))",
                )
                .bind(id.as_str())
                .bind(uint(expected_revision).map_err(err)?)
                .bind(crate::sources::digest(&prepared.source).to_vec())
                .bind(prepared.source.as_bytes()),
                guard(
                    "catalog.publish_race",
                    "coalesce((SELECT max(number) FROM runbook_version WHERE runbook_id = ?), 0) = ?",
                )
                .bind(id.as_str())
                .bind(latest_number),
                self.insert_version(id, &prepared, Some(number), None, None, &stored)?,
                project_event(&self.scope, &event)?,
            ];
            match self.db.batch(statements).await {
                Ok(_) => return Ok(view),
                Err(D1Error::Guard(code)) if code == "catalog.publish_race" => {}
                Err(D1Error::Unique) => {}
                Err(error) => return Err(err(error)),
            }
        }
        Err(StoreError::Backend("d1.retry"))
    }

    /// Git sync: see `CatalogStore::publish_git`. One batch: the
    /// runbook row (origin, archived, draft and its listing) and the new
    /// version with its event, guarded against a racing publish.
    pub async fn publish_git(
        &self,
        id: &RunbookId,
        prepared: PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<PublishedVersionView, StoreError> {
        let stored = self.store_source(&prepared.source).await?;
        for _ in 0..PUBLISH_ATTEMPTS {
            let runbook = self.runbook_row(id).await?;
            let revision = if runbook.draft_is(&prepared.source) {
                runbook.draft_revision
            } else {
                runbook
                    .draft_revision
                    .checked_add(1)
                    .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?
            };
            let listing = crate::listing::StoredListing::of(&runbook.slug, &prepared.source)?;
            let update = listing
                .bind(
                    prepare(format!(
                        "UPDATE runbook SET origin = 'git', archived = 0, draft_source = ?, \
                         draft_revision = ?, draft_digest = ?, draft_size = ?, {} \
                         WHERE org_id = ? AND project_id = ? AND runbook_id = ?",
                        crate::listing::LISTING_SET
                    ))
                    .bind(stored.inline.as_bytes())
                    .bind(uint(revision).map_err(err)?)
                    .bind(stored.digest_value())
                    .bind(stored.size),
                    revision,
                )?
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str());
            let latest = self
                .db
                .all(
                    prepare(
                        "SELECT version_id, number, semantic_hash FROM runbook_version \
                         WHERE runbook_id = ? AND number IS NOT NULL ORDER BY number DESC LIMIT 1",
                    )
                    .bind(id.as_str()),
                )
                .await
                .map_err(err)?;
            let mut next = 1u32;
            let mut latest_number = 0i64;
            if let Some(row) = latest.first() {
                latest_number = row.integer(1).map_err(err)?;
                let number = u32::try_from(latest_number)
                    .ok()
                    .and_then(NonZeroU32::new)
                    .ok_or(StoreError::Backend("d1.invalid_version_number"))?;
                if row.digest(2).map_err(err)? == prepared.semantic_hash {
                    self.db.run(update).await.map_err(err)?;
                    return Ok(PublishedVersionView {
                        warnings: prepared.warnings.clone(),
                        runbook_id: id.clone(),
                        runbook_version_id: row.parse(0).map_err(err)?,
                        runbook_version: number,
                    });
                }
                next = number
                    .get()
                    .checked_add(1)
                    .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
            }
            let number = NonZeroU32::new(next)
                .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
            let view = PublishedVersionView {
                warnings: prepared.warnings.clone(),
                runbook_id: id.clone(),
                runbook_version_id: prepared.version_id.clone(),
                runbook_version: number,
            };
            let event =
                runbook_core::webhooks::published_event(id, &runbook.slug, &view, self.db.now_ms());
            let statements = vec![
                guard(
                    "catalog.publish_race",
                    "coalesce((SELECT max(number) FROM runbook_version WHERE runbook_id = ?), 0) = ?",
                )
                .bind(id.as_str())
                .bind(latest_number),
                update,
                self.insert_version(
                    id,
                    &prepared,
                    Some(number),
                    branch.clone(),
                    commit_sha.clone(),
                    &stored,
                )?,
                project_event(&self.scope, &event)?,
            ];
            match self.db.batch(statements).await {
                Ok(_) => return Ok(view),
                Err(D1Error::Guard(code)) if code == "catalog.publish_race" => {}
                Err(D1Error::Unique) => {}
                Err(error) => return Err(err(error)),
            }
        }
        Err(StoreError::Backend("d1.retry"))
    }

    /// Git sync: make a Git-managed runbook editable in the app again.
    pub async fn detach_git(&self, id: &RunbookId) -> Result<(), StoreError> {
        let runbook = self.runbook_row(id).await?;
        if runbook.origin != RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.not_git_managed"));
        }
        self.db
            .run(
                prepare(
                    "UPDATE runbook SET origin = 'ui' \
                     WHERE org_id = ? AND project_id = ? AND runbook_id = ? AND origin = 'git'",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?;
        Ok(())
    }

    /// Git sync: archive a Git-managed runbook whose file is gone.
    pub async fn archive_git(&self, id: &RunbookId) -> Result<(), StoreError> {
        let runbook = self.runbook_row(id).await?;
        if runbook.origin != RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.not_git_managed"));
        }
        self.db
            .run(
                prepare(
                    "UPDATE runbook SET archived = 1 \
                     WHERE org_id = ? AND project_id = ? AND runbook_id = ? AND origin = 'git'",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?;
        Ok(())
    }

    pub async fn create_preview(
        &self,
        id: &RunbookId,
        prepared: PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<VersionView, StoreError> {
        self.runbook_exists(id).await?;
        let existing = self
            .version_where(
                id,
                "version_id = ?",
                Some(prepared.version_id.as_str().into()),
            )
            .await?;
        if let Some(record) = existing {
            return Ok(record.view(id));
        }
        let stored = self.store_source(&prepared.source).await?;
        let insert = self.insert_version(
            id,
            &prepared,
            None,
            branch.clone(),
            commit_sha.clone(),
            &stored,
        )?;
        match self.db.run(insert).await {
            Ok(_) => Ok(VersionView {
                runbook_id: id.clone(),
                runbook_version_id: prepared.version_id.clone(),
                runbook_version: None,
                branch,
                commit_sha,
            }),
            // A racing preview of the same version won: answer with it.
            Err(D1Error::Unique) => self
                .version_where(
                    id,
                    "version_id = ?",
                    Some(prepared.version_id.as_str().into()),
                )
                .await?
                .map(|record| record.view(id))
                .ok_or(StoreError::Backend("d1.unique_violation")),
            Err(error) => Err(err(error)),
        }
    }

    pub async fn resolve_version(
        &self,
        id: &RunbookId,
        selector: Option<VersionSelector>,
    ) -> Result<VersionView, StoreError> {
        self.runbook_exists(id).await?;
        let record = match &selector {
            None => {
                self.version_where(id, "number IS NOT NULL ORDER BY number DESC LIMIT 1", None)
                    .await?
            }
            Some(VersionSelector::Number(number)) => {
                self.version_where(id, "number = ?", Some(number.get().into()))
                    .await?
            }
            Some(VersionSelector::Id(version_id)) => {
                self.version_where(id, "version_id = ?", Some(version_id.as_str().into()))
                    .await?
            }
        };
        Ok(record.ok_or(StoreError::NotFound)?.view(id))
    }

    pub async fn version_record(
        &self,
        runbook_id: &RunbookId,
        version_id: &VersionId,
    ) -> Result<Option<VersionRecord>, StoreError> {
        self.version_where(
            runbook_id,
            "version_id = ?",
            Some(version_id.as_str().into()),
        )
        .await
    }

    pub async fn list_versions(
        &self,
        id: &RunbookId,
    ) -> Result<Vec<PublishedVersionView>, StoreError> {
        let rows = self
            .db
            .batch(vec![
                prepare(
                    "SELECT 1 AS present FROM runbook WHERE org_id = ? AND project_id = ? \
                     AND runbook_id = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
                prepare(format!(
                    "SELECT {VERSION_COLUMNS} FROM runbook_version WHERE runbook_id = ? \
                     AND number IS NOT NULL ORDER BY number"
                ))
                .bind(id.as_str()),
            ])
            .await
            .map_err(err)?;
        if rows[0].rows.is_empty() {
            return Err(StoreError::NotFound);
        }
        rows[1]
            .rows
            .iter()
            .map(|row| {
                let record = version_at(row, 0)?;
                Ok(PublishedVersionView {
                    warnings: Vec::new(),
                    runbook_id: id.clone(),
                    runbook_version_id: record.version_id,
                    runbook_version: record
                        .number
                        .ok_or(StoreError::Backend("d1.invalid_version_number"))?,
                })
            })
            .collect()
    }

    pub async fn list_previews(&self, id: &RunbookId) -> Result<Vec<VersionView>, StoreError> {
        let rows = self
            .db
            .batch(vec![
                prepare(
                    "SELECT 1 AS present FROM runbook WHERE org_id = ? AND project_id = ? \
                     AND runbook_id = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(id.as_str()),
                prepare(format!(
                    "SELECT {VERSION_COLUMNS} FROM runbook_version WHERE runbook_id = ? \
                     AND number IS NULL ORDER BY version_id"
                ))
                .bind(id.as_str()),
            ])
            .await
            .map_err(err)?;
        if rows[0].rows.is_empty() {
            return Err(StoreError::NotFound);
        }
        rows[1]
            .rows
            .iter()
            .map(|row| Ok(version_at(row, 0)?.view(id)))
            .collect()
    }

    /// Insert, or keep the existing record for the same name and hash. A new
    /// hash becomes the name's latest (`seq` grows per tenant).
    pub async fn record_skill_version(
        &self,
        version: SkillVersion,
    ) -> Result<SkillVersion, StoreError> {
        let rows = self
            .db
            .batch(vec![
                prepare(format!(
                    "INSERT INTO skill_version (org_id, project_id, {SKILL_COLUMNS}, seq) \
                     SELECT ?, ?, ?, ?, ?, ?, ?, coalesce(max(seq), 0) + 1 FROM skill_version \
                     WHERE org_id = ? AND project_id = ? \
                     ON CONFLICT (org_id, project_id, name, hash) DO NOTHING"
                ))
                .bind(self.org())
                .bind(self.project())
                .bind(&version.name)
                .bind(&version.description)
                .bind(version.hash.to_vec())
                .bind(json(&version.manifest).map_err(err)?)
                .bind(json(&version.provenance).map_err(err)?)
                .bind(self.org())
                .bind(self.project()),
                prepare(format!(
                    "SELECT {SKILL_COLUMNS} FROM skill_version WHERE org_id = ? AND project_id = ? \
                     AND name = ? AND hash = ?"
                ))
                .bind(self.org())
                .bind(self.project())
                .bind(&version.name)
                .bind(version.hash.to_vec()),
            ])
            .await
            .map_err(err)?;
        skill_of(rows[1].rows.first().ok_or(StoreError::NotFound)?)
    }

    pub async fn skill_version(&self, hash: &[u8; 32]) -> Result<Option<SkillVersion>, StoreError> {
        self.db
            .all(
                prepare(format!(
                    "SELECT {SKILL_COLUMNS} FROM skill_version WHERE org_id = ? AND project_id = ? \
                     AND hash = ? ORDER BY seq LIMIT 1"
                ))
                .bind(self.org())
                .bind(self.project())
                .bind(hash.to_vec()),
            )
            .await
            .map_err(err)?
            .first()
            .map(skill_of)
            .transpose()
    }

    pub async fn latest_skills(&self) -> Result<Vec<SkillVersion>, StoreError> {
        self.db
            .all(
                prepare(format!(
                    "SELECT {SKILL_COLUMNS} FROM skill_version s WHERE org_id = ? \
                     AND project_id = ? AND seq = (SELECT max(t.seq) FROM skill_version t \
                     WHERE t.org_id = s.org_id AND t.project_id = s.project_id \
                     AND t.name = s.name) ORDER BY name"
                ))
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(skill_of)
            .collect()
    }

    pub async fn versions_pinning_skill(
        &self,
        hash: &[u8; 32],
    ) -> Result<Vec<(RunbookId, VersionRecord)>, StoreError> {
        let mut pinned = vec![];
        for row in self
            .db
            .all(
                prepare(format!(
                    "SELECT runbook_id, {VERSION_COLUMNS} FROM runbook_version \
                     WHERE org_id = ? AND project_id = ? ORDER BY runbook_id, version_id"
                ))
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?
        {
            let record = version_at(&row, 1)?;
            if record.pins.skills.values().any(|p| p == hash) {
                pinned.push((row.parse(0).map_err(err)?, record));
            }
        }
        Ok(pinned)
    }

    // ---- routing reads (org-scoped, never grant access) ----

    async fn project_of(
        &self,
        sql: &'static str,
        org_id: &OrganizationId,
        id: &str,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.db
            .all(prepare(sql).bind(org_id.as_str()).bind(id))
            .await
            .map_err(err)?
            .first()
            .map(|row| row.parse(0).map_err(err))
            .transpose()
    }
    pub async fn execution_project(
        &self,
        org_id: &OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.project_of(
            "SELECT project_id FROM execution_owner WHERE org_id = ? AND execution_id = ?",
            org_id,
            id.as_str(),
        )
        .await
    }
    pub async fn tombstone_project(
        &self,
        org_id: &OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.project_of(
            "SELECT project_id FROM tombstone WHERE org_id = ? AND execution_id = ?",
            org_id,
            id.as_str(),
        )
        .await
    }
    pub async fn runbook_project(
        &self,
        org_id: &OrganizationId,
        id: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.project_of(
            "SELECT project_id FROM runbook WHERE org_id = ? AND runbook_id = ?",
            org_id,
            id.as_str(),
        )
        .await
    }
    /// B6b: which execution (and project) a step run belongs to, in `org`.
    pub async fn step_run_execution(
        &self,
        org_id: &OrganizationId,
        step_run: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT execution_id, project_id FROM step_run_index WHERE org_id = ? \
                     AND step_run_id = ?",
                )
                .bind(org_id.as_str())
                .bind(step_run.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| Ok((row.parse(0).map_err(err)?, row.parse(1).map_err(err)?)))
            .transpose()
    }
}

impl<D: D1> D1Catalog<D> {
    /// Record this tenant as `id`'s owner, as a start's batch does (a
    /// conformance `create`, which makes a run without starting it).
    /// Another tenant's run with this ID is `catalog.execution_scope_conflict`.
    pub async fn claim_owner(&self, id: &ExecutionId) -> Result<(), StoreError> {
        self.db
            .batch(crate::admission::claim_owner(&self.scope, id))
            .await
            .map(|_| ())
            .map_err(err)
    }
}

impl<D: D1> D1Catalog<D> {
    /// CF6: the organization's admitted, unfinished runs (from the runs
    /// list, so within the derived-row bound).
    pub async fn running_in_org(&self, org: &OrganizationId) -> Result<u64, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT count(*) FROM run_summary s JOIN execution_owner o \
                     ON o.execution_id = s.execution_id WHERE o.org_id = ? \
                     AND s.status NOT IN ('queued', 'succeeded', 'failed', 'cancelled')",
                )
                .bind(org.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| row.unsigned(0).map_err(err))
            .transpose()
            .map(|n| n.unwrap_or(0))
    }
}

/// The owner of an execution, from any organization (a global key read
/// for routing; grants nothing).
pub async fn execution_owner<D: D1>(
    db: &D,
    id: &ExecutionId,
) -> Result<Option<TenantScope>, StoreError> {
    db.all(
        prepare("SELECT org_id, project_id FROM execution_owner WHERE execution_id = ?")
            .bind(id.as_str()),
    )
    .await
    .map_err(err)?
    .first()
    .map(|row| {
        Ok(TenantScope {
            org_id: row.parse(0).map_err(err)?,
            project_id: row.parse(1).map_err(err)?,
        })
    })
    .transpose()
}

/// The most rows `list_runs` and `list_assignments` return (review 7: a
/// Worker reads a whole result into memory). The routes page through
/// search and `/v1/me/tasks`; these unpaged lists are for small tenants
/// and tests.
pub const LIST_LIMIT: usize = 10_000;

/// Publication attempts before a racing publisher wins for good.
const PUBLISH_ATTEMPTS: usize = 5;

pub(crate) fn project_event(
    scope: &TenantScope,
    event: &ProjectEvent,
) -> Result<Statement, StoreError> {
    Ok(prepare(
        "INSERT INTO project_event_outbox (org_id, project_id, event_id, created_at_ms, body) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(scope.org_id.as_str())
    .bind(scope.project_id.as_str())
    .bind(&event.event_id)
    .bind(event.at_ms)
    .bind(json(event).map_err(err)?))
}

impl<D: D1> ProjectOutbox for D1Catalog<D> {
    async fn pending_project_events(&self, limit: usize) -> Result<Vec<ProjectEvent>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM project_event_outbox WHERE org_id = ? AND project_id = ? \
                     ORDER BY created_at_ms, event_id LIMIT ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }
    async fn ack_project_event(&self, event_id: &str) -> Result<(), StoreError> {
        self.db
            .run(
                prepare(
                    "DELETE FROM project_event_outbox WHERE org_id = ? AND project_id = ? \
                     AND event_id = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(event_id),
            )
            .await
            .map(|_| ())
            .map_err(err)
    }
}
