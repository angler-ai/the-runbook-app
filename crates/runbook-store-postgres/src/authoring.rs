//! Authoring only: directory revision -> runbook row. No execution/admission locks.
use crate::{
    DatabaseConfig, PostgresExecutionStore,
    execution::READ_COMMITTED,
    pool::Pool,
    rows::{connection, db, decode, encode, int, uint},
};
use runbook_core::{
    CommitResult, StoreError,
    authoring::*,
    catalog::*,
    directory_operations::{ReplayResult, RequestReplay, StoredResponse},
};
use runbook_model::*;
use std::{cell::Cell, rc::Rc};
/// UI-1b review round 1: rows one keyset read of a searched runbook list
/// examines (each carries its draft, up to 1 MiB).
const LISTING_CHUNK: i64 = 32;
/// One read of the runbook list: the tenant's runbooks after `$3` (none
/// when NULL) in the list's order (byte order, as the reference compares
/// IDs), at most `$4`, each with its draft. Binds: org, project, after,
/// limit. UI-1c: migration 0033's `runbook_listing_order` index has exactly
/// this order, so a chunk reads its rows from the index instead of sorting
/// the project's runbooks (`tests/listing.rs` checks the plan).
pub const RUNBOOK_LISTING_CHUNK_SQL: &str = "SELECT runbook_id, slug, origin, archived, draft_source FROM runbook.runbook \
     WHERE org_id = $1 AND project_id = $2 \
       AND ($3::text IS NULL OR runbook_id < $3::text COLLATE \"C\") \
     ORDER BY runbook_id COLLATE \"C\" DESC LIMIT $4";
/// UI-1c: one runbook by its slug. Binds: org, project, slug. One probe of
/// the unique `(org_id, project_id, slug)` index migration 0009 declares
/// (`runbook_org_id_project_id_slug_key`; `tests/listing.rs` checks the
/// plan).
pub const RUNBOOK_BY_SLUG_SQL: &str = "SELECT runbook_id, slug, origin, archived \
     FROM runbook.runbook WHERE org_id = $1 AND project_id = $2 AND slug = $3";
#[derive(Clone)]
pub struct PostgresAuthoring {
    config: DatabaseConfig,
    pub(crate) pool: Rc<Pool>,
    fail: Rc<Cell<bool>>,
}
impl PostgresAuthoring {
    pub fn new(config: DatabaseConfig) -> Self {
        Self {
            pool: Pool::new(config.clone()),
            config,
            fail: Rc::default(),
        }
    }
    #[doc(hidden)]
    pub fn fail_before_replay(&self) {
        self.fail.set(true);
    }
}
pub struct PostgresAuthoringCatalog {
    store: PostgresExecutionStore,
    fail: Rc<Cell<bool>>,
}
// The factory's scoped adapter delegates existing catalog/blob interfaces; only
// the atomic authoring commit and keyed read additions live in this module.
impl AuthoringFactory for PostgresAuthoring {
    type Store = PostgresAuthoringCatalog;
    fn scoped(&self, scope: TenantScope) -> Self::Store {
        PostgresAuthoringCatalog {
            store: PostgresExecutionStore::new(self.config.clone(), scope),
            fail: self.fail.clone(),
        }
    }
    async fn project_for_runbook(
        &self,
        org: &OrganizationId,
        id: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        let c = self.pool.get().await.map_err(connection)?;
        c.client()
            .query_opt(
                "SELECT project_id FROM runbook.runbook WHERE org_id=$1 AND runbook_id=$2",
                &[&org.as_str(), &id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|r| {
                r.get::<_, &str>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("catalog.invalid_project"))
            })
            .transpose()
    }
}
impl AuthoringStore for PostgresAuthoringCatalog {
    type Catalog = PostgresExecutionStore;
    fn catalog(&self) -> &Self::Catalog {
        &self.store
    }
    async fn catalog_runbooks(&self) -> Result<Vec<RunbookRecord>, StoreError> {
        let s = &self.store;
        let c = s.pool.get().await.map_err(connection)?;
        c.client().query("SELECT runbook_id,slug,origin,archived FROM runbook.runbook WHERE org_id=$1 AND project_id=$2 ORDER BY runbook_id DESC",&[&s.scope.org_id.as_str(),&s.scope.project_id.as_str()]).await.map_err(db)?.into_iter().map(|r|Ok(RunbookRecord{runbook_id:r.get::<_,&str>(0).parse().map_err(|_|StoreError::Backend("catalog.invalid_id"))?,slug:r.get(1),origin:crate::publish::origin_of(r.get(2))?,archived:r.get(3)})).collect()
    }
    /// UI-1c: one probe of the unique slug index ([`RUNBOOK_BY_SLUG_SQL`]).
    async fn runbook_by_slug(&self, slug: &str) -> Result<Option<RunbookRecord>, StoreError> {
        let s = &self.store;
        let c = s.pool.get().await.map_err(connection)?;
        c.client()
            .query_opt(
                RUNBOOK_BY_SLUG_SQL,
                &[
                    &s.scope.org_id.as_str(),
                    &s.scope.project_id.as_str(),
                    &slug,
                ],
            )
            .await
            .map_err(db)?
            .map(|r| {
                Ok(RunbookRecord {
                    runbook_id: r
                        .get::<_, &str>(0)
                        .parse()
                        .map_err(|_| StoreError::Backend("catalog.invalid_id"))?,
                    slug: r.get(1),
                    origin: crate::publish::origin_of(r.get(2))?,
                    archived: r.get(3),
                })
            })
            .transpose()
    }
    /// UI-1b: Postgres keeps no listing; it derives each one while reading
    /// the tenant's rows in the list's order (byte order, as the reference
    /// compares IDs). Review round 1: the reads are bounded on the server
    /// too, not only in this process: without a search the query asks for
    /// exactly the page (`LIMIT`); with one, it reads keyset chunks of
    /// [`LISTING_CHUNK`] rows after the last row examined and stops at the
    /// chunk that completes the page, so Postgres reads and sends at most
    /// one chunk past the page's last match (a search that matches few
    /// rows still examines the rows before them, in chunks).
    async fn runbook_listing_page(
        &self,
        query: &runbook_core::listing::ListingQuery,
    ) -> Result<runbook_core::listing::ListingPage, StoreError> {
        use futures_util::TryStreamExt;
        use runbook_core::listing::{DraftListing, ListingPage};
        let s = &self.store;
        let c = s.pool.get().await.map_err(connection)?;
        let filtered = query.q.is_some() || !query.tags.is_empty();
        let mut after = query.after.clone();
        let mut out = vec![];
        while out.len() < query.limit {
            let chunk = if filtered {
                LISTING_CHUNK
            } else {
                i64::try_from(query.limit - out.len()).unwrap_or(i64::MAX)
            };
            let position = after.as_deref();
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 4] = [
                &s.scope.org_id.as_str(),
                &s.scope.project_id.as_str(),
                &position,
                &chunk,
            ];
            let rows = c
                .client()
                .query_raw(RUNBOOK_LISTING_CHUNK_SQL, params.iter().copied())
                .await
                .map_err(db)?;
            let mut rows = std::pin::pin!(rows);
            let mut read = 0;
            while out.len() < query.limit {
                let Some(r) = rows.try_next().await.map_err(db)? else {
                    break;
                };
                read += 1;
                let record = RunbookRecord {
                    runbook_id: r
                        .get::<_, &str>(0)
                        .parse()
                        .map_err(|_| StoreError::Backend("catalog.invalid_id"))?,
                    slug: r.get(1),
                    origin: crate::publish::origin_of(r.get(2))?,
                    archived: r.get(3),
                };
                after = Some(record.runbook_id.to_string());
                let source = crate::publish::decode_draft_source(r.get(4))?;
                let listing = DraftListing::of(&record.slug, Some(&source));
                if query.matches(&record.slug, &listing) {
                    out.push(listing.listed(record));
                }
            }
            if read < chunk {
                break;
            }
        }
        Ok(ListingPage::complete(out))
    }
    /// UI-1b: the page's latest published versions with their source's
    /// SHA-256 computed by Postgres, so no source text is sent.
    async fn latest_published_digests(
        &self,
        runbook_ids: &[RunbookId],
    ) -> Result<std::collections::BTreeMap<RunbookId, (std::num::NonZeroU32, [u8; 32])>, StoreError>
    {
        if runbook_ids.is_empty() {
            return Ok(Default::default());
        }
        let s = &self.store;
        let ids: Vec<&str> = runbook_ids.iter().map(|id| id.as_str()).collect();
        let c = s.pool.get().await.map_err(connection)?;
        // One probe of runbook_version_latest (runbook_id, number DESC) per ID.
        c.client()
            .query(
                "SELECT DISTINCT ON (runbook_id) runbook_id, number, \
                   sha256(convert_to(source, 'UTF8')) \
                 FROM runbook.runbook_version \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = ANY($3) \
                   AND number IS NOT NULL \
                 ORDER BY runbook_id, number DESC",
                &[&s.scope.org_id.as_str(), &s.scope.project_id.as_str(), &ids],
            )
            .await
            .map_err(db)?
            .into_iter()
            .map(|r| {
                let id: RunbookId = r
                    .get::<_, &str>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("catalog.invalid_id"))?;
                let number = u32::try_from(r.get::<_, i32>(1))
                    .ok()
                    .and_then(std::num::NonZeroU32::new)
                    .ok_or(StoreError::Backend("catalog.invalid_version_number"))?;
                let digest: [u8; 32] = r
                    .get::<_, Vec<u8>>(2)
                    .try_into()
                    .map_err(|_| StoreError::Backend("catalog.invalid_digest"))?;
                Ok((id, (number, digest)))
            })
            .collect()
    }
    async fn version_data(
        &self,
        id: &RunbookId,
        selector: Option<VersionSelector>,
    ) -> Result<VersionRecord, StoreError> {
        let v = self.store.resolve_version(id, selector).await?;
        let s = &self.store;
        let c = s.pool.get().await.map_err(connection)?;
        let row=c.client().query_opt(&format!("SELECT {} FROM runbook.runbook_version WHERE org_id=$1 AND project_id=$2 AND runbook_id=$3 AND version_id=$4",crate::publish::VERSION_COLUMNS),&[&s.scope.org_id.as_str(),&s.scope.project_id.as_str(),&id.as_str(),&v.runbook_version_id.as_str()]).await.map_err(db)?.ok_or(StoreError::NotFound)?;
        crate::publish::version_record(&row)
    }
    async fn skill_versions(&self) -> Result<Vec<SkillVersion>, StoreError> {
        let s = &self.store;
        let c = s.pool.get().await.map_err(connection)?;
        c.client().query("SELECT name,description,hash,manifest,provenance FROM runbook.skill_version WHERE org_id=$1 AND project_id=$2 ORDER BY name,seq DESC",&[&s.scope.org_id.as_str(),&s.scope.project_id.as_str()]).await.map_err(db)?.into_iter().map(|r|Ok(SkillVersion{name:r.get(0),description:r.get(1),hash:r.get::<_,Vec<u8>>(2).try_into().map_err(|_|StoreError::Backend("skill.invalid_hash"))?,manifest:decode(r.get(3))?,provenance:decode(r.get(4))?})).collect()
    }
    async fn commit_authoring(
        &self,
        revision: u64,
        replay: Option<RequestReplay>,
        command: AuthoringCommand,
    ) -> Result<StoredResponse, StoreError> {
        let s = &self.store;
        s.transaction(READ_COMMITTED,async |c|{
   let org=s.scope.org_id.as_str();
   let row=c.query_opt("SELECT revision FROM runbook.directory_revision WHERE org_id=$1 FOR UPDATE",&[&org]).await.map_err(db)?.ok_or(StoreError::NotFound)?;
   if uint(row.get(0))?!=revision {return Err(StoreError::InvalidBatch("directory.concurrent_change"));}
   let now:i64=c.query_one("SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint",&[]).await.map_err(db)?.get(0);
   if let Some(r)=&replay {match crate::directory::operations::lookup(c,org,r,now).await.map_err(|_|StoreError::Backend("authoring.replay"))? {
    ReplayResult::Applied(response)=>return Ok(response),ReplayResult::KeyReused=>return Err(StoreError::InvalidBatch("request.idempotency_key_reused")),ReplayResult::Missing=>{},ReplayResult::Conflict=>return Err(StoreError::InvalidBatch("directory.concurrent_change")),
   }}
   let response=match command {
    AuthoringCommand::Create{record,source,diagnostics}=>draft_response(s.create_runbook_in(c,record,source).await?,diagnostics,201),
    AuthoringCommand::Save{runbook_id,revision,source,diagnostics}=>{
     match s.save_draft_in(c,&runbook_id,revision,source.clone()).await? {CommitResult::Conflict=>return Err(StoreError::InvalidBatch("catalog.draft_conflict")),CommitResult::Committed(next)=>draft_response(crate::publish::draft_view(&runbook_id,source,int(next)?)?,diagnostics,200)}
    },
    AuthoringCommand::Publish{runbook_id,revision,prepared}=>StoredResponse{status:201,body:serde_json::json!(s.publish_in(c,&runbook_id,revision,*prepared).await?),etag:None},
    AuthoringCommand::Sync{ops,archive,branch,commit_sha}=>{
     let mut published=Vec::with_capacity(ops.len());
     for op in &ops {
      if let Some(record)=&op.create {s.create_runbook_in(c,record.clone(),op.prepared.source.clone()).await?;}
      published.push(s.publish_git_in(c,&op.runbook_id,(*op.prepared).clone(),branch.clone(),commit_sha.clone()).await?);
     }
     for (_,id) in &archive {s.archive_git_in(c,id).await?;}
     runbook_core::authoring::sync_response(&ops,&published,&archive,branch,commit_sha)
    },
    AuthoringCommand::Skill{version}=>{let version=s.record_skill_in(c,version).await?;let latest=s.is_latest_skill_in(c,&version).await?;StoredResponse{status:201,body:serde_json::json!(skill_view(version,latest)),etag:None}},
   };
   if self.fail.replace(false){return Err(StoreError::Backend("injected.authoring_before_replay"));}
   if let Some(r)=replay {
    c.execute("DELETE FROM runbook.directory_request WHERE org_id=$1 AND expires_at<=$2",&[&org,&now]).await.map_err(db)?;
    c.execute("INSERT INTO runbook.directory_request(org_id,principal,operation,key,payload,response,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)",&[&org,&r.principal,&r.operation,&r.key,&encode(&r.payload)?,&encode(&response)?,&now.saturating_add(86_400_000)]).await.map_err(db)?;
   }
   Ok(response)
  }).await
    }
}
