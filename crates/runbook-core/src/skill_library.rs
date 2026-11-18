//! Project skill library orchestration (B3.2b): validate a bundle with the
//! pure D19 rules, store every file's bytes under its own digest through
//! BlobStore, then record the immutable version in the catalog.
use crate::{
    StoreError,
    blob::{BlobKey, BlobStore},
    catalog::{CatalogStore, SkillProvenance, SkillVersion},
    skills::{SkillError, SkillFile, build_bundle},
};
use runbook_model::RunbookId;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum PublishSkillError {
    Bundle(SkillError),
    Store(StoreError),
}
impl From<StoreError> for PublishSkillError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl std::fmt::Display for PublishSkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bundle(error) => f.write_str(error.code()),
            Self::Store(_) => f.write_str("skill.store_failed"),
        }
    }
}
impl std::error::Error for PublishSkillError {}

/// Validate the bundle, store each file under its byte digest, then record
/// the version. A validation failure publishes nothing.
pub async fn publish_skill(
    catalog: &impl CatalogStore,
    blobs: &impl BlobStore,
    files: &[SkillFile],
    provenance: SkillProvenance,
    max_bytes: u64,
) -> Result<SkillVersion, PublishSkillError> {
    let version = prepare_skill(blobs, files, provenance, max_bytes).await?;
    Ok(catalog.record_skill_version(version).await?)
}

pub async fn prepare_skill(
    blobs: &impl BlobStore,
    files: &[SkillFile],
    provenance: SkillProvenance,
    max_bytes: u64,
) -> Result<SkillVersion, PublishSkillError> {
    let bundle = build_bundle(files, max_bytes).map_err(PublishSkillError::Bundle)?;
    for file in files {
        blobs
            .put(
                BlobKey::Shared {
                    hash: Sha256::digest(&file.content).into(),
                },
                &file.content,
            )
            .await?;
    }
    Ok(SkillVersion {
        name: bundle.name,
        description: bundle.description,
        hash: bundle.hash,
        manifest: bundle.manifest,
        provenance,
    })
}

/// Resolve compile targets to latest folder hashes for `CompileContext.skills`.
/// A `skill://<name>` target names a project skill; any other target (a spec
/// alias value such as `./skills/x`) matches the provenance reference a
/// version was uploaded for, else its trailing segment as a skill name.
/// Registry forms (`skill://owner/name@version`) are not yet resolvable.
pub async fn resolve_skill_pins(
    catalog: &impl CatalogStore,
    references: &[String],
) -> Result<BTreeMap<String, [u8; 32]>, StoreError> {
    let latest = catalog.latest_skills().await?;
    let mut pins = BTreeMap::new();
    for reference in references {
        let hash = resolve_skill_reference(&latest, reference)
            .ok_or(StoreError::InvalidBatch("skill.unresolved_reference"))?;
        pins.insert(reference.clone(), hash);
    }
    Ok(pins)
}

/// Read one bundled file by folder hash. Only paths listed in that
/// version's manifest are readable — never a filesystem lookup.
pub async fn read_pinned_file(
    catalog: &impl CatalogStore,
    blobs: &impl BlobStore,
    hash: &[u8; 32],
    relative_path: &str,
) -> Result<Vec<u8>, StoreError> {
    let version = catalog
        .skill_version(hash)
        .await?
        .ok_or(StoreError::NotFound)?;
    let entry = version
        .manifest
        .entries
        .iter()
        .find(|entry| entry.path == relative_path)
        .ok_or(StoreError::NotFound)?;
    let digest = parse_digest(&entry.sha256)?;
    blobs.get(&BlobKey::Shared { hash: digest }).await
}

fn parse_digest(hex: &str) -> Result<[u8; 32], StoreError> {
    let mut digest = [0u8; 32];
    if hex.len() != 64 {
        return Err(StoreError::Backend("skill.invalid_manifest_digest"));
    }
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
            .map_err(|_| StoreError::Backend("skill.invalid_manifest_digest"))?;
    }
    Ok(digest)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillUsage {
    pub runbook_id: RunbookId,
    pub version_id: runbook_model::VersionId,
    pub step_ids: Vec<String>,
}

/// Which stored runbook versions pin this skill hash, and at which steps.
/// Step IDs come from restoring each version's snapshot and matching its
/// recorded pins — live library state never changes the answer.
pub async fn skill_usage(
    catalog: &impl CatalogStore,
    blobs: &impl BlobStore,
    hash: &[u8; 32],
) -> Result<Vec<SkillUsage>, StoreError> {
    if catalog.skill_version(hash).await?.is_none() {
        return Err(StoreError::NotFound);
    }
    let mut usage = Vec::new();
    for (runbook_id, version) in catalog.versions_pinning_skill(hash).await? {
        let bytes = blobs
            .get(&BlobKey::Shared {
                hash: version.snapshot_digest,
            })
            .await?;
        let compiled = runbook_spec::restore(&bytes)
            .map_err(|_| StoreError::Backend("skill.snapshot_restore_failed"))?;
        let references: Vec<&str> = compiled
            .skill_hashes()
            .iter()
            .filter(|(_, pinned)| *pinned == hash)
            .map(|(reference, _)| reference.as_str())
            .collect();
        let mut step_ids = Vec::new();
        collect_steps(compiled.ir().steps.as_slice(), &references, &mut step_ids);
        collect_steps(compiled.ir().finally.as_slice(), &references, &mut step_ids);
        usage.push(SkillUsage {
            runbook_id,
            version_id: version.version_id,
            step_ids,
        });
    }
    usage.sort_by(|a, b| (&a.runbook_id, &a.version_id).cmp(&(&b.runbook_id, &b.version_id)));
    Ok(usage)
}

fn collect_steps(nodes: &[runbook_spec::ir::Node], references: &[&str], out: &mut Vec<String>) {
    use runbook_spec::ir::NodeKind;
    for node in nodes {
        match &node.kind {
            NodeKind::Task(task) => {
                if let Some(reference) = &task.skill
                    && references.contains(&reference.as_str())
                {
                    out.push(node.id.clone());
                }
            }
            NodeKind::Group(group) => {
                collect_steps(&group.children, references, out);
                collect_steps(&group.catch, references, out);
                collect_steps(&group.finally, references, out);
            }
            NodeKind::Parallel { children, .. } => collect_steps(children, references, out),
            NodeKind::Foreach(foreach) => collect_steps(&foreach.children, references, out),
            NodeKind::Loop(r#loop) => collect_steps(&r#loop.children, references, out),
            NodeKind::Switch(switch) => {
                for case in &switch.cases {
                    collect_steps(&case.children, references, out);
                }
                collect_steps(&switch.default, references, out);
            }
            NodeKind::Wait(_)
            | NodeKind::Approval(_)
            | NodeKind::Set(_)
            | NodeKind::End(_)
            | NodeKind::Call(_) => {}
        }
    }
}

/// Resolve an address against a single project library snapshot.
pub fn resolve_skill_reference(latest: &[SkillVersion], reference: &str) -> Option<[u8; 32]> {
    runbook_spec::authoring::resolve_skill_reference(latest, reference)
}
impl runbook_spec::authoring::SkillEntry for SkillVersion {
    fn skill_name(&self) -> &str {
        &self.name
    }
    fn skill_reference(&self) -> Option<&str> {
        self.provenance.reference.as_deref()
    }
    fn skill_hash(&self) -> [u8; 32] {
        self.hash
    }
}
