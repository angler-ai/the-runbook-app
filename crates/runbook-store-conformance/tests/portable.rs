//! D3 portable project archive on the memory adapters: deterministic round
//! trip, no secrets, tenant scope, integrity and all-or-nothing import, and
//! a mid-flight run that resumes after import.
use runbook_core::{
    ExecutionStore,
    actor::begin_actor,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord},
    credentials::{
        CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind,
        memory::MemoryCredentials, secret_hash,
    },
    decisions::vote,
    directory::{DirectoryStore, memory::MemoryDirectory},
    drain::{Processed, process_lease},
    execution_credentials::{MintEntropy, mint_execution_pair},
    memory::MemoryExecutionStore,
    portable::{self, Archive, ImportOptions, PortableError, ProjectData},
    webhooks::{Subscription, WebhookStore, memory::MemoryWebhooks},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{DecisionRequest, ExecutionStatus, ProjectRole};
use runbook_store_conformance::portable::{
    self as fixture, SEALED_PREVIOUS, SEALED_SECRET, SEALED_SECRET_Q, exe, human, p, q, rb, user,
    webhook,
};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory adapters must be synchronous"),
    }
}
/// Fixture store time (November 2023): in the past for any real clock.
const T0: i64 = 1_700_000_000_000;
const CREATED: i64 = 1_800_000_000_000;

struct Memory {
    store: MemoryExecutionStore,
    directory: MemoryDirectory,
    webhooks: MemoryWebhooks,
}
fn empty() -> Memory {
    let directory = MemoryDirectory::default();
    directory.set_time_ms(T0);
    Memory {
        store: MemoryExecutionStore::empty(p(), T0),
        directory,
        webhooks: MemoryWebhooks::new(T0),
    }
}
fn fixture() -> Memory {
    let m = empty();
    ready(fixture::build(
        &m.store,
        &m.store.scoped(q()),
        &m.directory,
        &m.webhooks,
    ));
    m
}
fn data(m: &Memory) -> ProjectData {
    portable::memory::export(&m.store, &m.directory, &m.webhooks).unwrap()
}
fn archive(m: &Memory) -> Archive {
    portable::encode(&data(m), CREATED).unwrap()
}
fn import(m: &Memory, archive: &Archive) -> Result<(), PortableError> {
    import_with(m, archive, ImportOptions::default())
}
const INTO_EXISTING: ImportOptions = ImportOptions {
    into_existing_org: true,
};
fn import_with(m: &Memory, archive: &Archive, options: ImportOptions) -> Result<(), PortableError> {
    let (_, data) = portable::decode(archive)?;
    portable::memory::import(&m.store, &m.directory, &m.webhooks, &data, options)
}
fn bytes(archive: &Archive) -> Vec<u8> {
    archive
        .files
        .iter()
        .flat_map(|(path, bytes)| path.bytes().chain(bytes.iter().copied()))
        .collect()
}
fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn memory_round_trip_is_byte_identical() {
    let source = fixture();
    let first = archive(&source);
    let target = empty();
    import(&target, &first).unwrap();
    assert_eq!(archive(&target), first);
    // Deterministic for equal content and time.
    assert_eq!(archive(&source), first);

    let (manifest, data) = portable::decode(&first).unwrap();
    assert_eq!(manifest.created_at_ms, CREATED);
    let runs: Vec<_> = data
        .rows
        .runs
        .iter()
        .map(|r| r.slice.run.projection.execution_id.clone())
        .collect();
    assert_eq!(runs, [exe(1), exe(2), exe(4), exe(6)]);
    let claimed = &data.rows.runs[3];
    assert_eq!(claimed.claim_logs[0].lines, ["checking disk", "disk ok"]);
    assert_eq!(claimed.slice.run.claims.len(), 1);
    assert!(data.rows.runs.iter().all(|r| r.start.is_some()));
    assert_eq!(data.rows.runs[0].start_replays.len(), 1);
    assert_eq!(data.rows.tombstones.len(), 1);
    assert_eq!(data.rows.pools[0].queue, [exe(2)]);
    assert_eq!(data.rows.skills.len(), 1);
    assert_eq!(data.rows.drafts.len(), 1);
    assert_eq!(data.rows.runbooks[0].draft_revision, 2);
    assert!(data.variables.is_some());
    // Project members plus the organization owner; never Q's member.
    let users: Vec<_> = data.directory.users.iter().map(|u| u.id.clone()).collect();
    assert_eq!(users, [user(1), user(3), user(4)]);
    assert_eq!(data.directory.team_members.len(), 1);
    // Imported IDs are preserved and the imported run reads back.
    assert_eq!(
        ready(target.store.load(&exe(1))).unwrap(),
        ready(source.store.load(&exe(1))).unwrap()
    );
    // D42: schema version 2 carries P's memory with every revision, never Q's.
    assert_eq!(manifest.schema_version, 2);
    let memory: Vec<_> = data
        .rows
        .memory
        .iter()
        .map(|row| {
            (
                row.current().unwrap().id.clone(),
                row.revisions.len(),
                row.proposal_sequence.is_some(),
            )
        })
        .collect();
    assert_eq!(
        memory,
        [
            (fixture::memory_id(1), 2, false),
            (fixture::memory_id(2), 1, true)
        ]
    );
    assert!(!contains(&bytes(&first), "project Q memory body"));
    let manifest_paths: Vec<_> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(&manifest_paths[..portable::TABLES.len()], portable::TABLES);
    assert_eq!(manifest.files[17].records, Some(2));
    assert_eq!(manifest.files[18].records, Some(3));
    use runbook_core::memory_entries::MemoryStore;
    for id in [fixture::memory_id(1), fixture::memory_id(2)] {
        assert_eq!(
            ready(target.store.memory_revisions(&p(), &id)).unwrap(),
            ready(source.store.memory_revisions(&p(), &id)).unwrap()
        );
    }
    // The imported proposal keeps its D40 sequence: replaying it is a no-op.
    let proposed = ready(target.store.get_memory(&p(), &fixture::memory_id(2)))
        .unwrap()
        .unwrap();
    let sequence = data.rows.memory[1].proposal_sequence.unwrap();
    let mut replay = proposed.clone();
    replay.id = fixture::memory_id(9);
    assert!(
        !ready(target.store.apply_memory_proposal(
            &p(),
            runbook_core::memory_entries::MemoryProposal {
                execution_id: exe(6),
                sequence,
                entry: replay,
            },
            500,
        ))
        .unwrap()
    );
}

/// The checked-in archive written by the schema version 1 encoder (before
/// D42): it still decodes and imports, and re-exports as version 2 with
/// empty memory tables.
fn v1_fixture() -> Archive {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/archive-v1");
    let mut archive = Archive::default();
    for dir in [root.clone(), root.join("blobs")] {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                let name = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                archive.files.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    archive
}

#[test]
fn version_1_archives_still_import() {
    let v1 = v1_fixture();
    let (manifest, data) = portable::decode(&v1).unwrap();
    assert_eq!(manifest.schema_version, portable::SCHEMA_VERSION_V1);
    assert!(data.rows.memory.is_empty());
    assert_eq!(data.rows.runs.len(), 4);
    let target = empty();
    import(&target, &v1).unwrap();
    let v2 = archive(&target);
    let (manifest, again) = portable::decode(&v2).unwrap();
    assert_eq!(manifest.schema_version, portable::SCHEMA_VERSION);
    assert_eq!(again, data);
    // v2 adds exactly the two memory tables, empty, to the v1 file set.
    let mut added: Vec<_> = v2
        .files
        .keys()
        .filter(|k| !v1.files.contains_key(*k))
        .collect();
    added.sort();
    assert_eq!(added, ["memory_entries.ndjson", "memory_revisions.ndjson"]);
    assert!(v2.files["memory_entries.ndjson"].is_empty());
    for (path, bytes) in &v1.files {
        if path != portable::MANIFEST {
            assert_eq!(&v2.files[path], bytes, "{path}");
        }
    }
    // A target that already holds memory is not empty.
    let busy = empty();
    ready(runbook_core::memory_entries::create(
        &busy.store,
        &p(),
        fixture::memory_id(7),
        runbook_model::MemoryScope::Project,
        &runbook_model::SaveMemoryRequest {
            title: "Existing".into(),
            body_markdown: "existing".to_owned().try_into().unwrap(),
            tags: vec![],
            step_id: None,
            expires_at: None,
            propose: false,
        },
        None,
        runbook_model::MemorySource::Person { user_id: user(1) },
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    ))
    .unwrap();
    assert!(matches!(
        import(&busy, &v1),
        Err(PortableError::TargetNotEmpty)
    ));
    // A version 1 manifest that lists the memory tables is not version 1.
    let mut mislabeled = v2.clone();
    let mut m: serde_json::Value =
        serde_json::from_slice(&mislabeled.files["manifest.json"]).unwrap();
    m["schema_version"] = 1.into();
    mislabeled.files.insert(
        "manifest.json".into(),
        serde_json::to_vec_pretty(&m).unwrap(),
    );
    assert!(portable::decode(&mislabeled).is_err());
    m["schema_version"] = 3.into();
    mislabeled.files.insert(
        "manifest.json".into(),
        serde_json::to_vec_pretty(&m).unwrap(),
    );
    assert_eq!(
        code(rejected(&mislabeled)),
        "archive.unsupported_schema_version"
    );
}

/// Crafted memory rows are refused before any store is touched: another
/// tenant's scope, a revision gap, a current line that is not the last
/// revision, an orphan revision, revisions out of canonical order, and a
/// runbook entry naming a runbook the archive does not hold.
#[test]
fn crafted_memory_rows_are_rejected() {
    let first = archive(&fixture());
    let (_, data) = portable::decode(&first).unwrap();
    let reject = |data: &portable::ProjectData| portable::validate(data).map_err(code).unwrap_err();
    let mut foreign = data.clone();
    for revision in &mut foreign.rows.memory[0].revisions {
        revision.entry.scope = q();
    }
    assert_eq!(reject(&foreign), "archive.scope_mismatch");
    let mut gap = data.clone();
    gap.rows.memory[0].revisions.remove(0);
    assert_eq!(reject(&gap), "archive.invalid_memory");
    let mut renumbered = data.clone();
    renumbered.rows.memory[0].revisions[1].entry.revision = 3;
    assert_eq!(reject(&renumbered), "archive.invalid_memory");
    let mut unsourced = data.clone();
    unsourced.rows.memory[1].proposal_sequence = None;
    assert_eq!(reject(&unsourced), "archive.invalid_memory");
    let mut dangling = data.clone();
    for revision in &mut dangling.rows.memory[0].revisions {
        revision.entry.memory_scope = runbook_model::MemoryScope::Runbook { runbook_id: rb(3) };
    }
    assert_eq!(reject(&dangling), "archive.dangling_reference");
    let mut duplicate = data.clone();
    duplicate.rows.memory.push(duplicate.rows.memory[0].clone());
    portable::normalize(&mut duplicate);
    assert_eq!(reject(&duplicate), "archive.duplicate_row");
    // File-level: the entry line must be the last revision; revisions sorted.
    let edit = |file: &str, f: &dyn Fn(Vec<serde_json::Value>) -> Vec<serde_json::Value>| {
        let mut a = first.clone();
        let lines: Vec<serde_json::Value> = a.files[file]
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        let mut out = Vec::new();
        for line in f(lines) {
            out.extend(serde_json::to_vec(&line).unwrap());
            out.push(b'\n');
        }
        a.files.insert(file.into(), out);
        let mut manifest: portable::Manifest =
            serde_json::from_slice(&a.files["manifest.json"]).unwrap();
        for entry in &mut manifest.files {
            if entry.path == file {
                entry.bytes = a.files[file].len() as u64;
                entry.sha256 = portable::sha256_hex(&a.files[file]);
            }
        }
        a.files.insert(
            "manifest.json".into(),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        );
        rejected(&a)
    };
    assert_eq!(
        code(edit("memory_revisions.ndjson", &|mut l| {
            l.swap(0, 1);
            l
        })),
        "archive.not_canonical"
    );
    // CF12: blobs are checked in place (their bytes are not copied), by key.
    assert_eq!(
        code(edit("blobs.ndjson", &|mut l| {
            l.swap(0, 1);
            l
        })),
        "archive.not_canonical"
    );
    assert_eq!(
        code(edit("memory_entries.ndjson", &|mut l| {
            l[0]["entry"]["title"] = "Tampered".into();
            l
        })),
        "archive.invalid_memory"
    );
    assert_eq!(
        code(edit("memory_entries.ndjson", &|mut l| {
            l.truncate(1);
            l
        })),
        "archive.record_count_mismatch"
    );
}

#[test]
fn archive_never_carries_secrets_or_hashes() {
    let source = fixture();
    let credentials = MemoryCredentials::default();
    let issuer = CredentialRecord {
        id: "tok_00000000000000000000000200".parse().unwrap(),
        kind: TokenKind::Service,
        hash: secret_hash("trb_svc_launcher"),
        principal: CredentialPrincipal::Service {
            project_id: p().project_id,
            role: ProjectRole::Operator,
        },
        org_id: p().org_id,
        scopes: [Scope::ExecutionsAct].into(),
        projects: None,
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
    };
    ready(credentials.insert(issuer.clone())).unwrap();
    let pair = ready(mint_execution_pair(
        &source.store,
        &credentials,
        &source.directory,
        &issuer,
        &exe(1),
        &runbook_model::MintTokenRequest {
            session_id: "s1".into(),
        },
        "mint-1",
        [9; 32],
        MintEntropy {
            family_id: "fam-1".into(),
            access_token_id: "tok_00000000000000000000000002".parse().unwrap(),
            refresh_token_id: "tok_00000000000000000000000003".parse().unwrap(),
            access_random: [1; 32],
            refresh_random: [51; 32],
        },
    ))
    .unwrap();
    assert!(pair.access.secret.starts_with("trb_"));
    let all = bytes(&archive(&source));
    for secret in [
        "trb_",
        "whsec_",
        SEALED_SECRET,
        SEALED_PREVIOUS,
        SEALED_SECRET_Q,
        pair.access.secret.as_str(),
        pair.refresh.secret.as_str(),
    ] {
        assert!(!contains(&all, secret), "archive contains {secret}");
    }
    // Token metadata travels only as the run's audit events (family ID,
    // generation, issuer); there are no credential rows to import.
    assert!(contains(&all, "ExecutionCredentialsIssued"));
    for hash in [
        secret_hash(&pair.access.secret),
        secret_hash(&pair.refresh.secret),
        secret_hash("trb_svc_launcher"),
    ] {
        let raw = hash.as_bytes();
        assert!(!contains(&all, &runbook_core::authoring::hex(raw)));
        assert!(!contains(&all, &serde_json::to_string(raw).unwrap()));
    }
    // An imported subscription keeps its settings but no secret.
    let target = empty();
    import(&target, &archive(&source)).unwrap();
    let imported: Subscription = ready(target.webhooks.get_subscription(&p(), &webhook(1)))
        .unwrap()
        .unwrap();
    assert!(imported.enabled);
    assert_eq!(imported.secret.0, portable::UNSET_SECRET);
    assert!(imported.previous_secret.is_none());
    // Rotating mints the first usable secret; the unset one never becomes
    // a "previous" secret that deliveries would try to open.
    let mut rotated = imported.clone();
    rotated.rotate(runbook_core::webhooks::SealedSecret("v1.fresh".into()), T0);
    assert_eq!(rotated.secret.0, "v1.fresh");
    assert!(rotated.previous_secret.is_none());
}

#[test]
fn export_is_tenant_scoped() {
    let source = fixture();
    let p_bytes = bytes(&archive(&source));
    for foreign in [
        q().project_id.to_string(),
        exe(5).to_string(),
        rb(3).to_string(),
        webhook(2).to_string(),
        "foreign project log".into(),
        SEALED_SECRET_Q.into(),
        "\"bob\"".into(),
    ] {
        assert!(
            !contains(&p_bytes, &foreign),
            "P archive contains {foreign}"
        );
    }
    // Q's member is never exported as a user or membership row. (P's own
    // run history may still name him: the approval notified team oncall.)
    let p_archive = archive(&source);
    // Only teams P's versions and runs reference: oncall, not platform,
    // although P's member alice belongs to both.
    let (_, p_data) = portable::decode(&p_archive).unwrap();
    let teams: Vec<_> = p_data
        .directory
        .teams
        .iter()
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(teams, [fixture::team()]);
    assert!(
        p_data
            .directory
            .team_members
            .iter()
            .all(|m| m.team_id == fixture::team())
    );
    for table in [
        "users.ndjson",
        "project_members.ndjson",
        "team_members.ndjson",
    ] {
        assert!(!contains(&p_archive.files[table], &user(2).to_string()));
    }
    let q_store = source.store.scoped(q());
    let q_archive = portable::encode(
        &portable::memory::export(&q_store, &source.directory, &source.webhooks).unwrap(),
        CREATED,
    )
    .unwrap();
    let q_bytes = bytes(&q_archive);
    for foreign in [
        p().project_id.to_string(),
        exe(1).to_string(),
        rb(1).to_string(),
        webhook(1).to_string(),
        "step started".into(),
        SEALED_SECRET.into(),
    ] {
        assert!(
            !contains(&q_bytes, &foreign),
            "Q archive contains {foreign}"
        );
    }
    for table in [
        "users.ndjson",
        "project_members.ndjson",
        "team_members.ndjson",
    ] {
        for member in [user(1), user(4)] {
            assert!(!contains(&q_archive.files[table], &member.to_string()));
        }
    }
}

/// The organization and both projects, with no members or variables.
fn bare_directory(directory: &MemoryDirectory) {
    use runbook_core::directory::{DirectoryCommit, DirectoryMutation, ProjectRecord};
    let mut mutations = vec![DirectoryMutation::CreateOrganization {
        title: "Org".into(),
    }];
    for (scope, slug, title) in [(p(), "api", "API"), (q(), "web", "Web")] {
        mutations.push(DirectoryMutation::CreateProject(ProjectRecord {
            id: scope.project_id,
            org_id: scope.org_id,
            slug: slug.into(),
            title: title.into(),
        }));
    }
    ready(directory.commit(DirectoryCommit {
        org_id: p().org_id,
        expected_revision: 0,
        mutations,
    }))
    .unwrap();
}
fn rejected(archive: &Archive) -> PortableError {
    let target = empty();
    let error = import(&target, archive).unwrap_err();
    // Nothing reached the target.
    assert!(ready(target.directory.snapshot(&p().org_id)).is_err());
    assert!(ready(target.store.read_runbook(&rb(1))).unwrap().is_none());
    error
}
fn code(error: PortableError) -> &'static str {
    match error {
        PortableError::Archive { code, .. } => code,
        other => panic!("not an archive error: {other:?}"),
    }
}

#[test]
fn tampered_or_truncated_archives_are_rejected() {
    let good = archive(&fixture());
    let mut tampered = good.clone();
    let file = tampered.files.get_mut("runbooks.ndjson").unwrap();
    let at = file.len() / 2;
    file[at] ^= 1;
    assert_eq!(code(rejected(&tampered)), "archive.digest_mismatch");

    let mut truncated = good.clone();
    let file = truncated.files.get_mut("execution_events.ndjson").unwrap();
    file.truncate(file.len() - 10);
    assert_eq!(code(rejected(&truncated)), "archive.size_mismatch");

    let mut missing = good.clone();
    let blob = missing
        .files
        .keys()
        .find(|k| k.starts_with(portable::BLOB_DIR))
        .unwrap()
        .clone();
    missing.files.remove(&blob);
    assert_eq!(code(rejected(&missing)), "archive.missing_file");

    let mut extra = good.clone();
    extra
        .files
        .insert("credentials.ndjson".into(), b"{}\n".to_vec());
    assert_eq!(code(rejected(&extra)), "archive.unexpected_file");

    let manifest = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut archive = good.clone();
        let mut value: serde_json::Value =
            serde_json::from_slice(&archive.files[portable::MANIFEST]).unwrap();
        edit(&mut value);
        archive.files.insert(
            portable::MANIFEST.into(),
            serde_json::to_vec_pretty(&value).unwrap(),
        );
        archive
    };
    assert_eq!(
        code(rejected(&manifest(&|m| m["schema_version"] = 99.into()))),
        "archive.unsupported_schema_version"
    );
    assert_eq!(
        code(rejected(
            &manifest(&|m| m["files"][7]["records"] = 7.into())
        )),
        "archive.record_count_mismatch"
    );
    // A whole file replaced with consistent manifest digests still has to
    // be internally consistent: events without their run are rejected.
    let mut orphan = good.clone();
    orphan.files.insert("executions.ndjson".into(), vec![]);
    let orphan = {
        let mut value: serde_json::Value =
            serde_json::from_slice(&orphan.files[portable::MANIFEST]).unwrap();
        value["files"][12]["records"] = 0.into();
        value["files"][12]["bytes"] = 0.into();
        value["files"][12]["sha256"] = portable::sha256_hex(b"").into();
        orphan.files.insert(
            portable::MANIFEST.into(),
            serde_json::to_vec_pretty(&value).unwrap(),
        );
        orphan
    };
    assert_eq!(code(rejected(&orphan)), "archive.orphan_events");
    // Internally inconsistent content with valid digests: a queued run
    // missing from its pool's queue.
    let (_, mut data) = portable::decode(&good).unwrap();
    data.rows.pools[0].queue.clear();
    let inconsistent = portable::encode(&data, CREATED).unwrap();
    assert_eq!(code(rejected(&inconsistent)), "archive.dangling_reference");
}

#[test]
fn import_refuses_non_empty_targets_and_leaves_no_partial_state() {
    let good = archive(&fixture());
    // A target project that already holds a runbook is refused untouched.
    let busy = empty();
    bare_directory(&busy.directory);
    ready(busy.store.create_runbook(
        RunbookRecord {
            runbook_id: "rb_00000000000000000000000099".parse().unwrap(),
            slug: "other".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        "draft".into(),
    ))
    .unwrap();
    let before = ready(busy.directory.snapshot(&p().org_id)).unwrap();
    // The organization exists: refused unless the operator opts in.
    assert_eq!(
        import(&busy, &good),
        Err(PortableError::Conflict("import.org_exists"))
    );
    assert_eq!(
        import_with(&busy, &good, INTO_EXISTING),
        Err(PortableError::TargetNotEmpty)
    );
    assert_eq!(ready(busy.directory.snapshot(&p().org_id)).unwrap(), before);
    assert!(ready(busy.store.read_runbook(&rb(1))).unwrap().is_none());
    // Project variables or a webhook subscription also make it non-empty.
    let configured = empty();
    ready(fixture::directory(&configured.directory));
    assert_eq!(
        import_with(&configured, &good, INTO_EXISTING),
        Err(PortableError::TargetNotEmpty)
    );
    let hooked = empty();
    let now = ready(hooked.webhooks.webhook_time_ms()).unwrap();
    let mut hook = ready(fixture().webhooks.get_subscription(&p(), &webhook(1)))
        .unwrap()
        .unwrap();
    hook.id = webhook(9);
    hook.created_at_ms = now;
    ready(hooked.webhooks.create_subscription(&p(), hook, 20, None)).unwrap();
    assert_eq!(import(&hooked, &good), Err(PortableError::TargetNotEmpty));
    assert!(ready(hooked.directory.snapshot(&p().org_id)).is_err());

    // The last adapter to stage fails (a subscription ID taken by another
    // project): the directory and execution store stages are discarded.
    let clash = empty();
    let mut foreign = fixture::q();
    foreign.project_id = "prj_00000000000000000000000077".parse().unwrap();
    let now = ready(clash.webhooks.webhook_time_ms()).unwrap();
    let mut taken = ready(fixture().webhooks.get_subscription(&p(), &webhook(1)))
        .unwrap()
        .unwrap();
    taken.scope = foreign.clone();
    taken.created_at_ms = now;
    ready(
        clash
            .webhooks
            .create_subscription(&foreign, taken, 20, None),
    )
    .unwrap();
    assert_eq!(
        import(&clash, &good),
        Err(PortableError::Conflict("import.id_conflict"))
    );
    assert!(ready(clash.directory.snapshot(&p().org_id)).is_err());
    assert!(ready(clash.store.read_runbook(&rb(1))).unwrap().is_none());
    assert!(ready(clash.store.load(&exe(1))).is_err());
}

#[test]
fn mid_flight_run_resumes_after_import() {
    let source = fixture();
    let target = empty();
    import(&target, &archive(&source)).unwrap();
    let store = &target.store;
    // Only a human approval is open: the run reports waiting (R50).
    assert_eq!(
        ready(fixture::status(store, 1)).unwrap(),
        ExecutionStatus::Waiting
    );
    // The decision works on the imported approval.
    let work = ready(begin_actor(store, &target.directory, p(), &exe(1))).unwrap();
    let gate = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "gate")
        .and_then(|s| s.id.clone())
        .unwrap();
    ready(vote(
        work,
        &human(1),
        &gate,
        &DecisionRequest {
            option: "keep".into(),
            comment: None,
            session_id: None,
            actor: None,
        },
        None,
    ))
    .unwrap();
    // The pending deadline timer fires after import.
    store.set_time(T0 + 2 * 3_600_000);
    let leases = ready(store.claim_due(16, 60_000)).unwrap();
    let timer = leases
        .iter()
        .find(|l| l.id.execution_id == exe(1) && matches!(l.id.kind, WorkKind::Timer { .. }))
        .expect("deadline timer is due");
    assert_eq!(
        ready(process_lease(store, &target.directory, p(), timer)).unwrap(),
        Processed::Fired
    );
    assert_eq!(
        ready(fixture::status(store, 1)).unwrap(),
        ExecutionStatus::Failed
    );
    // Its admission released: the queued run is promoted and activates.
    let activation = ready(store.claim_due(16, 60_000))
        .unwrap()
        .into_iter()
        .find(|l| l.id.execution_id == exe(2) && l.id.kind == WorkKind::Activation)
        .expect("queued run promoted");
    assert_eq!(
        ready(process_lease(store, &target.directory, p(), &activation)).unwrap(),
        Processed::Activated
    );
    // It too waits on its human approval (R50).
    assert_eq!(
        ready(fixture::status(store, 2)).unwrap(),
        ExecutionStatus::Waiting
    );
}

fn archive_error(error: PortableError) -> &'static str {
    match error {
        PortableError::Archive { code, .. } => code,
        other => panic!("not an archive error: {other:?}"),
    }
}

type RunEdit = Box<dyn Fn(&mut portable::RunUnit)>;
type DataEdit = Box<dyn Fn(&mut ProjectData)>;

/// CF12c (CF12b's final review, item 2): a start record past one Durable
/// Object or D1 row (`start::MAX_START_ROW_BYTES`) imports natively, as an
/// older native export may hold one (runs started before CF12b had no such
/// bound). Only the Cloudflare edition, whose rows cannot hold it, refuses
/// it before any write (`runbook-worker-conformance`). Before, every
/// edition refused the whole archive with `archive.value_too_large`.
#[test]
fn an_oversized_start_record_imports_natively() {
    let mut crafted = data(&fixture());
    let start = crafted
        .rows
        .runs
        .iter_mut()
        .find_map(|r| r.start.as_mut())
        .unwrap();
    start.context.fields.insert(
        "big".into(),
        serde_json::Value::String("x".repeat(2_000_000)),
    );
    assert!(serde_json::to_vec(&*start).unwrap().len() > runbook_core::start::MAX_START_ROW_BYTES);
    let archive = portable::encode(&crafted, CREATED).unwrap();
    let target = empty();
    import(&target, &archive).unwrap();
    // It exports again unchanged.
    assert_eq!(data(&target), crafted);
}

#[test]
fn embedded_ids_must_name_their_own_run() {
    let good = data(&fixture());
    assert!(!good.rows.runs[0].slice.catalog.assignments.is_empty());
    assert!(!good.rows.runs[0].slice.work.is_empty());
    let victim = exe(5);
    let edits: Vec<(&str, RunEdit)> = vec![
        (
            "summary",
            Box::new(|r| r.slice.catalog.summary.as_mut().unwrap().execution_id = exe(5)),
        ),
        (
            "assignment",
            Box::new(|r| r.slice.catalog.assignments[0].execution_id = exe(5)),
        ),
        (
            "admission request",
            Box::new(|r| r.slice.catalog.admission.as_mut().unwrap().0.execution_id = exe(5)),
        ),
        (
            "admission",
            Box::new(|r| r.slice.catalog.admission.as_mut().unwrap().1.execution_id = exe(5)),
        ),
        (
            "work lease",
            Box::new(|r| r.slice.work[0].lease.id.execution_id = exe(5)),
        ),
        (
            "start replay outcome",
            Box::new(|r| r.start_replays[0].replay.outcome.execution_id = exe(5)),
        ),
        (
            "actor replay",
            Box::new(|r| {
                let mut record = r.slice.run.idempotency.values().next().cloned().unwrap_or(
                    runbook_core::IdempotencyRecord {
                        execution_id: exe(1),
                        key: format!("actor:{}:1700000000000", "0".repeat(64)),
                        request_hash: [0; 32],
                        response: serde_json::json!({"at_ms": 1_700_000_000_000_i64}),
                    },
                );
                record.execution_id = exe(5);
                r.slice.actor_replays.push(record);
            }),
        ),
    ];
    for (name, edit) in edits {
        let mut crafted = good.clone();
        edit(&mut crafted.rows.runs[0]);
        // Through an archive whose digests were recomputed...
        let archive = portable::encode(&crafted, CREATED).unwrap();
        assert_eq!(
            archive_error(rejected(&archive)),
            "archive.foreign_id",
            "{name}"
        );
        // ...and handed straight to import, bypassing decode.
        let target = empty();
        assert!(
            matches!(
                portable::memory::import(
                    &target.store,
                    &target.directory,
                    &target.webhooks,
                    &crafted,
                    ImportOptions::default(),
                ),
                Err(PortableError::Archive {
                    code: "archive.foreign_id",
                    ..
                })
            ),
            "{name}"
        );
        assert!(ready(target.directory.snapshot(&p().org_id)).is_err());
    }
    let _ = victim;
}

#[test]
fn crafted_archives_cannot_touch_a_victim_run() {
    let source = fixture();
    // The target holds only project Q, whose exe_5 is the victim.
    let target = empty();
    let q_store = target.store.scoped(q());
    let q_data = portable::memory::export(
        &source.store.scoped(q()),
        &source.directory,
        &source.webhooks,
    )
    .unwrap();
    portable::memory::import(
        &q_store,
        &target.directory,
        &target.webhooks,
        &q_data,
        ImportOptions::default(),
    )
    .unwrap();
    let victim = || {
        (
            ready(q_store.load(&exe(5))).unwrap(),
            ready(q_store.get_run(&exe(5))).unwrap(),
            ready(q_store.tombstone_project(&p().org_id, &exe(5))).unwrap(),
            ready(target.directory.snapshot(&p().org_id)).unwrap(),
        )
    };
    let before = victim();
    let p_store = target.store.scoped(p());
    let attempt = |data: &ProjectData| {
        let archive = portable::encode(data, CREATED).unwrap();
        let (_, data) = portable::decode(&archive)?;
        portable::memory::import(
            &p_store,
            &target.directory,
            &target.webhooks,
            &data,
            INTO_EXISTING,
        )
    };
    // A tombstone naming the victim would freeze it.
    let mut tombstone = data(&source);
    tombstone
        .rows
        .tombstones
        .push(runbook_core::retention::Tombstone {
            scope: p(),
            execution_id: exe(5),
            runbook_id: rb(3),
            deleted_at_ms: T0,
            reason: "frozen".into(),
        });
    assert_eq!(
        attempt(&tombstone),
        Err(PortableError::Conflict("import.id_conflict"))
    );
    // A summary naming the victim would overwrite its run row.
    let mut summary = data(&source);
    summary.rows.runs[2]
        .slice
        .catalog
        .summary
        .as_mut()
        .unwrap()
        .execution_id = exe(5);
    assert!(matches!(
        attempt(&summary),
        Err(PortableError::Archive {
            code: "archive.foreign_id",
            ..
        })
    ));
    assert_eq!(victim(), before);
    assert!(ready(p_store.read_runbook(&rb(1))).unwrap().is_none());
}

#[test]
fn memory_import_requires_the_store_bound_project() {
    let good = archive(&fixture());
    let target = empty();
    let (_, data) = portable::decode(&good).unwrap();
    assert_eq!(
        portable::memory::import(
            &target.store.scoped(q()),
            &target.directory,
            &target.webhooks,
            &data,
            ImportOptions::default(),
        ),
        Err(PortableError::Conflict("import.scope_mismatch"))
    );
    assert!(ready(target.directory.snapshot(&p().org_id)).is_err());
    assert!(ready(target.store.read_runbook(&rb(1))).unwrap().is_none());
}

#[test]
fn existing_org_is_refused_by_default_and_never_escalated() {
    use runbook_core::directory::{OrgRole, memory::MemoryDirectory};
    let victim = fixture();
    let crafted = fixture::attacker_archive();
    let archive = portable::encode(&crafted, CREATED).unwrap();
    let store = victim.store.scoped(crafted.scope.clone());
    let run = |options| {
        let (_, data) = portable::decode(&archive).unwrap();
        portable::memory::import(&store, &victim.directory, &victim.webhooks, &data, options)
    };
    let before = ready(victim.directory.snapshot(&p().org_id)).unwrap();
    assert_eq!(
        run(ImportOptions::default()),
        Err(PortableError::Conflict("import.org_exists"))
    );
    assert_eq!(
        ready(victim.directory.snapshot(&p().org_id)).unwrap(),
        before
    );

    run(INTO_EXISTING).unwrap();
    let after = ready(victim.directory.snapshot(&p().org_id)).unwrap();
    // Nothing that existed changed: org title, alice's role, team oncall.
    assert_eq!(after.organization, before.organization);
    assert_eq!(after.users[&user(1)], before.users[&user(1)]);
    assert_eq!(
        after.teams[&fixture::team()],
        before.teams[&fixture::team()]
    );
    let members = |s: &runbook_core::directory::DirectorySnapshot, team| {
        s.team_members
            .iter()
            .filter(|(t, _)| t == &team)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        members(&after, fixture::team()),
        members(&before, fixture::team())
    );
    for ((project, user_id), role) in &before.project_members {
        assert_eq!(
            after
                .project_members
                .get(&(project.clone(), user_id.clone())),
            Some(role)
        );
    }
    // The attacker exists only as a Member; the new team and project exist.
    assert_eq!(after.users[&fixture::attacker()].role, OrgRole::Member);
    assert!(after.projects.contains_key(&crafted.scope.project_id));
    assert_eq!(after.teams.len(), before.teams.len() + 1);

    // A user ID known on the instance but outside the target org is
    // rejected, in a new organization as in an existing one.
    let other = MemoryDirectory::default();
    let elsewhere = empty();
    ready(other.commit(runbook_core::directory::DirectoryCommit {
        org_id: "org_00000000000000000000000009".parse().unwrap(),
        expected_revision: 0,
        mutations: vec![
            runbook_core::directory::DirectoryMutation::CreateOrganization {
                title: "Other".into(),
            },
            runbook_core::directory::DirectoryMutation::CreateUser(
                runbook_core::directory::UserRecord {
                    id: fixture::attacker(),
                    name: "mallory".into(),
                    active: true,
                    role: OrgRole::Member,
                },
            ),
        ],
    }))
    .unwrap();
    let (_, data) = portable::decode(&archive).unwrap();
    assert_eq!(
        portable::memory::import(
            &elsewhere.store.scoped(crafted.scope.clone()),
            &other,
            &elsewhere.webhooks,
            &data,
            ImportOptions::default(),
        ),
        Err(PortableError::Conflict("import.user_exists"))
    );
}

#[test]
fn duplicate_rows_and_inconsistent_pools_are_rejected() {
    let good = data(&fixture());
    let cases: Vec<(&str, &str, DataEdit)> = vec![
        (
            "draft",
            "archive.duplicate_row",
            Box::new(|d| d.rows.drafts.push(d.rows.drafts[0].clone())),
        ),
        (
            "blob key",
            "archive.duplicate_row",
            Box::new(|d| {
                let mut copy = d.rows.blobs[0].clone();
                copy.bytes.push(b'!');
                d.rows.blobs.push(copy);
            }),
        ),
        (
            "runbook",
            "archive.duplicate_row",
            Box::new(|d| d.rows.runbooks.push(d.rows.runbooks[0].clone())),
        ),
        (
            "webhook",
            "archive.duplicate_row",
            Box::new(|d| d.webhooks.push(d.webhooks[0].clone())),
        ),
        (
            "start replay key",
            "archive.duplicate_row",
            Box::new(|d| {
                let mut copy = d.rows.runs[0].start_replays[0].clone();
                copy.replay.outcome.execution_id =
                    d.rows.runs[1].slice.run.projection.execution_id.clone();
                d.rows.runs[1].start_replays = vec![copy];
            }),
        ),
        (
            "pool active count",
            "archive.invalid_pool",
            Box::new(|d| d.rows.pools[0].active += 1),
        ),
        (
            "queued admission key",
            "archive.invalid_pool",
            Box::new(|d| {
                d.rows.runs[1]
                    .slice
                    .catalog
                    .admission
                    .as_mut()
                    .unwrap()
                    .0
                    .key = "other".into()
            }),
        ),
    ];
    for (name, expected, edit) in cases {
        let mut crafted = good.clone();
        edit(&mut crafted);
        let archive = portable::encode(&crafted, CREATED).unwrap();
        assert_eq!(archive_error(rejected(&archive)), expected, "{name}");
    }
}

/// MEM6 review: a D1 drop marker (a run's write that lost the race for the
/// last slot) never breaks an archive. Export leaves it out (`normalize`),
/// and an archive written with one still decodes and imports without it.
#[test]
fn drop_markers_are_left_out_of_archives() {
    use runbook_core::memory_entries::{MemoryAction, MemoryRevisionRecord};
    let mut data = data(&fixture());
    let count = data.rows.memory.len();
    let run_row = data
        .rows
        .memory
        .iter()
        .find(|r| r.proposal_sequence.is_some())
        .unwrap()
        .clone();
    let last = run_row.revisions.last().unwrap().clone();
    let mut entry = last.entry.clone();
    entry.id = "mem_00000000000000000000000099".parse().unwrap();
    entry.status = runbook_model::MemoryStatus::Archived;
    entry.dropped = Some("memory.limit_reached".into());
    entry.revision = 1;
    let mut marker = run_row;
    marker.revisions = vec![MemoryRevisionRecord {
        action: MemoryAction::Drop,
        actor: last.actor,
        at_ms: last.at_ms,
        entry,
    }];
    data.rows.memory.push(marker);
    let mut normalized = data.clone();
    portable::normalize(&mut normalized);
    assert_eq!(normalized.rows.memory.len(), count);
    // Written as is (an archive from before this fix), it still imports.
    let written = portable::encode(&data, CREATED).unwrap();
    let (_, decoded) = portable::decode(&written).unwrap();
    assert_eq!(decoded.rows.memory.len(), count);
    assert!(
        decoded
            .rows
            .memory
            .iter()
            .all(|r| !portable::is_drop_marker(r))
    );
    import(&empty(), &written).unwrap();
}

/// MEM6 review fix 2 and re-check fix 1: a pruned history keeps revision
/// 1, the newest revision a person wrote and the newest 50; an archive's
/// row validates only when it holds exactly those and counts the rest as
/// pruned.
#[test]
fn pruned_histories_validate_only_as_pruning_leaves_them() {
    let data = data(&fixture());
    let index = data
        .rows
        .memory
        .iter()
        .position(|r| r.revisions.len() == 2 && r.proposal_sequence.is_none())
        .unwrap();
    // Revisions 1, maybe 2 (a person's), and 13..=62.
    let history = |person: bool, drop: Option<u32>, pruned: u32| {
        let mut data = data.clone();
        let row = &mut data.rows.memory[index];
        let edit = row.revisions[1].clone();
        let mut revisions = vec![row.revisions[0].clone()];
        let numbers = person.then_some(2).into_iter().chain(13..=62);
        for n in numbers.filter(|n| Some(*n) != drop) {
            let mut r = edit.clone();
            r.entry.revision = n;
            revisions.push(r);
        }
        let last = &mut revisions.last_mut().unwrap().entry;
        last.revisions_pruned = pruned;
        last.person_revision = if person { 2 } else { 0 };
        row.revisions = revisions;
        data
    };
    let check = |data: portable::ProjectData| portable::validate(&data).map_err(code);
    assert_eq!(check(history(false, None, 11)), Ok(()));
    assert_eq!(check(history(true, None, 10)), Ok(()));
    for bad in [
        history(false, None, 10),
        history(true, None, 11),
        history(false, Some(30), 12),
        history(true, Some(2), 11),
    ] {
        assert_eq!(check(bad), Err("archive.invalid_memory"));
    }
}
