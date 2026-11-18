//! CF11: runbook source out of D1. With a source blob store, drafts, keyed
//! drafts and versions keep only a digest and size in D1, the text lives in
//! the blobs, and every read stitches it back; rows written inline still
//! read; a missing or altered blob is an error, never a wrong source.
use runbook_core::{
    CommitResult, StoreError,
    catalog::{RunbookOrigin, RunbookRecord},
    publish::prepare,
};
use runbook_model::{RunbookId, VersionId};
use runbook_store_conformance::portable::p;
use runbook_store_d1::{
    D1, D1Catalog,
    native::{NativeD1, run},
    prepare as sql,
    sources::{MemorySources, SourceBlobs},
    sql::Columns,
};
use std::rc::Rc;

fn rb(n: u8) -> RunbookId {
    format!("rb_{n:026}").parse().unwrap()
}
fn rbv(n: u8) -> VersionId {
    format!("rbv_{n:026}").parse().unwrap()
}

/// A valid runbook of about `kb` KB.
fn source(slug: &str, kb: usize, edit: u32) -> String {
    let mut text = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {slug}, title: Edit {edit}}}\nspec:\n  steps:\n"
    );
    let mut n = 0;
    while text.len() < kb * 1024 {
        n += 1;
        text.push_str(&format!(
            "    - id: s{n}\n      title: Stage {n}\n      instructions: Check the dashboards and write down what you saw at stage {n}.\n"
        ));
    }
    text
}

fn setup() -> (NativeD1, D1Catalog<NativeD1>, MemorySources) {
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    let blobs = MemorySources::default();
    let catalog = D1Catalog::new(db.clone(), p()).with_sources(Rc::new(blobs.clone()));
    (db, catalog, blobs)
}

fn text_bytes(db: &NativeD1, sql_text: &'static str) -> (usize, usize) {
    let rows = run(db.all(sql(sql_text))).unwrap();
    let row = rows.first().unwrap();
    (
        row.unsigned(0).unwrap() as usize,
        row.unsigned(1).unwrap() as usize,
    )
}

#[test]
fn drafts_and_versions_keep_only_a_digest_in_d1_and_read_back_whole() {
    let (db, catalog, blobs) = setup();
    let id = rb(1);
    let first = source("ops", 20, 0);
    run(catalog.create_runbook(
        RunbookRecord {
            runbook_id: id.clone(),
            slug: "ops".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        first.clone(),
    ))
    .unwrap();
    assert_eq!(run(catalog.get_draft(&id)).unwrap().unwrap().source, first);

    // Publish what was reviewed: the guard compares digests.
    let prepared = prepare("ops", &first, &Default::default(), rbv(1)).unwrap();
    run(catalog.publish_revision(&id, 1, prepared)).unwrap();
    let version = run(catalog.version_record(&id, &rbv(1))).unwrap().unwrap();
    assert_eq!(version.source, first);

    let second = source("ops", 30, 1);
    assert_eq!(
        run(catalog.save_draft(&id, 1, second.clone())).unwrap(),
        CommitResult::Committed(2)
    );
    assert_eq!(run(catalog.get_draft(&id)).unwrap().unwrap().source, second);
    // A publication of another source than the draft is a conflict.
    let stale = prepare("ops", &first, &Default::default(), rbv(2)).unwrap();
    assert!(matches!(
        run(catalog.publish_revision(&id, 2, stale)),
        Err(StoreError::InvalidBatch("catalog.draft_conflict"))
    ));

    // Keyed drafts too.
    run(catalog.replace_draft("git:main", 0, second.clone())).unwrap();
    assert_eq!(
        run(catalog.read_draft("git:main")).unwrap().unwrap().source,
        second
    );

    // D1 holds no source text, only the digests and sizes.
    for table_sql in [
        "SELECT length(draft_source), draft_size FROM runbook",
        "SELECT length(source), source_size FROM runbook_version",
        "SELECT length(source), source_size FROM draft",
    ] {
        let (inline, size) = text_bytes(&db, table_sql);
        assert_eq!(inline, 0, "{table_sql}");
        assert!(size >= 20 * 1024, "{table_sql}");
    }
    // The blobs are content-addressed: the version and its draft share one.
    assert_eq!(blobs.0.borrow().len(), 2);
}

#[test]
fn inline_rows_still_read_and_a_bad_blob_is_an_error() {
    let (db, catalog, blobs) = setup();
    let id = rb(2);
    let text = source("old", 4, 0);
    // A row from before 1010: inline text, no digest.
    let inline = D1Catalog::new(db.clone(), p());
    run(inline.create_runbook(
        RunbookRecord {
            runbook_id: id.clone(),
            slug: "old".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        text.clone(),
    ))
    .unwrap();
    assert_eq!(run(catalog.get_draft(&id)).unwrap().unwrap().source, text);
    // Saved through the blob store, it moves out of D1.
    run(catalog.save_draft(&id, 1, text.clone())).unwrap();
    assert_eq!(
        text_bytes(&db, "SELECT length(draft_source), draft_size FROM runbook").0,
        0
    );
    assert_eq!(run(catalog.get_draft(&id)).unwrap().unwrap().source, text);
    // Without the blob store an external row cannot be read.
    assert!(matches!(
        run(inline.get_draft(&id)),
        Err(StoreError::Backend("d1.source_unavailable"))
    ));
    // An altered blob is refused, a missing one too.
    let digest = *blobs.0.borrow().keys().next().unwrap();
    run(blobs.put(digest, b"tampered".to_vec())).unwrap();
    assert!(matches!(
        run(catalog.get_draft(&id)),
        Err(StoreError::Backend("d1.source_corrupt"))
    ));
    blobs.0.borrow_mut().clear();
    assert!(matches!(
        run(catalog.get_draft(&id)),
        Err(StoreError::Backend("d1.source_missing"))
    ));
}
