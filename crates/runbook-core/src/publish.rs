//! Publication preparation: compile and policy-check a reviewed draft
//! outside any storage transaction, then store its immutable snapshot
//! before the catalog's short numbering transaction runs.
use crate::{
    StoreError,
    blob::{BlobKey, BlobStore},
    catalog::CatalogStore,
};
use runbook_model::{
    Diagnostic, DraftView, PublishedVersionView, RunbookId, TeamId, UserId, VersionId, VersionView,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The identities compilation selected, pinned onto the version row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedPins {
    pub skills: BTreeMap<String, [u8; 32]>,
    pub users: BTreeMap<String, UserId>,
    pub teams: BTreeMap<String, TeamId>,
}

/// A compile-checked candidate version. The snapshot is stored through
/// BlobStore under its byte digest (D21) before `publish_revision` runs;
/// an unreferenced blob is not a published version.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedPublication {
    pub version_id: VersionId,
    pub semantic_hash: [u8; 32],
    pub snapshot_digest: [u8; 32],
    pub snapshot_size: u64,
    pub source: String,
    pub snapshot: Vec<u8>,
    pub pins: SelectedPins,
    pub warnings: Vec<Diagnostic>,
}

fn wire(diagnostic: runbook_spec::Diagnostic) -> Diagnostic {
    Diagnostic {
        path: diagnostic.path,
        message: diagnostic
            .message
            .unwrap_or_else(|| diagnostic.code.clone()),
        code: diagnostic.code,
        line: u32::try_from(diagnostic.line).ok().filter(|n| *n > 0),
        column: u32::try_from(diagnostic.column).ok().filter(|n| *n > 0),
    }
}
fn wire_all(diagnostics: Vec<runbook_spec::Diagnostic>) -> Vec<Diagnostic> {
    diagnostics.into_iter().map(wire).collect()
}

/// Parse, compile and policy-check without failing the caller: an empty
/// result means the source could publish against this context (D23).
pub fn draft_diagnostics(source: &str, context: &runbook_spec::CompileContext) -> Vec<Diagnostic> {
    match runbook_spec::parse(source).and_then(|spec| runbook_spec::compile(&spec, context)) {
        Ok(compiled) => {
            let mut diagnostics = runbook_spec::validate_policy(&compiled);
            diagnostics.extend(runbook_spec::publication_approval_diagnostics(compiled.ir()).0);
            wire_all(diagnostics)
        }
        Err(diagnostics) => wire_all(diagnostics),
    }
}

/// Compile a reviewed draft into a prepared publication. The slug is the
/// runbook identity fixed at creation; a differently named draft fails
/// with `runbook.id_mismatch` (D23). A6 approval/destructive policy runs
/// here as its own pass, exactly as compile() documents.
pub fn prepare(
    slug: &str,
    source: &str,
    context: &runbook_spec::CompileContext,
    version_id: VersionId,
) -> Result<PreparedPublication, Vec<Diagnostic>> {
    let spec = runbook_spec::parse(source).map_err(wire_all)?;
    let compiled = runbook_spec::compile(&spec, context).map_err(wire_all)?;
    let mut policy = runbook_spec::validate_policy(&compiled);
    let (approval_errors, approval_warnings) =
        runbook_spec::publication_approval_diagnostics(compiled.ir());
    policy.extend(approval_errors);
    if !policy.is_empty() {
        return Err(wire_all(policy));
    }
    if compiled.ir().metadata.id != slug {
        return Err(vec![Diagnostic {
            path: "/metadata/id".into(),
            code: "runbook.id_mismatch".into(),
            message: "Draft metadata.id must match the runbook slug fixed at creation.".into(),
            line: None,
            column: None,
        }]);
    }
    let snapshot = compiled.snapshot_bytes().to_vec();
    Ok(PreparedPublication {
        version_id,
        semantic_hash: *compiled.hash(),
        warnings: wire_all(approval_warnings),
        snapshot_digest: Sha256::digest(&snapshot).into(),
        snapshot_size: snapshot.len() as u64,
        source: source.to_owned(),
        snapshot,
        pins: SelectedPins {
            skills: compiled.skill_hashes().clone(),
            users: compiled.users().clone(),
            teams: compiled.teams().clone(),
        },
    })
}

/// Store the immutable snapshot, then number the version. If the blob
/// store is unavailable the catalog is untouched; a stored but
/// unreferenced blob is not a version.
pub async fn publish_draft(
    catalog: &impl CatalogStore,
    blobs: &impl BlobStore,
    runbook_id: &RunbookId,
    expected_revision: u64,
    prepared: PreparedPublication,
) -> Result<PublishedVersionView, StoreError> {
    blobs
        .put(
            BlobKey::Shared {
                hash: prepared.snapshot_digest,
            },
            &prepared.snapshot,
        )
        .await?;
    catalog
        .publish_revision(runbook_id, expected_revision, prepared)
        .await
}

/// Store the snapshot, then record a preview: an rbv_ identity with no
/// number, never counted as latest.
pub async fn publish_preview(
    catalog: &impl CatalogStore,
    blobs: &impl BlobStore,
    runbook_id: &RunbookId,
    prepared: PreparedPublication,
    branch: Option<String>,
    commit_sha: Option<String>,
) -> Result<VersionView, StoreError> {
    blobs
        .put(
            BlobKey::Shared {
                hash: prepared.snapshot_digest,
            },
            &prepared.snapshot,
        )
        .await?;
    catalog
        .create_preview(runbook_id, prepared, branch, commit_sha)
        .await
}

/// DraftView from a revision a successful save produced (never zero).
pub fn draft_view(runbook_id: RunbookId, source: String, revision: u64) -> DraftView {
    DraftView {
        runbook_id,
        source,
        revision: revision.try_into().expect("draft revisions start at 1"),
    }
}
