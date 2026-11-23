//! B3.1 runbook identity, draft and publication rows. Publishing locks the
//! runbook row, rechecks the reviewed revision and source, and numbers
//! versions serially; a semantic duplicate of the latest version returns it.
use crate::{
    PostgresExecutionStore,
    execution::READ_COMMITTED,
    rows::{db, decode, encode, int, is_unique_violation, uint},
};
use runbook_core::{
    StoreError,
    catalog::{RunbookOrigin, RunbookRecord, TenantScope, VersionRecord},
    publish::PreparedPublication,
};
use runbook_model::{
    DraftView, PublishedVersionView, RunbookId, VersionId, VersionSelector, VersionView,
};
use std::num::NonZeroU32;
use tokio_postgres::Client;

pub(crate) fn origin_label(origin: RunbookOrigin) -> &'static str {
    match origin {
        RunbookOrigin::Ui => "ui",
        RunbookOrigin::Git => "git",
    }
}
pub(crate) fn origin_of(text: &str) -> Result<RunbookOrigin, StoreError> {
    match text {
        "ui" => Ok(RunbookOrigin::Ui),
        "git" => Ok(RunbookOrigin::Git),
        _ => Err(StoreError::Backend("postgres.invalid_runbook_origin")),
    }
}
fn parse_id<T: std::str::FromStr>(text: &str) -> Result<T, StoreError> {
    text.parse()
        .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))
}
fn hash_of(bytes: Vec<u8>) -> Result<[u8; 32], StoreError> {
    bytes
        .try_into()
        .map_err(|_| StoreError::Backend("postgres.invalid_hash"))
}
pub(crate) fn draft_view(
    id: &RunbookId,
    source: String,
    revision: i64,
) -> Result<DraftView, StoreError> {
    Ok(DraftView {
        runbook_id: id.clone(),
        source,
        revision: uint(revision)?
            .try_into()
            .map_err(|_| StoreError::Backend("postgres.invalid_revision"))?,
    })
}

pub(crate) struct LockedRunbook {
    pub(crate) origin: RunbookOrigin,
    pub(crate) draft_source: String,
    pub(crate) draft_revision: u64,
    pub(crate) slug: String,
}
pub(crate) async fn lock_runbook(
    c: &Client,
    scope: &TenantScope,
    id: &RunbookId,
) -> Result<LockedRunbook, StoreError> {
    let row = c
        .query_opt(
            "SELECT origin, draft_source, draft_revision, slug FROM runbook.runbook \
             WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 FOR UPDATE",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &id.as_str(),
            ],
        )
        .await
        .map_err(db)?
        .ok_or(StoreError::NotFound)?;
    Ok(LockedRunbook {
        origin: origin_of(row.get(0))?,
        draft_source: decode_draft_source(row.get(1))?,
        draft_revision: uint(row.get(2))?,
        slug: row.get(3),
    })
}

pub(crate) const VERSION_COLUMNS: &str = "version_id, number, branch, commit_sha, semantic_hash, \
     snapshot_digest, snapshot_size, source, pins";
pub(crate) fn version_record(row: &tokio_postgres::Row) -> Result<VersionRecord, StoreError> {
    Ok(VersionRecord {
        version_id: parse_id(row.get(0))?,
        number: row
            .get::<_, Option<i32>>(1)
            .map(|n| {
                NonZeroU32::new(u32::try_from(n).unwrap_or_default())
                    .ok_or(StoreError::Backend("postgres.invalid_version_number"))
            })
            .transpose()?,
        branch: row.get(2),
        commit_sha: row.get(3),
        semantic_hash: hash_of(row.get(4))?,
        snapshot_digest: hash_of(row.get(5))?,
        snapshot_size: uint(row.get(6))?,
        source: row.get(7),
        pins: decode(row.get(8))?,
    })
}

async fn insert_version(
    c: &Client,
    scope: &TenantScope,
    id: &RunbookId,
    prepared: &PreparedPublication,
    number: Option<NonZeroU32>,
    branch: &Option<String>,
    commit_sha: &Option<String>,
) -> Result<(), StoreError> {
    c.execute(
        &format!(
            "INSERT INTO runbook.runbook_version \
             (org_id, project_id, runbook_id, {VERSION_COLUMNS}) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"
        ),
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &id.as_str(),
            &prepared.version_id.as_str(),
            &number.map(|n| n.get() as i32),
            branch,
            commit_sha,
            &prepared.semantic_hash.as_slice(),
            &prepared.snapshot_digest.as_slice(),
            &int(prepared.snapshot_size)?,
            &prepared.source,
            &encode(&prepared.pins)?,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

impl PostgresExecutionStore {
    pub(crate) async fn create_runbook_row(
        &self,
        record: RunbookRecord,
        source: String,
    ) -> Result<DraftView, StoreError> {
        if runbook_model::parse_runbook_ref(&record.slug).is_err() || record.slug.contains('@') {
            return Err(StoreError::InvalidBatch("catalog.invalid_slug"));
        }
        self.transaction(READ_COMMITTED, async |c| {
            self.create_runbook_in(c, record, source).await
        })
        .await
    }

    pub(crate) async fn publish_row(
        &self,
        id: &RunbookId,
        expected_revision: u64,
        prepared: PreparedPublication,
    ) -> Result<PublishedVersionView, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            self.publish_in(c, id, expected_revision, prepared).await
        })
        .await
    }

    pub(crate) async fn preview_row(
        &self,
        id: &RunbookId,
        prepared: PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<VersionView, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            lock_runbook(c, &self.scope, id).await?;
            if let Some(row) = c
                .query_opt(
                    &format!(
                        "SELECT {VERSION_COLUMNS} FROM runbook.runbook_version \
                         WHERE runbook_id = $1 AND version_id = $2"
                    ),
                    &[&id.as_str(), &prepared.version_id.as_str()],
                )
                .await
                .map_err(db)?
            {
                return Ok(version_record(&row)?.view(id));
            }
            insert_version(c, &self.scope, id, &prepared, None, &branch, &commit_sha).await?;
            Ok(VersionView {
                runbook_id: id.clone(),
                runbook_version_id: prepared.version_id.clone(),
                runbook_version: None,
                branch,
                commit_sha,
            })
        })
        .await
    }

    pub(crate) async fn resolve_row(
        &self,
        id: &RunbookId,
        selector: Option<VersionSelector>,
    ) -> Result<VersionView, StoreError> {
        self.transaction(crate::execution::SNAPSHOT_READ, async |c| {
            runbook_exists(c, &self.scope, id).await?;
            let row = match &selector {
                None => {
                    c.query_opt(
                        &format!(
                            "SELECT {VERSION_COLUMNS} FROM runbook.runbook_version \
                             WHERE runbook_id = $1 AND number IS NOT NULL \
                             ORDER BY number DESC LIMIT 1"
                        ),
                        &[&id.as_str()],
                    )
                    .await
                }
                Some(VersionSelector::Number(number)) => {
                    c.query_opt(
                        &format!(
                            "SELECT {VERSION_COLUMNS} FROM runbook.runbook_version \
                             WHERE runbook_id = $1 AND number = $2"
                        ),
                        &[&id.as_str(), &(number.get() as i32)],
                    )
                    .await
                }
                Some(VersionSelector::Id(version_id)) => {
                    c.query_opt(
                        &format!(
                            "SELECT {VERSION_COLUMNS} FROM runbook.runbook_version \
                             WHERE runbook_id = $1 AND version_id = $2"
                        ),
                        &[&id.as_str(), &version_id.as_str()],
                    )
                    .await
                }
            }
            .map_err(db)?
            .ok_or(StoreError::NotFound)?;
            Ok(version_record(&row)?.view(id))
        })
        .await
    }
}

/// Existence inside the caller's read transaction (no row lock taken).
pub(crate) async fn runbook_exists(
    c: &Client,
    scope: &TenantScope,
    id: &RunbookId,
) -> Result<(), StoreError> {
    let exists: bool = c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM runbook.runbook \
             WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3)",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &id.as_str(),
            ],
        )
        .await
        .map_err(db)?
        .get(0);
    if exists {
        Ok(())
    } else {
        Err(StoreError::NotFound)
    }
}

impl PostgresExecutionStore {
    pub(crate) async fn version_row(
        &self,
        runbook_id: &RunbookId,
        version_id: &VersionId,
    ) -> Result<Option<VersionRecord>, StoreError> {
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        conn.client()
            .query_opt(
                &format!(
                    "SELECT {VERSION_COLUMNS} FROM runbook.runbook_version \
                     WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND version_id = $4"
                ),
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &runbook_id.as_str(),
                    &version_id.as_str(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| version_record(&row))
            .transpose()
    }
}

impl PostgresExecutionStore {
    pub(crate) async fn create_runbook_in(
        &self,
        c: &Client,
        record: RunbookRecord,
        source: String,
    ) -> Result<DraftView, StoreError> {
        let inserted = c
                .execute(
                    "INSERT INTO runbook.runbook \
                     (org_id, project_id, runbook_id, slug, origin, archived, draft_source, draft_revision) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 1) ON CONFLICT (runbook_id) DO NOTHING",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &record.runbook_id.as_str(),
                        &record.slug,
                        &origin_label(record.origin),
                        &record.archived,
                        &source.as_bytes(),
                    ],
                )
                .await;
        match inserted {
            Ok(1) => draft_view(&record.runbook_id, source, 1),
            Ok(_) => Err(StoreError::InvalidBatch("catalog.runbook_exists")),
            Err(error) if is_unique_violation(&error) => {
                Err(StoreError::InvalidBatch("catalog.runbook_slug_conflict"))
            }
            Err(error) => Err(db(error)),
        }
    }
}

impl PostgresExecutionStore {
    pub(crate) async fn publish_in(
        &self,
        c: &Client,
        id: &RunbookId,
        expected_revision: u64,
        prepared: PreparedPublication,
    ) -> Result<PublishedVersionView, StoreError> {
        let runbook = lock_runbook(c, &self.scope, id).await?;
        if runbook.origin == RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.git_managed"));
        }
        if runbook.draft_revision != expected_revision || runbook.draft_source != prepared.source {
            return Err(StoreError::InvalidBatch("catalog.draft_conflict"));
        }
        let latest = c
            .query_opt(
                "SELECT version_id, number, semantic_hash FROM runbook.runbook_version \
                     WHERE runbook_id = $1 AND number IS NOT NULL ORDER BY number DESC LIMIT 1",
                &[&id.as_str()],
            )
            .await
            .map_err(db)?;
        let mut next = 1u32;
        if let Some(row) = &latest {
            if hash_of(row.get(2))? == prepared.semantic_hash {
                return Ok(PublishedVersionView {
                    warnings: prepared.warnings.clone(),
                    runbook_id: id.clone(),
                    runbook_version_id: parse_id(row.get(0))?,
                    runbook_version: NonZeroU32::new(
                        u32::try_from(row.get::<_, i32>(1)).unwrap_or_default(),
                    )
                    .ok_or(StoreError::Backend("postgres.invalid_version_number"))?,
                });
            }
            next = u32::try_from(row.get::<_, i32>(1))
                .ok()
                .and_then(|n| n.checked_add(1))
                .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
        }
        let number =
            NonZeroU32::new(next).ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
        insert_version(c, &self.scope, id, &prepared, Some(number), &None, &None).await?;
        let view = PublishedVersionView {
            warnings: prepared.warnings.clone(),
            runbook_id: id.clone(),
            runbook_version_id: prepared.version_id.clone(),
            runbook_version: number,
        };
        // C1: `runbook.published` commits with the version it reports.
        let now: i64 = c
            .query_one(
                "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                &[],
            )
            .await
            .map_err(db)?
            .get(0);
        crate::project_outbox::insert(
            c,
            &self.scope,
            &runbook_core::webhooks::published_event(id, &runbook.slug, &view, now),
        )
        .await?;
        Ok(view)
    }
}

impl PostgresExecutionStore {
    pub(crate) async fn publish_git_row(
        &self,
        id: &RunbookId,
        prepared: PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<PublishedVersionView, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            self.publish_git_in(c, id, prepared.clone(), branch.clone(), commit_sha.clone())
                .await
        })
        .await
    }

    /// Git sync: see [`runbook_core::catalog::CatalogStore::publish_git`].
    pub(crate) async fn publish_git_in(
        &self,
        c: &Client,
        id: &RunbookId,
        prepared: PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<PublishedVersionView, StoreError> {
        let runbook = lock_runbook(c, &self.scope, id).await?;
        let revision = if runbook.draft_source == prepared.source {
            runbook.draft_revision
        } else {
            runbook
                .draft_revision
                .checked_add(1)
                .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?
        };
        c.execute(
            "UPDATE runbook.runbook SET origin = 'git', archived = false, draft_source = $4, \
             draft_revision = $5 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3",
            &[
                &self.scope.org_id.as_str(),
                &self.scope.project_id.as_str(),
                &id.as_str(),
                &prepared.source.as_bytes(),
                &int(revision)?,
            ],
        )
        .await
        .map_err(db)?;
        let latest = c
            .query_opt(
                "SELECT version_id, number, semantic_hash FROM runbook.runbook_version \
                     WHERE runbook_id = $1 AND number IS NOT NULL ORDER BY number DESC LIMIT 1",
                &[&id.as_str()],
            )
            .await
            .map_err(db)?;
        let mut next = 1u32;
        if let Some(row) = &latest {
            if hash_of(row.get(2))? == prepared.semantic_hash {
                return Ok(PublishedVersionView {
                    warnings: prepared.warnings.clone(),
                    runbook_id: id.clone(),
                    runbook_version_id: parse_id(row.get(0))?,
                    runbook_version: NonZeroU32::new(
                        u32::try_from(row.get::<_, i32>(1)).unwrap_or_default(),
                    )
                    .ok_or(StoreError::Backend("postgres.invalid_version_number"))?,
                });
            }
            next = u32::try_from(row.get::<_, i32>(1))
                .ok()
                .and_then(|n| n.checked_add(1))
                .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
        }
        let number =
            NonZeroU32::new(next).ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
        insert_version(
            c,
            &self.scope,
            id,
            &prepared,
            Some(number),
            &branch,
            &commit_sha,
        )
        .await?;
        let view = PublishedVersionView {
            warnings: prepared.warnings.clone(),
            runbook_id: id.clone(),
            runbook_version_id: prepared.version_id.clone(),
            runbook_version: number,
        };
        let now: i64 = c
            .query_one(
                "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                &[],
            )
            .await
            .map_err(db)?
            .get(0);
        crate::project_outbox::insert(
            c,
            &self.scope,
            &runbook_core::webhooks::published_event(id, &runbook.slug, &view, now),
        )
        .await?;
        Ok(view)
    }

    pub(crate) async fn detach_git_row(&self, id: &RunbookId) -> Result<(), StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            let runbook = lock_runbook(c, &self.scope, id).await?;
            if runbook.origin != RunbookOrigin::Git {
                return Err(StoreError::InvalidBatch("runbook.not_git_managed"));
            }
            c.execute(
                "UPDATE runbook.runbook SET origin = 'ui' \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &id.as_str(),
                ],
            )
            .await
            .map_err(db)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn archive_git_row(&self, id: &RunbookId) -> Result<(), StoreError> {
        self.transaction(READ_COMMITTED, async |c| self.archive_git_in(c, id).await)
            .await
    }

    pub(crate) async fn archive_git_in(
        &self,
        c: &Client,
        id: &RunbookId,
    ) -> Result<(), StoreError> {
        let runbook = lock_runbook(c, &self.scope, id).await?;
        if runbook.origin != RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.not_git_managed"));
        }
        c.execute(
            "UPDATE runbook.runbook SET archived = true \
             WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3",
            &[
                &self.scope.org_id.as_str(),
                &self.scope.project_id.as_str(),
                &id.as_str(),
            ],
        )
        .await
        .map_err(db)?;
        Ok(())
    }
}

/// Postgres TEXT cannot represent U+0000; drafts store their exact UTF-8 bytes.
pub(crate) fn decode_draft_source(bytes: Vec<u8>) -> Result<String, StoreError> {
    String::from_utf8(bytes).map_err(|_| StoreError::Backend("catalog.invalid_draft_utf8"))
}
