//! UI-1b: the runbook list's store reads answer alike on every edition.
//! `runbook_listing_page` must equal the reference
//! ([`ListingQuery::page`] over every draft, read one by one) for each
//! search, cut and cursor below, and `latest_published_digests` must give
//! each published runbook's highest number and the SHA-256 of that
//! version's exact source. The fixture covers what the D1 query matches in
//! SQL: non-ASCII case folding, a description match past the 300 listed
//! characters, a comma inside a tag, trimmed and repeated tags, a slug-only
//! match, a draft that does not parse and a draft saved ahead of its
//! version. Review round 1 added a NUL inside a title and a tag (D1's
//! `json_each` and text `instr` stop at one) and a description match past
//! the searched characters (no edition finds it). A store may stop early
//! ([`runbook_core::listing::ListingPage::examined_to`]); [`walk`] follows
//! it as the API's next cursor would, and the walk must equal the
//! reference.
use runbook_core::{
    StoreError,
    authoring::AuthoringStore,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord},
    listing::{DraftListing, ListedRunbook, ListingQuery, SEARCHED_CHARS},
    portable::sha256,
    publish::prepare,
};
use runbook_model::{RunbookId, VersionId};

fn rb(n: u8) -> RunbookId {
    format!("rb_{n:026}").parse().unwrap()
}
fn rbv(n: u8) -> VersionId {
    format!("rbv_{n:026}").parse().unwrap()
}

/// The runbooks: (number, slug, draft source).
pub fn fixture() -> Vec<(u8, &'static str, String)> {
    let long = "x".repeat(320);
    vec![
        (
            1,
            "alpha",
            format!(
                "apiVersion: therunbook.app/v1\nkind: Runbook\n\
                 metadata: {{id: alpha, title: Éclair Deploy, description: '{long} needle', \
                 tags: [' prod ', prod, 'a,b', '']}}\nspec: {{steps: [{{id: s, title: S}}]}}\n"
            ),
        ),
        (
            2,
            "beta",
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: beta, tags: [ops]}\n\
             spec: {steps: [{id: s, title: S}]}\n"
                .into(),
        ),
        (3, "gamma", "[".into()),
        (
            4,
            "delta",
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {id: delta, title: Delta, description: Rolls ÄPI back, tags: [ops, prod]}\n\
             spec: {steps: [{id: s, title: S}]}\n"
                .into(),
        ),
        (
            5,
            "epsilon-check",
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {id: epsilon-check, title: Health}\nspec: {steps: [{id: s, title: S}]}\n"
                .into(),
        ),
        (
            6,
            "zeta",
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {id: zeta, title: \"Nul\\0 tail\", tags: [\"a\\0b\", ops]}\n\
             spec: {steps: [{id: s, title: S}]}\n"
                .into(),
        ),
        (
            7,
            "eta",
            format!(
                "apiVersion: therunbook.app/v1\nkind: Runbook\n\
                 metadata: {{id: eta, title: Eta, description: '{} farneedle'}}\n\
                 spec: {{steps: [{{id: s, title: S}}]}}\n",
                "y".repeat(SEARCHED_CHARS)
            ),
        ),
    ]
}

/// Create the fixture through the catalog; publish delta and epsilon, then
/// save delta ahead of its version. Returns delta's published source.
pub async fn seed<S: AuthoringStore>(store: &S) -> Result<String, StoreError> {
    let catalog = store.catalog();
    for (n, slug, source) in fixture() {
        catalog
            .create_runbook(
                RunbookRecord {
                    runbook_id: rb(n),
                    slug: slug.into(),
                    origin: RunbookOrigin::Ui,
                    archived: false,
                },
                source,
            )
            .await?;
    }
    let sources = fixture();
    for (n, slug, source) in [&sources[3], &sources[4]] {
        let prepared = prepare(slug, source, &Default::default(), rbv(*n))
            .map_err(|_| StoreError::Backend("test.prepare"))?;
        catalog.publish_revision(&rb(*n), 1, prepared).await?;
    }
    let published = sources[3].2.clone();
    catalog
        .save_draft(
            &rb(4),
            1,
            published.replace("title: Delta", "title: Delta v2"),
        )
        .await?;
    Ok(published)
}

/// The reference page: every draft read one by one.
async fn reference<S: AuthoringStore>(
    store: &S,
    query: &ListingQuery,
) -> Result<Vec<ListedRunbook>, StoreError> {
    let mut rows = vec![];
    for record in store.catalog_runbooks().await? {
        let draft = store.catalog().get_draft(&record.runbook_id).await?;
        let listing = DraftListing::of(&record.slug, draft.as_ref().map(|d| d.source.as_str()));
        rows.push((record, listing));
    }
    Ok(query.page(rows))
}

/// A page as the API and its client assemble it: follow
/// [`runbook_core::listing::ListingPage::examined_to`] (the API's next
/// cursor) until the store has `limit` rows or reaches the end.
pub async fn walk<S: AuthoringStore>(
    store: &S,
    query: &ListingQuery,
) -> Result<Vec<ListedRunbook>, StoreError> {
    let mut rows: Vec<ListedRunbook> = vec![];
    let mut after = query.after.clone();
    loop {
        let page = store
            .runbook_listing_page(&ListingQuery {
                after: after.clone(),
                limit: query.limit - rows.len(),
                ..query.clone()
            })
            .await?;
        rows.extend(page.rows);
        match page.examined_to {
            Some(last) if rows.len() < query.limit => {
                assert!(
                    after.as_deref().is_none_or(|a| last.as_str() < a),
                    "examined_to moves forward"
                );
                after = Some(last);
            }
            _ => return Ok(rows),
        }
    }
}

/// The queries compared, as the API normalizes them.
pub fn queries() -> Vec<ListingQuery> {
    let q = |q: &str| ListingQuery {
        q: Some(q.into()),
        limit: 10,
        ..Default::default()
    };
    let tags = |tags: &[&str]| ListingQuery {
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        limit: 10,
        ..Default::default()
    };
    vec![
        ListingQuery {
            limit: 10,
            ..Default::default()
        },
        ListingQuery {
            limit: 2,
            ..Default::default()
        },
        ListingQuery {
            after: Some(rb(4).to_string()),
            limit: 2,
            ..Default::default()
        },
        ListingQuery {
            after: Some(rb(1).to_string()),
            limit: 10,
            ..Default::default()
        },
        q("éclair"),
        q("needle"),
        q("a,b"),
        q("äpi"),
        q("epsilon"),
        q("gam"),
        q("health"),
        q("v2"),
        q("delta"),
        q("x needle"),
        q("nothing"),
        q("tail"),
        q("nul\0 t"),
        q("yyyy"),
        q("farneedle"),
        tags(&["a\0b"]),
        tags(&["a\0c"]),
        tags(&["a"]),
        tags(&["ops"]),
        tags(&["prod"]),
        tags(&["a,b", "prod"]),
        tags(&["ops", "prod"]),
        tags(&["a"]),
        ListingQuery {
            q: Some("e".into()),
            tags: vec!["prod".into()],
            after: Some(rb(5).to_string()),
            limit: 1,
        },
    ]
}

/// The case: run [`seed`] first (once per store).
pub async fn runbook_listing_pages<S: AuthoringStore>(
    store: &S,
    delta_published: &str,
) -> Result<(), StoreError> {
    for query in queries() {
        let page = walk(store, &query).await?;
        let expected = reference(store, &query).await?;
        assert_eq!(page, expected, "{query:?}");
    }
    // The reference's answers, spelled out for the main cases.
    let all = walk(
        store,
        &ListingQuery {
            limit: 10,
            ..Default::default()
        },
    )
    .await?;
    let ids: Vec<RunbookId> = all.iter().map(|r| r.record.runbook_id.clone()).collect();
    assert_eq!(ids, [rb(7), rb(6), rb(5), rb(4), rb(3), rb(2), rb(1)]);
    let all = &all[2..];
    let alpha = &all[4];
    assert_eq!(alpha.title, "Éclair Deploy");
    assert_eq!(alpha.tags, ["prod", "a,b"]);
    assert!(alpha.description_truncated);
    assert_eq!(
        alpha.description.as_deref().map(|d| d.chars().count()),
        Some(300)
    );
    assert_eq!(all[2].title, "gamma", "an unparsable draft lists its slug");
    assert_eq!(all[3].title, "Beta", "LANG7: derived from the ID");
    assert_eq!(all[1].title, "Delta v2");
    for (row, (_, _, source)) in all.iter().rev().zip(fixture()) {
        if row.record.runbook_id != rb(4) {
            assert_eq!(row.draft_digest, Some(sha256(source.as_bytes())));
        }
    }
    // Latest versions: delta's draft is ahead, epsilon's is level.
    let latest = store
        .latest_published_digests(&[rb(1), rb(4), rb(5), rb(9)])
        .await?;
    assert_eq!(latest.len(), 2, "{latest:?}");
    let (number, digest) = latest[&rb(4)];
    assert_eq!(number.get(), 1);
    assert_eq!(digest, sha256(delta_published.as_bytes()));
    assert_ne!(Some(digest), all[1].draft_digest);
    assert_eq!(Some(latest[&rb(5)].1), all[0].draft_digest);
    assert!(store.latest_published_digests(&[]).await?.is_empty());
    // Review round 1: what the NUL and the searched length answer.
    let found = |q: ListingQuery| async move {
        Ok::<_, StoreError>(
            walk(store, &q)
                .await?
                .into_iter()
                .map(|r| r.record.slug)
                .collect::<Vec<_>>(),
        )
    };
    let q = |q: &str| ListingQuery {
        q: Some(q.into()),
        limit: 10,
        ..Default::default()
    };
    let tag = |t: &str| ListingQuery {
        tags: vec![t.into()],
        limit: 10,
        ..Default::default()
    };
    assert_eq!(found(q("tail")).await?, ["zeta"], "past a NUL");
    assert_eq!(found(tag("a\0b")).await?, ["zeta"]);
    assert!(found(tag("a\0c")).await?.is_empty(), "a NUL is not an end");
    assert!(found(tag("a")).await?.is_empty());
    assert_eq!(found(q("yyyy")).await?, ["eta"]);
    assert!(
        found(q("farneedle")).await?.is_empty(),
        "past the searched characters"
    );
    Ok(())
}

/// UI-1c: `runbook_by_slug` finds this scope's runbook by its exact slug
/// (a point read of the unique `(org_id, project_id, slug)` index on the SQL
/// editions), and nothing for a slug only another project of the same org
/// has, a prefix, another case or a slug no runbook has. Run [`seed`] on
/// `store` first; `other` is another project of the same org, seeded here.
pub async fn runbook_by_slug<S: AuthoringStore>(store: &S, other: &S) -> Result<(), StoreError> {
    let record = |n: u8, slug: &str| RunbookRecord {
        runbook_id: rb(n),
        slug: slug.into(),
        origin: RunbookOrigin::Ui,
        archived: false,
    };
    let minimal = |slug: &str| {
        format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {slug}}}\n\
             spec: {{steps: [{{id: s, title: S}}]}}\n"
        )
    };
    for (n, slug) in [(21, "alpha"), (22, "only-other")] {
        other
            .catalog()
            .create_runbook(record(n, slug), minimal(slug))
            .await?;
    }
    for (n, slug, _) in fixture() {
        assert_eq!(
            store.runbook_by_slug(slug).await?,
            Some(record(n, slug)),
            "{slug}"
        );
    }
    assert_eq!(
        other.runbook_by_slug("alpha").await?,
        Some(record(21, "alpha"))
    );
    for missing in ["only-other", "alph", "alpha-", "ALPHA", "nope", "epsilon"] {
        assert_eq!(store.runbook_by_slug(missing).await?, None, "{missing}");
    }
    Ok(())
}
