//! Review fixes (group 1) on the portable archive. F95: the archive and
//! every stored run head say which engine semantics wrote them, so a later release can tell old runs from
//! new instead of replaying them with new rules.
use runbook_core::{
    directory::memory::MemoryDirectory,
    memory::MemoryExecutionStore,
    portable::{self, Archive, ImportOptions, PortableError},
    webhooks::memory::MemoryWebhooks,
};
use runbook_engine::{ENGINE_VERSION, LEGACY_ENGINE_VERSION};
use runbook_store_conformance::portable::{self as fixture, p, q};
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
const T0: i64 = 1_700_000_000_000;

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
fn archive() -> Archive {
    let m = empty();
    ready(fixture::build(
        &m.store,
        &m.store.scoped(q()),
        &m.directory,
        &m.webhooks,
    ));
    let data = portable::memory::export(&m.store, &m.directory, &m.webhooks).unwrap();
    portable::encode(&data, 1_800_000_000_000).unwrap()
}
fn with_manifest(archive: &Archive, edit: impl Fn(&mut serde_json::Value)) -> Archive {
    let mut archive = archive.clone();
    let mut value: serde_json::Value =
        serde_json::from_slice(&archive.files[portable::MANIFEST]).unwrap();
    edit(&mut value);
    archive.files.insert(
        portable::MANIFEST.into(),
        serde_json::to_vec_pretty(&value).unwrap(),
    );
    archive
}
fn import(archive: &Archive) -> Result<(), PortableError> {
    let target = empty();
    let (_, data) = portable::decode(archive)?;
    portable::memory::import(
        &target.store,
        &target.directory,
        &target.webhooks,
        &data,
        ImportOptions::default(),
    )
}

#[test]
fn archive_manifest_and_run_heads_carry_the_engine_version() {
    let good = archive();
    let (manifest, data) = portable::decode(&good).unwrap();
    assert_eq!(manifest.engine_version, ENGINE_VERSION);
    assert!(!data.rows.runs.is_empty());
    for unit in &data.rows.runs {
        assert_eq!(unit.slice.run.projection.engine_version, ENGINE_VERSION);
    }
    import(&good).unwrap();
}

#[test]
fn archive_from_a_newer_engine_is_refused_and_an_unmarked_one_is_legacy() {
    let good = archive();
    let newer = with_manifest(&good, |m| m["engine_version"] = (ENGINE_VERSION + 1).into());
    assert!(matches!(
        import(&newer),
        Err(PortableError::Archive {
            code: "archive.unsupported_engine_version",
            ..
        })
    ));
    let unmarked = with_manifest(&good, |m| {
        m.as_object_mut().unwrap().remove("engine_version");
    });
    let (manifest, _) = portable::decode(&unmarked).unwrap();
    assert_eq!(manifest.engine_version, LEGACY_ENGINE_VERSION);
}

#[test]
fn a_stored_head_without_the_marker_reads_as_legacy() {
    let good = archive();
    let (_, data) = portable::decode(&good).unwrap();
    let mut head = serde_json::to_value(&data.rows.runs[0].slice.run.projection).unwrap();
    assert_eq!(head["engine_version"], ENGINE_VERSION);
    head.as_object_mut().unwrap().remove("engine_version");
    let head: runbook_core::ExecutionProjection = serde_json::from_value(head).unwrap();
    assert_eq!(head.engine_version, LEGACY_ENGINE_VERSION);
}

// F19: a shared blob's bytes must hash to its key, or a crafted archive
// could serve skill files under a SHA they do not match.
#[test]
fn shared_blob_bytes_must_match_their_key() {
    let (_, mut data) = portable::decode(&archive()).unwrap();
    portable::validate(&data).unwrap();
    let blob = data
        .rows
        .blobs
        .iter_mut()
        .find(|b| matches!(b.key, runbook_core::blob::BlobKey::Shared { .. }))
        .expect("the fixture has a shared blob");
    blob.bytes = b"not the content this key names".to_vec();
    assert!(matches!(
        portable::validate(&data),
        Err(PortableError::Archive {
            code: "archive.blob_hash_mismatch",
            ..
        })
    ));
}

// F23: an archive runbook whose draft revision is 0 is refused at
// validation; draft revisions start at 1 and the draft view cannot hold 0.
#[test]
fn a_zero_draft_revision_is_refused() {
    let (_, mut data) = portable::decode(&archive()).unwrap();
    data.rows.runbooks[0].draft_revision = 0;
    assert!(matches!(
        portable::validate(&data),
        Err(PortableError::Archive {
            code: "archive.invalid_draft",
            ..
        })
    ));
}
