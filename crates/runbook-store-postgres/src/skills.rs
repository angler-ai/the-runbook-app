//! B3.2b skill version rows. File bytes live in blob chunks under per-file
//! digests; these rows record identity, manifest and provenance per project.
use crate::{
    PostgresExecutionStore,
    execution::{READ_COMMITTED, SNAPSHOT_READ},
    rows::{db, decode, encode},
};
use runbook_core::{
    StoreError,
    catalog::{SkillVersion, VersionRecord},
};
use runbook_model::RunbookId;

pub(crate) const COLUMNS: &str = "name, description, hash, manifest, provenance";
pub(crate) fn skill_of(row: &tokio_postgres::Row) -> Result<SkillVersion, StoreError> {
    Ok(SkillVersion {
        name: row.get(0),
        description: row.get(1),
        hash: row
            .get::<_, Vec<u8>>(2)
            .try_into()
            .map_err(|_| StoreError::Backend("postgres.invalid_hash"))?,
        manifest: decode(row.get(3))?,
        provenance: decode(row.get(4))?,
    })
}

impl PostgresExecutionStore {
    pub(crate) async fn record_skill_row(
        &self,
        version: SkillVersion,
    ) -> Result<SkillVersion, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            self.record_skill_in(c, version).await
        })
        .await
    }

    /// One hash can be recorded under several names (an imported archive);
    /// the first recorded wins, as in the memory store (F39).
    pub(crate) async fn skill_row(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<SkillVersion>, StoreError> {
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        conn.client()
            .query_opt(
                &format!(
                    "SELECT {COLUMNS} FROM runbook.skill_version \
                     WHERE org_id = $1 AND project_id = $2 AND hash = $3 \
                     ORDER BY seq LIMIT 1"
                ),
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &hash.as_slice(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| skill_of(&row))
            .transpose()
    }

    pub(crate) async fn latest_skill_rows(&self) -> Result<Vec<SkillVersion>, StoreError> {
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        let mut latest = Vec::new();
        for row in conn
            .client()
            .query(
                &format!(
                    "SELECT DISTINCT ON (name) {COLUMNS} FROM runbook.skill_version \
                     WHERE org_id = $1 AND project_id = $2 ORDER BY name, seq DESC"
                ),
                &[&self.scope.org_id.as_str(), &self.scope.project_id.as_str()],
            )
            .await
            .map_err(db)?
        {
            latest.push(skill_of(&row)?);
        }
        Ok(latest)
    }

    pub(crate) async fn pinning_rows(
        &self,
        hash: &[u8; 32],
    ) -> Result<Vec<(RunbookId, VersionRecord)>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            let mut pinned = Vec::new();
            for row in c
                .query(
                    &format!(
                        "SELECT runbook_id, {} FROM runbook.runbook_version \
                         WHERE org_id = $1 AND project_id = $2 ORDER BY runbook_id, version_id",
                        crate::publish::VERSION_COLUMNS
                    ),
                    &[&self.scope.org_id.as_str(), &self.scope.project_id.as_str()],
                )
                .await
                .map_err(db)?
            {
                let runbook_id: RunbookId = row
                    .get::<_, &str>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?;
                let record = version_record_at(&row)?;
                if record.pins.skills.values().any(|pinned| pinned == hash) {
                    pinned.push((runbook_id, record));
                }
            }
            Ok(pinned)
        })
        .await
    }
}

/// VersionRecord from a row whose first column is runbook_id, followed by
/// the shared version columns at offset 1.
fn version_record_at(row: &tokio_postgres::Row) -> Result<VersionRecord, StoreError> {
    Ok(VersionRecord {
        version_id: row
            .get::<_, &str>(1)
            .parse()
            .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
        number: row
            .get::<_, Option<i32>>(2)
            .map(|n| {
                std::num::NonZeroU32::new(u32::try_from(n).unwrap_or_default())
                    .ok_or(StoreError::Backend("postgres.invalid_version_number"))
            })
            .transpose()?,
        branch: row.get(3),
        commit_sha: row.get(4),
        semantic_hash: hash_at(row, 5)?,
        snapshot_digest: hash_at(row, 6)?,
        snapshot_size: crate::rows::uint(row.get(7))?,
        source: row.get(8),
        pins: decode(row.get(9))?,
    })
}
fn hash_at(row: &tokio_postgres::Row, index: usize) -> Result<[u8; 32], StoreError> {
    row.get::<_, Vec<u8>>(index)
        .try_into()
        .map_err(|_| StoreError::Backend("postgres.invalid_hash"))
}

impl PostgresExecutionStore {
    /// Whether `version` is its name's latest (the `latest_skill_rows` order).
    pub(crate) async fn is_latest_skill_in(
        &self,
        c: &tokio_postgres::Client,
        version: &SkillVersion,
    ) -> Result<bool, StoreError> {
        let latest: Vec<u8> = c
            .query_one(
                "SELECT hash FROM runbook.skill_version \
                 WHERE org_id = $1 AND project_id = $2 AND name = $3 \
                 ORDER BY seq DESC LIMIT 1",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &version.name,
                ],
            )
            .await
            .map_err(db)?
            .get(0);
        Ok(latest == version.hash)
    }

    pub(crate) async fn record_skill_in(
        &self,
        c: &tokio_postgres::Client,
        version: SkillVersion,
    ) -> Result<SkillVersion, StoreError> {
        let inserted = c
            .execute(
                &format!(
                    "INSERT INTO runbook.skill_version \
                         (org_id, project_id, {COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                         ON CONFLICT (org_id, project_id, name, hash) DO NOTHING"
                ),
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &version.name,
                    &version.description,
                    &version.hash.as_slice(),
                    &encode(&version.manifest)?,
                    &encode(&version.provenance)?,
                ],
            )
            .await
            .map_err(db)?;
        if inserted == 1 {
            return Ok(version);
        }
        let row = c
            .query_one(
                &format!(
                    "SELECT {COLUMNS} FROM runbook.skill_version \
                         WHERE org_id = $1 AND project_id = $2 AND name = $3 AND hash = $4"
                ),
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &version.name,
                    &version.hash.as_slice(),
                ],
            )
            .await
            .map_err(db)?;
        skill_of(&row)
    }
}
