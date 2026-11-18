//! Authoring transaction contract. Blobs and compilation precede this short commit;
//! directory authorization revision, catalog mutation and POST replay are atomic.
use crate::{
    StoreError,
    blob::BlobStore,
    catalog::*,
    directory_operations::{RequestReplay, StoredResponse},
    publish::PreparedPublication,
};
use runbook_model::*;
use std::future::Future;
#[derive(Clone, Debug)]
pub enum AuthoringCommand {
    Create {
        record: RunbookRecord,
        source: String,
        diagnostics: Vec<Diagnostic>,
    },
    Save {
        runbook_id: RunbookId,
        revision: u64,
        source: String,
        diagnostics: Vec<Diagnostic>,
    },
    Publish {
        runbook_id: RunbookId,
        revision: u64,
        prepared: Box<PreparedPublication>,
    },
    Skill {
        version: SkillVersion,
    },
    /// Git sync (2026-11-17): every file compiled and its snapshot stored
    /// before the commit; the stores apply the batch in one transaction
    /// where they have one (Postgres), in order otherwise.
    Sync {
        ops: Vec<SyncOp>,
        /// Git-managed runbooks without a file (`prune`): slug and ID.
        archive: Vec<(String, RunbookId)>,
        branch: Option<String>,
        commit_sha: Option<String>,
    },
}

/// One file of a sync, ready to publish.
#[derive(Clone, Debug)]
pub struct SyncOp {
    pub path: String,
    pub slug: String,
    pub runbook_id: RunbookId,
    /// The new runbook's record (origin Git) when it does not exist yet.
    pub create: Option<RunbookRecord>,
    /// It exists and was authored in the UI: it becomes Git-managed.
    pub adopt: bool,
    pub prepared: Box<PreparedPublication>,
}

/// The sync response once each op published (in order): `create`, `adopt`,
/// `update`, or `unchanged` when the store returned the latest version (a
/// semantic duplicate), then `archive` rows.
pub fn sync_response(
    ops: &[SyncOp],
    published: &[PublishedVersionView],
    archive: &[(String, RunbookId)],
    branch: Option<String>,
    commit_sha: Option<String>,
) -> StoredResponse {
    let mut runbooks: Vec<SyncRunbookView> = ops
        .iter()
        .zip(published)
        .map(|(op, view)| SyncRunbookView {
            path: Some(op.path.clone()),
            slug: op.slug.clone(),
            runbook_id: Some(op.runbook_id.clone()),
            action: if op.create.is_some() {
                "create"
            } else if op.adopt {
                "adopt"
            } else if view.runbook_version_id == op.prepared.version_id {
                "update"
            } else {
                "unchanged"
            }
            .into(),
            runbook_version: Some(view.runbook_version.get()),
            runbook_version_id: Some(view.runbook_version_id.clone()),
        })
        .collect();
    runbooks.extend(archive.iter().map(|(slug, id)| SyncRunbookView {
        path: None,
        slug: slug.clone(),
        runbook_id: Some(id.clone()),
        action: "archive".into(),
        runbook_version: None,
        runbook_version_id: None,
    }));
    StoredResponse {
        status: 200,
        body: serde_json::json!(SyncView {
            dry_run: false,
            commit_sha,
            branch,
            runbooks,
        }),
        etag: None,
    }
}
pub trait AuthoringStore {
    type Catalog: CatalogStore + BlobStore;
    fn catalog(&self) -> &Self::Catalog;
    fn catalog_runbooks(&self) -> impl Future<Output = Result<Vec<RunbookRecord>, StoreError>>;
    /// UI-1c: this project's runbook whose slug is exactly `slug`, if any
    /// (`GET /v1/projects/{p}/runbooks/{slug}`). The SQL stores read the
    /// unique `(org_id, project_id, slug)` index; this default (memory and
    /// test stores) scans [`AuthoringStore::catalog_runbooks`].
    fn runbook_by_slug(
        &self,
        slug: &str,
    ) -> impl Future<Output = Result<Option<RunbookRecord>, StoreError>> {
        async move {
            Ok(self
                .catalog_runbooks()
                .await?
                .into_iter()
                .find(|record| record.slug == slug))
        }
    }
    /// UI-1b: one page of the runbook list ([`crate::listing`]): the
    /// runbooks matching `query`, after its cursor, newest ID first, at most
    /// `query.limit`, each with its list fields and draft digest. A store
    /// must answer this from rows (no source reads per runbook); the D1
    /// store reads the listing it keeps beside each draft and filters and
    /// cuts in SQL, Postgres derives listings while reading rows in chunks.
    /// A store may stop early, saying how far it looked
    /// ([`crate::listing::ListingPage::examined_to`]). This default (the
    /// reference) reads every draft.
    fn runbook_listing_page(
        &self,
        query: &crate::listing::ListingQuery,
    ) -> impl Future<Output = Result<crate::listing::ListingPage, StoreError>> {
        async move {
            let mut rows = vec![];
            for record in self.catalog_runbooks().await? {
                let draft = self
                    .catalog()
                    .get_draft(&record.runbook_id)
                    .await?
                    .map(|d| d.source);
                let listing = crate::listing::DraftListing::of(&record.slug, draft.as_deref());
                rows.push((record, listing));
            }
            Ok(crate::listing::ListingPage::complete(query.page(rows)))
        }
    }
    /// UI-1b: for each of these runbooks that has a published version, its
    /// highest number and the SHA-256 of that version's source (the
    /// runbook list's `latest_version` and `draft_ahead`). The SQL adapters
    /// read a whole page in one query and no source blobs; this default
    /// reads each runbook's latest version.
    fn latest_published_digests(
        &self,
        runbook_ids: &[RunbookId],
    ) -> impl Future<
        Output = Result<
            std::collections::BTreeMap<RunbookId, (std::num::NonZeroU32, [u8; 32])>,
            StoreError,
        >,
    > {
        async move {
            let mut out = std::collections::BTreeMap::new();
            for id in runbook_ids {
                match self.version_data(id, None).await {
                    Ok(VersionRecord {
                        number: Some(number),
                        source,
                        ..
                    }) => {
                        out.insert(
                            id.clone(),
                            (number, crate::portable::sha256(source.as_bytes())),
                        );
                    }
                    Ok(_) | Err(StoreError::NotFound) => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(out)
        }
    }
    fn version_data(
        &self,
        id: &RunbookId,
        selector: Option<VersionSelector>,
    ) -> impl Future<Output = Result<VersionRecord, StoreError>>;
    fn skill_versions(&self) -> impl Future<Output = Result<Vec<SkillVersion>, StoreError>>;
    fn commit_authoring(
        &self,
        directory_revision: u64,
        replay: Option<RequestReplay>,
        command: AuthoringCommand,
    ) -> impl Future<Output = Result<StoredResponse, StoreError>>;
}
pub trait AuthoringFactory {
    type Store: AuthoringStore;
    fn scoped(&self, scope: TenantScope) -> Self::Store;
    /// Explicit org-scoped key read, never a scan of all tenants.
    fn project_for_runbook(
        &self,
        org: &OrganizationId,
        id: &RunbookId,
    ) -> impl Future<Output = Result<Option<ProjectId>, StoreError>>;
}
pub fn draft_etag(draft: &DraftView) -> String {
    format!("\"draft-{}-{}\"", draft.runbook_id, draft.revision)
}
pub fn draft_response(
    draft: DraftView,
    diagnostics: Vec<Diagnostic>,
    status: u16,
) -> StoredResponse {
    StoredResponse {
        status,
        etag: Some(draft_etag(&draft)),
        body: serde_json::json!(DraftWriteView { draft, diagnostics }),
    }
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// `latest`: whether `version` is its name's latest (`latest_skills`).
pub fn skill_view(version: SkillVersion, latest: bool) -> SkillVersionView {
    SkillVersionView {
        name: version.name,
        description: version.description,
        hash: hex(&version.hash),
        files: version
            .manifest
            .entries
            .into_iter()
            .map(|f| SkillFileView {
                path: f.path,
                size: f.size,
                sha256: f.sha256,
                content: None,
            })
            .collect(),
        reference: version.provenance.reference,
        usages: vec![],
        latest,
    }
}
