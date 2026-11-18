//! B3.1: draft identity and immutable publication on Postgres.
mod support;
use runbook_core::{
    CommitResult, StoreError,
    blob::{BlobKey, BlobPrefix, BlobStore},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord},
    publish::{prepare, publish_draft, publish_preview},
};
use runbook_model::{RunbookId, VersionId, VersionSelector};
use runbook_store_conformance::scope;
use runbook_store_postgres::PostgresExecutionStore;
use std::num::NonZeroU32;
use support::factory::{PostgresFactory, with_factory};

fn rb(n: u8) -> RunbookId {
    format!("rb_{n:026}").parse().unwrap()
}
fn rbv(n: u8) -> VersionId {
    format!("rbv_{n:026}").parse().unwrap()
}
fn source(slug: &str, title: &str) -> String {
    format!(
        "# keep this comment\napiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: {slug}, title: {title}}}\n\
         spec: {{steps: [{{id: only, title: Only}}]}}\n"
    )
}
fn numbers(default: &str, threshold: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: numbers, title: Numbers}}\n\
         spec:\n  inputs: {{ratio: {{type: number, default: {default}}}}}\n  steps:\n    \
         - id: check\n      title: Check\n      with: {{threshold: {threshold}}}\n"
    )
}
async fn create(f: &PostgresFactory, n: u8, slug: &str, source: String) -> PostgresExecutionStore {
    let store = f.fresh_store(scope(1)).await;
    store
        .create_runbook(
            RunbookRecord {
                runbook_id: rb(n),
                slug: slug.into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            source,
        )
        .await
        .unwrap();
    store
}
fn prepared(slug: &str, source: &str, n: u8) -> runbook_core::publish::PreparedPublication {
    prepare(
        slug,
        source,
        &runbook_spec::CompileContext::default(),
        rbv(n),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn draft_save_preserves_source_and_cas() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        let foreign = f.fresh_store(scope(2)).await;
        let original = source("demo", "Demo");
        let view = store
            .create_runbook(
                RunbookRecord {
                    runbook_id: rb(1),
                    slug: "demo".into(),
                    origin: RunbookOrigin::Ui,
                    archived: false,
                },
                original.clone(),
            )
            .await?;
        assert_eq!(view.source, original);
        assert_eq!(view.revision.get(), 1);
        assert!(store.list_versions(&rb(1)).await?.is_empty());
        // Two writes at revision 1: one revision 2, one conflict.
        let edited = source("demo", "Demo edited");
        let id = rb(1);
        let (a, b) = tokio::join!(store.save_draft(&id, 1, edited.clone()), async {
            foreign.save_draft(&id, 1, edited.clone()).await
        });
        assert_eq!(a?, CommitResult::Committed(2));
        assert_eq!(b, Err(StoreError::NotFound));
        assert_eq!(
            store.save_draft(&rb(1), 1, edited.clone()).await?,
            CommitResult::Conflict
        );
        let draft = store.get_draft(&rb(1)).await?.unwrap();
        assert_eq!(draft.source, edited);
        assert_eq!(draft.revision.get(), 2);
        assert_eq!(foreign.get_draft(&rb(1)).await?, None);
        assert_eq!(foreign.read_runbook(&rb(1)).await?, None);
        // D23: invalid YAML saves verbatim; diagnostics are a separate read.
        let invalid = "not: [valid".to_string();
        assert_eq!(
            store.save_draft(&rb(1), 2, invalid.clone()).await?,
            CommitResult::Committed(3)
        );
        assert_eq!(store.get_draft(&rb(1)).await?.unwrap().source, invalid);
        assert!(
            !runbook_core::publish::draft_diagnostics(
                &invalid,
                &runbook_spec::CompileContext::default()
            )
            .is_empty()
        );
        assert!(store.list_versions(&rb(1)).await?.is_empty());
        // Git-managed runbooks reject draft writes and draft publication.
        store
            .create_runbook(
                RunbookRecord {
                    runbook_id: rb(7),
                    slug: "from-git".into(),
                    origin: RunbookOrigin::Git,
                    archived: false,
                },
                source("from-git", "From Git"),
            )
            .await?;
        assert_eq!(
            store
                .save_draft(&rb(7), 1, source("from-git", "Edited"))
                .await,
            Err(StoreError::InvalidBatch("runbook.git_managed"))
        );
        let git_source = source("from-git", "From Git");
        assert_eq!(
            publish_draft(
                &store,
                &store,
                &rb(7),
                1,
                prepared("from-git", &git_source, 7)
            )
            .await,
            Err(StoreError::InvalidBatch("runbook.git_managed"))
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn publish_selects_reviewed_revision() {
    with_factory(async |f| {
        let store = create(f, 1, "demo", source("demo", "Demo")).await;
        let reviewed = prepared("demo", &source("demo", "Demo"), 1);
        store
            .save_draft(&rb(1), 1, source("demo", "Changed after review"))
            .await?;
        // The prepared publication was reviewed at revision 1; the draft
        // moved on, so publishing it fails its precondition.
        assert!(
            publish_draft(&store, &store, &rb(1), 1, reviewed)
                .await
                .is_err()
        );
        assert!(store.list_versions(&rb(1)).await?.is_empty());
        // Publishing the current revision succeeds.
        let current = prepared("demo", &source("demo", "Changed after review"), 2);
        let view = publish_draft(&store, &store, &rb(1), 2, current).await?;
        assert_eq!(view.runbook_version.get(), 1);
        // A6 policy is part of preparation: an auto-decided gate reaching
        // destructive work without opt-in never becomes a publication.
        let gated = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
            metadata: {id: demo, title: Demo}\nspec:\n  steps:\n    \
            - id: gate\n      approval: {prompt: \"Go?\", options: [{id: rollback, auto: true}], approvers: {users: [sam]}, timeout: 1m, on_timeout: {decide: rollback}}\n    \
            - id: group\n      steps: [{id: rollback, title: Task, destructive: true}]\n";
        let context = runbook_spec::CompileContext {
            users: [(
                "sam".into(),
                "usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            )]
            .into(),
            ..Default::default()
        };
        let diagnostics = prepare("demo", gated, &context, rbv(9)).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "spec.auto_decision_destructive"),
            "{diagnostics:?}"
        );
        // The slug is fixed at creation (D23).
        let diagnostics = prepare(
            "demo",
            &source("renamed", "Demo"),
            &runbook_spec::CompileContext::default(),
            rbv(10),
        )
        .unwrap_err();
        assert!(diagnostics.iter().any(|d| d.code == "runbook.id_mismatch"));
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn publish_numbers_and_reverts_are_atomic() {
    with_factory(async |f| {
        let store = create(f, 1, "demo", source("demo", "One")).await;
        let other = f.fresh_store(scope(1)).await;
        // Two independent clients publish the same reviewed content.
        let one = source("demo", "One");
        let id = rb(1);
        let (a, b) = tokio::join!(
            publish_draft(&store, &store, &id, 1, prepared("demo", &one, 1)),
            async { publish_draft(&other, &other, &id, 1, prepared("demo", &one, 2)).await }
        );
        let (a, b) = (a?, b?);
        assert_eq!(a, b);
        assert_eq!(a.runbook_version.get(), 1);
        assert_eq!(store.list_versions(&rb(1)).await?.len(), 1);
        // Distinct reviewed drafts number successively; a revert is new.
        store.save_draft(&rb(1), 1, source("demo", "Two")).await?;
        let two = publish_draft(
            &store,
            &store,
            &rb(1),
            2,
            prepared("demo", &source("demo", "Two"), 3),
        )
        .await?;
        assert_eq!(two.runbook_version.get(), 2);
        store.save_draft(&rb(1), 2, source("demo", "One")).await?;
        let three = publish_draft(
            &store,
            &store,
            &rb(1),
            3,
            prepared("demo", &source("demo", "One"), 4),
        )
        .await?;
        assert_eq!(three.runbook_version.get(), 3);
        assert_ne!(three.runbook_version_id, a.runbook_version_id);
        // A stale publish allocates no visible number.
        assert!(
            publish_draft(
                &store,
                &store,
                &rb(1),
                2,
                prepared("demo", &source("demo", "Two"), 5)
            )
            .await
            .is_err()
        );
        assert_eq!(store.list_versions(&rb(1)).await?.len(), 3);
        // A preview has an rbv_ and no number, and never becomes latest.
        let preview = publish_preview(
            &store,
            &store,
            &rb(1),
            prepared("demo", &source("demo", "Preview"), 6),
            Some("feature".into()),
            Some("abc123".into()),
        )
        .await?;
        assert_eq!(preview.runbook_version, None);
        assert_eq!(preview.branch.as_deref(), Some("feature"));
        assert_eq!(
            store.resolve_version(&rb(1), None).await?.runbook_version,
            NonZeroU32::new(3)
        );
        assert_eq!(store.list_previews(&rb(1)).await?, vec![preview]);
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn version_resolution_is_runbook_scoped() {
    with_factory(async |f| {
        let store = create(f, 1, "demo", source("demo", "One")).await;
        let one = publish_draft(
            &store,
            &store,
            &rb(1),
            1,
            prepared("demo", &source("demo", "One"), 1),
        )
        .await?;
        store.save_draft(&rb(1), 1, source("demo", "Two")).await?;
        let two = publish_draft(
            &store,
            &store,
            &rb(1),
            2,
            prepared("demo", &source("demo", "Two"), 2),
        )
        .await?;
        assert_eq!(
            store
                .resolve_version(&rb(1), None)
                .await?
                .runbook_version_id,
            two.runbook_version_id
        );
        assert_eq!(
            store
                .resolve_version(
                    &rb(1),
                    Some(VersionSelector::Number(NonZeroU32::new(1).unwrap()))
                )
                .await?
                .runbook_version_id,
            one.runbook_version_id
        );
        assert_eq!(
            store
                .resolve_version(
                    &rb(1),
                    Some(VersionSelector::Id(two.runbook_version_id.clone()))
                )
                .await?
                .runbook_version,
            NonZeroU32::new(2)
        );
        // An already resolved version is unchanged by the later publish.
        assert_eq!(
            store
                .resolve_version(
                    &rb(1),
                    Some(VersionSelector::Number(NonZeroU32::new(1).unwrap()))
                )
                .await?,
            store
                .resolve_version(
                    &rb(1),
                    Some(VersionSelector::Id(one.runbook_version_id.clone()))
                )
                .await?
        );
        // Nonexistent, foreign-runbook and foreign-scope are one NotFound.
        let foreign = f.fresh_store(scope(2)).await;
        for result in [
            store
                .resolve_version(
                    &rb(1),
                    Some(VersionSelector::Number(NonZeroU32::new(9).unwrap())),
                )
                .await,
            store
                .resolve_version(&rb(1), Some(VersionSelector::Id(rbv(99))))
                .await,
            store.resolve_version(&rb(9), None).await,
            foreign.resolve_version(&rb(1), None).await,
        ] {
            assert_eq!(result, Err(StoreError::NotFound));
        }
        // Another runbook's version ID does not resolve under this runbook.
        let other = create(f, 2, "other", source("other", "Other")).await;
        let other_view = publish_draft(
            &other,
            &other,
            &rb(2),
            1,
            prepared("other", &source("other", "Other"), 3),
        )
        .await?;
        assert_eq!(
            store
                .resolve_version(
                    &rb(1),
                    Some(VersionSelector::Id(other_view.runbook_version_id))
                )
                .await,
            Err(StoreError::NotFound)
        );
        Ok(())
    })
    .await;
}

struct UnavailableBlobs;
impl BlobStore for UnavailableBlobs {
    async fn put(&self, _key: BlobKey, _bytes: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::Backend("test.blob_unavailable"))
    }
    async fn get(&self, _key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Backend("test.blob_unavailable"))
    }
    async fn get_range(
        &self,
        _key: &BlobKey,
        _range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Backend("test.blob_unavailable"))
    }
    async fn head(&self, _key: &BlobKey) -> Result<runbook_core::blob::BlobHead, StoreError> {
        Err(StoreError::Backend("test.blob_unavailable"))
    }
    async fn delete(&self, _key: &BlobKey) -> Result<(), StoreError> {
        Err(StoreError::Backend("test.blob_unavailable"))
    }
    async fn list_prefix(&self, _prefix: &BlobPrefix) -> Result<Vec<BlobKey>, StoreError> {
        Err(StoreError::Backend("test.blob_unavailable"))
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn publish_blob_failure_creates_no_version() {
    with_factory(async |f| {
        let store = create(f, 1, "numbers", numbers("3.0", "0.5")).await;
        let candidate = prepared("numbers", &numbers("3.0", "0.5"), 1);
        assert!(
            publish_draft(&store, &UnavailableBlobs, &rb(1), 1, candidate.clone())
                .await
                .is_err()
        );
        assert!(store.list_versions(&rb(1)).await?.is_empty());
        assert_eq!(store.get_draft(&rb(1)).await?.unwrap().revision.get(), 1);
        let view = publish_draft(&store, &store, &rb(1), 1, candidate.clone()).await?;
        // The stored snapshot restores to the catalogued semantic hash.
        let bytes = store
            .get(&BlobKey::Shared {
                hash: candidate.snapshot_digest,
            })
            .await?;
        assert_eq!(bytes, candidate.snapshot);
        assert_eq!(
            *runbook_spec::restore(&bytes).unwrap().hash(),
            candidate.semantic_hash
        );
        let catalogued: Vec<u8> = f
            .admin
            .client()
            .query_one(
                "SELECT semantic_hash FROM runbook.runbook_version WHERE version_id = $1",
                &[&view.runbook_version_id.as_str()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(catalogued, candidate.semantic_hash.as_slice());
        // D21: a numeric-spelling variant shares the semantic hash, has a
        // different snapshot digest, and publishing it returns the latest.
        store.save_draft(&rb(1), 1, numbers("3.00", "0.50")).await?;
        let variant = prepared("numbers", &numbers("3.00", "0.50"), 2);
        assert_eq!(variant.semantic_hash, candidate.semantic_hash);
        assert_ne!(variant.snapshot_digest, candidate.snapshot_digest);
        assert_eq!(
            publish_draft(&store, &store, &rb(1), 2, variant).await?,
            view
        );
        assert_eq!(store.list_versions(&rb(1)).await?.len(), 1);
        Ok(())
    })
    .await;
}

/// C1: a newly numbered version commits a `runbook.published` row in the
/// project outbox; duplicates, previews and failed publishes add none.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn publication_commits_runbook_published_in_its_transaction() {
    use runbook_core::webhooks::ProjectOutbox;
    with_factory(async |f| {
        let store = create(f, 1, "demo", source("demo", "One")).await;
        let other = f.fresh_store(scope(1)).await;
        let one = source("demo", "One");
        let id = rb(1);
        let (a, b) = tokio::join!(
            publish_draft(&store, &store, &id, 1, prepared("demo", &one, 1)),
            async { publish_draft(&other, &other, &id, 1, prepared("demo", &one, 2)).await }
        );
        assert_eq!(a?, b?);
        let pending = store.pending_project_events(10).await?;
        assert_eq!(pending.len(), 1, "{pending:?}");
        let event = &pending[0];
        assert_eq!(event.event_type, "runbook.published");
        assert_eq!(event.runbook_id.as_ref(), Some(&id));
        assert_eq!(event.data["runbook_slug"], "demo");
        assert_eq!(event.data["runbook_version"], 1);
        assert!(!event.data.to_string().contains("apiVersion"));
        // A stale publish and a preview record nothing.
        assert!(
            publish_draft(&store, &store, &id, 7, prepared("demo", &one, 3))
                .await
                .is_err()
        );
        publish_preview(
            &store,
            &store,
            &id,
            prepared("demo", &source("demo", "Preview"), 4),
            None,
            None,
        )
        .await?;
        assert_eq!(store.pending_project_events(10).await?, pending);
        assert!(
            f.fresh_store(scope(2))
                .await
                .pending_project_events(10)
                .await?
                .is_empty()
        );
        store.save_draft(&id, 1, source("demo", "Two")).await?;
        let two = publish_draft(
            &store,
            &store,
            &id,
            2,
            prepared("demo", &source("demo", "Two"), 5),
        )
        .await?;
        let pending = store.pending_project_events(10).await?;
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending[1].data["runbook_version_id"],
            two.runbook_version_id.as_str()
        );
        // Fanned-out rows are removed; a repeated acknowledgement is fine.
        store.ack_project_event(&pending[0].event_id).await?;
        store.ack_project_event(&pending[0].event_id).await?;
        assert_eq!(store.pending_project_events(10).await?, pending[1..]);
        Ok(())
    })
    .await;
}
