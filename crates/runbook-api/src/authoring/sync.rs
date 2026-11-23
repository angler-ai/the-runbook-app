//! `POST /v1/projects/{p}/sync` (2026-11-17): a repository folder's
//! runbooks published as one batch, the open-source path of PRD §12 that
//! `runbook sync push` drives from CI. Every file is compiled first; one
//! error refuses the batch (422 `sync.invalid`, each file's errors listed)
//! and nothing changes. A dry run reports what would change.
use super::*;
use runbook_core::authoring::SyncOp;

pub(super) enum SyncPlan {
    /// A dry run's report, or a refusal: answered without a commit.
    Answer(StoredResponse),
    Commit(AuthoringCommand),
}

/// The batch's `index`-th fresh ID: stable for one request (`new_id`) and
/// distinct per index, in the 26-character form IDs take.
fn nth_id(prefix: &str, new_id: &str, index: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"runbook.sync.v1\0");
    hash.update(new_id.as_bytes());
    hash.update(b"\0");
    hash.update(index.to_be_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    let mut number = u128::from_be_bytes(bytes);
    let mut encoded = [b'0'; 26];
    for byte in encoded.iter_mut().rev() {
        *byte = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[(number & 31) as usize];
        number >>= 5;
    }
    format!(
        "{prefix}{}",
        std::str::from_utf8(&encoded).expect("base32 is ASCII")
    )
}

fn file_error(code: &str, message: &str) -> Diagnostic {
    Diagnostic {
        path: "/metadata/id".into(),
        code: code.into(),
        message: message.into(),
        line: None,
        column: None,
    }
}

impl<D: DirectoryOperations, C: CredentialStore, F: AuthoringFactory> AuthoringService<D, C, F> {
    pub(super) async fn sync_plan(
        &self,
        r: &Request,
        s: &DirectorySnapshot,
        project: &ProjectId,
        store: &F::Store,
    ) -> Result<SyncPlan, Error> {
        let b: SyncRequest = parse(r, &r.body)?;
        if b.files.len() > SYNC_MAX_FILES {
            return Err(error(r, 422, "sync.too_many_files"));
        }
        let text = |value: &Option<String>| {
            value
                .as_deref()
                .is_some_and(|v| v.is_empty() || v.len() > 255 || v.chars().any(char::is_control))
        };
        if text(&b.commit_sha) || text(&b.branch) {
            return Err(error(r, 422, "request.invalid"));
        }
        let existing: BTreeMap<String, RunbookRecord> = store
            .catalog_runbooks()
            .await
            .map_err(|e| store_error(r, e))?
            .into_iter()
            .map(|record| (record.slug.clone(), record))
            .collect();

        let mut failures: Vec<SyncFileErrors> = vec![];
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        let mut ops: Vec<SyncOp> = vec![];
        for (index, file) in b.files.iter().enumerate() {
            if file.path.is_empty()
                || file.path.len() > 1024
                || file.path.chars().any(char::is_control)
            {
                return Err(error(r, 422, "request.invalid"));
            }
            self.source_limit(r, &file.source)?;
            let slug = match creation_slug(
                r,
                &CreateRunbookRequest {
                    source: file.source.clone(),
                    slug: None,
                },
            ) {
                Ok(slug) => slug,
                Err(e) => {
                    let code = e.body["code"].as_str().unwrap_or("runbook.slug_required");
                    failures.push(SyncFileErrors {
                        path: file.path.clone(),
                        errors: vec![file_error(
                            code,
                            "metadata.id must name the runbook (its slug)",
                        )],
                    });
                    continue;
                }
            };
            if let Some(first) = seen.insert(slug.clone(), file.path.clone()) {
                failures.push(SyncFileErrors {
                    path: file.path.clone(),
                    errors: vec![file_error(
                        "sync.duplicate_slug",
                        &format!("{first} has the same metadata.id"),
                    )],
                });
                continue;
            }
            let context = self
                .compile_context(r, s, project, store, &file.source)
                .await?;
            let version_id = parse_id(r, &nth_id("rbv_", &r.new_id, index))?;
            let prepared =
                match runbook_core::publish::prepare(&slug, &file.source, &context, version_id) {
                    Ok(prepared) => prepared,
                    Err(errors) => {
                        failures.push(SyncFileErrors {
                            path: file.path.clone(),
                            errors,
                        });
                        continue;
                    }
                };
            let (runbook_id, create, adopt) = match existing.get(&slug) {
                Some(record) => (
                    record.runbook_id.clone(),
                    None,
                    record.origin == RunbookOrigin::Ui,
                ),
                None => {
                    let runbook_id: RunbookId = parse_id(r, &nth_id("rb_", &r.new_id, index))?;
                    let record = RunbookRecord {
                        runbook_id: runbook_id.clone(),
                        slug: slug.clone(),
                        origin: RunbookOrigin::Git,
                        archived: false,
                    };
                    (runbook_id, Some(record), false)
                }
            };
            ops.push(SyncOp {
                path: file.path.clone(),
                slug,
                runbook_id,
                create,
                adopt,
                prepared: Box::new(prepared),
            });
        }
        if !failures.is_empty() {
            let mut e = error(r, 422, "sync.invalid");
            e.body["files"] = json!(failures);
            return Err(e);
        }
        let archive: Vec<(String, RunbookId)> = if b.prune {
            existing
                .values()
                .filter(|record| {
                    record.origin == RunbookOrigin::Git
                        && !record.archived
                        && !seen.contains_key(&record.slug)
                })
                .map(|record| (record.slug.clone(), record.runbook_id.clone()))
                .collect()
        } else {
            vec![]
        };

        if b.dry_run {
            let mut runbooks = vec![];
            for op in &ops {
                let action = if op.create.is_some() {
                    "create"
                } else if op.adopt {
                    "adopt"
                } else {
                    match store.version_data(&op.runbook_id, None).await {
                        Ok(latest) if latest.semantic_hash == op.prepared.semantic_hash => {
                            "unchanged"
                        }
                        Ok(_) | Err(runbook_core::StoreError::NotFound) => "update",
                        Err(e) => return Err(store_error(r, e)),
                    }
                };
                runbooks.push(SyncRunbookView {
                    warnings: op.prepared.warnings.clone(),
                    path: Some(op.path.clone()),
                    slug: op.slug.clone(),
                    runbook_id: op.create.is_none().then(|| op.runbook_id.clone()),
                    action: action.into(),
                    runbook_version: None,
                    runbook_version_id: None,
                });
            }
            runbooks.extend(archive.iter().map(|(slug, id)| SyncRunbookView {
                warnings: Vec::new(),
                path: None,
                slug: slug.clone(),
                runbook_id: Some(id.clone()),
                action: "archive".into(),
                runbook_version: None,
                runbook_version_id: None,
            }));
            return Ok(SyncPlan::Answer(response(
                200,
                json!(SyncView {
                    dry_run: true,
                    commit_sha: b.commit_sha,
                    branch: b.branch,
                    runbooks,
                }),
            )));
        }

        // Snapshots are immutable, content-addressed blobs: stored before
        // the commit, like a single publish.
        let catalog = store.catalog();
        for op in &ops {
            catalog
                .put(
                    BlobKey::Shared {
                        hash: op.prepared.snapshot_digest,
                    },
                    &op.prepared.snapshot,
                )
                .await
                .map_err(|e| store_error(r, e))?;
        }
        Ok(SyncPlan::Commit(AuthoringCommand::Sync {
            ops,
            archive,
            branch: b.branch,
            commit_sha: b.commit_sha,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::nth_id;

    #[test]
    fn batch_ids_are_stable_distinct_and_well_formed() {
        let a = nth_id("rbv_", "01JABCDEFGHJKMNPQRSTVWXYZ4", 0);
        assert_eq!(a, nth_id("rbv_", "01JABCDEFGHJKMNPQRSTVWXYZ4", 0));
        assert_ne!(a, nth_id("rbv_", "01JABCDEFGHJKMNPQRSTVWXYZ4", 1));
        assert!(
            a.parse::<runbook_model::VersionId>().is_ok(),
            "{a} parses as a version ID"
        );
        assert!(
            nth_id("rb_", "x", 7)
                .parse::<runbook_model::RunbookId>()
                .is_ok()
        );
    }
}
