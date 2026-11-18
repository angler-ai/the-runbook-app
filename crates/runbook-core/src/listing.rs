//! UI-1 / UI-1b: what the runbook list shows and searches for each runbook,
//! derived from its current draft, and the list's search and page cut.
//!
//! Every edition derives a runbook's listing with [`DraftListing::of`]; the
//! Workers edition stores it beside the draft when the draft is written
//! (D1 `1018`), so a list request reads rows only, never a source (UI-1b).
//! Postgres and the memory stores derive it while reading. The search and
//! page rules here are the reference the D1 query must answer alike (the
//! listing conformance case checks both).
use crate::catalog::RunbookRecord;
use serde_json::Value;

/// The list carries at most this many characters of a description
/// (`description_truncated` says it was cut); `q` searches further (up to
/// [`SEARCHED_CHARS`]).
pub const LISTED_DESCRIPTION_CHARS: usize = 300;

/// UI-1b review round 1: `q` searches the first this many characters of
/// each field (slug, title, description, each tag), on every edition, so
/// the search text D1 stores beside a draft stays small (D1 rows are at
/// most 2,000,000 bytes) whatever the draft says.
pub const SEARCHED_CHARS: usize = 2_000;

/// The version of [`DraftListing::of`] and of [`searched_terms`] (and of
/// the D1 store's encoding of them). A store that keeps listings
/// recomputes any row stored under another value; bump it whenever the
/// derivation changes. The golden tests (`listing::tests::golden` here,
/// `runbook-store-d1`'s `listing::tests::golden`) pin the derivation's
/// output to this value and fail until it is bumped.
///
/// 2 (UI-1c): a field is lowercased before it is cut to [`SEARCHED_CHARS`].
pub const LISTING_FORMAT: i64 = 2;

/// A runbook's list fields, from its current draft. The title falls back to
/// the slug (no draft, a draft that does not parse, or an empty title);
/// description and tags are the draft's metadata, when present. `digest` is
/// the SHA-256 of the draft's source (`draft_ahead` compares it with the
/// latest published version's), `None` when there is no draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftListing {
    pub title: String,
    /// The whole description (the list cuts it; search reads all of it).
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub digest: Option<[u8; 32]>,
}

impl DraftListing {
    pub fn of(slug: &str, source: Option<&str>) -> Self {
        let spec = source.and_then(|s| runbook_spec::parse(s).ok());
        let metadata = spec.as_ref().map(|spec| &spec.metadata);
        Self {
            title: metadata
                .map(|m| m.title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| slug.to_owned()),
            description: metadata
                .and_then(|m| m.extra.get("description"))
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map(str::to_owned),
            // UI-1 review: trimmed, empty ones dropped, duplicates removed
            // (first occurrence kept), so a row never repeats a tag and
            // `tag=` matches what the row shows.
            tags: metadata
                .and_then(|m| m.extra.get("tags"))
                .and_then(Value::as_array)
                .map(|tags| {
                    let mut seen = std::collections::BTreeSet::new();
                    tags.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|t| !t.is_empty() && seen.insert(*t))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            digest: source.map(|s| crate::portable::sha256(s.as_bytes())),
        }
    }

    /// The lowercased strings `q` is matched against besides the slug:
    /// the title, the description and each tag, each lowercased and then
    /// cut to its first [`SEARCHED_CHARS`] characters (one entry each, so a
    /// match never spans two fields).
    pub fn search_terms(&self) -> Vec<String> {
        std::iter::once(self.title.as_str())
            .chain(self.description.as_deref())
            .chain(self.tags.iter().map(String::as_str))
            .map(searched)
            .collect()
    }

    /// The row the list shows: the description cut to
    /// [`LISTED_DESCRIPTION_CHARS`].
    pub fn listed(self, record: RunbookRecord) -> ListedRunbook {
        let (description, description_truncated) = listed_description(self.description);
        ListedRunbook {
            record,
            title: self.title,
            description,
            description_truncated,
            tags: self.tags,
            draft_digest: self.digest,
        }
    }
}

/// One field as `q` searches it: lowercased, then cut to its first
/// [`SEARCHED_CHARS`] characters. UI-1c: lowercased whole first, so the cut
/// never changes how a character lowercases (a `Σ` at the cut is `σ` when
/// the field goes on with a letter, as `q` lowercases it, not a final `ς`).
fn searched(field: &str) -> String {
    let lower = field.to_lowercase();
    match lower.char_indices().nth(SEARCHED_CHARS) {
        Some((at, _)) => lower[..at].to_owned(),
        None => lower,
    }
}

/// Everything `q` is matched against for one runbook: the slug, then
/// [`DraftListing::search_terms`]. A runbook matches `q` when one of these
/// contains it.
pub fn searched_terms(slug: &str, listing: &DraftListing) -> Vec<String> {
    std::iter::once(searched(slug))
        .chain(listing.search_terms())
        .collect()
}

/// A description as the list carries it, and whether it was cut.
pub fn listed_description(description: Option<String>) -> (Option<String>, bool) {
    match description {
        Some(d) => match d.char_indices().nth(LISTED_DESCRIPTION_CHARS) {
            Some((at, _)) => (Some(d[..at].to_owned()), true),
            None => (Some(d), false),
        },
        None => (None, false),
    }
}

/// One runbook as a list page holds it: small fields only, never a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRunbook {
    pub record: RunbookRecord,
    pub title: String,
    /// At most [`LISTED_DESCRIPTION_CHARS`] characters.
    pub description: Option<String>,
    pub description_truncated: bool,
    pub tags: Vec<String>,
    pub draft_digest: Option<[u8; 32]>,
}

/// One list page's request, as the API normalized it: `q` trimmed and
/// lowercased (`None` when blank), `tags` trimmed, sorted and de-duplicated
/// (all required, exact), `after` the previous page's last runbook ID, and
/// `limit` how many rows to return at most (the API asks for one more than
/// the page to learn whether another page exists).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingQuery {
    pub q: Option<String>,
    pub tags: Vec<String>,
    pub after: Option<String>,
    pub limit: usize,
}

impl ListingQuery {
    /// Whether a runbook matches the search (`after` aside): every
    /// required tag, exactly; and `q` as a substring of one of
    /// [`searched_terms`] (the lowercased slug, title, description or a
    /// tag, each up to [`SEARCHED_CHARS`] characters).
    pub fn matches(&self, slug: &str, listing: &DraftListing) -> bool {
        if !self.tags.iter().all(|t| listing.tags.contains(t)) {
            return false;
        }
        let Some(q) = &self.q else {
            return true;
        };
        searched_terms(slug, listing)
            .iter()
            .any(|term| term.contains(q.as_str()))
    }

    /// Whether a runbook ID comes after the cursor (IDs descend, compared
    /// as bytes).
    pub fn after_cursor(&self, runbook_id: &str) -> bool {
        self.after.as_deref().is_none_or(|after| runbook_id < after)
    }

    /// The reference page: the matching runbooks after the cursor, newest
    /// ID first, at most `limit`.
    pub fn page(
        &self,
        rows: impl IntoIterator<Item = (RunbookRecord, DraftListing)>,
    ) -> Vec<ListedRunbook> {
        let mut rows: Vec<(RunbookRecord, DraftListing)> = rows
            .into_iter()
            .filter(|(record, listing)| {
                self.after_cursor(record.runbook_id.as_str()) && self.matches(&record.slug, listing)
            })
            .collect();
        rows.sort_by(|a, b| b.0.runbook_id.as_str().cmp(a.0.runbook_id.as_str()));
        rows.truncate(self.limit);
        rows.into_iter()
            .map(|(record, listing)| listing.listed(record))
            .collect()
    }
}

/// One list page as a store answers it: `rows` as [`ListingQuery::page`]
/// would cut them, unless the store stopped early.
///
/// UI-1b review round 1: a store may bound the work one request does (D1
/// derives at most a fixed number of listings a request). When it stops
/// before it has `limit` rows and before the end of the list, it says how
/// far it looked: `examined_to` is the last runbook ID it examined, and
/// `rows` holds every matching runbook after the cursor down to and
/// including it. The next page continues after it, so a short page never
/// skips a runbook nor ends the list early.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingPage {
    pub rows: Vec<ListedRunbook>,
    pub examined_to: Option<String>,
}

impl ListingPage {
    /// A page that is the reference's answer (nothing left unexamined).
    pub fn complete(rows: Vec<ListedRunbook>) -> Self {
        Self {
            rows,
            examined_to: None,
        }
    }
}

/// UI-1b review round 1: what the golden tests pin [`LISTING_FORMAT`] to.
/// A store that keeps listings (D1) recomputes a row only when its stored
/// format differs from the current one, so a change to the derivation that
/// leaves the constant alone would leave every stored listing disagreeing
/// with Postgres and the single read. The golden tests hash the derivation's
/// output for [`golden::fixtures`] and compare it with the digest recorded
/// for the current format.
#[doc(hidden)]
pub mod golden {
    /// The drafts the golden tests derive: (slug, draft source). They cover
    /// a draft that does not parse, none at all, titles derived from the ID
    /// or empty, tags with commas, quotes, backslashes, tabs, NUL and
    /// surrounding blanks (trimmed, repeated, empty), descriptions past the
    /// listed and the searched lengths, and non-ASCII case folding.
    pub fn fixtures() -> Vec<(&'static str, Option<String>)> {
        let head = "apiVersion: therunbook.app/v1\nkind: Runbook\n";
        let steps = "spec: {steps: [{id: s, title: S}]}\n";
        let long: String = (0..2_500)
            .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap_or(0)))
            .collect();
        vec![
            ("broken", Some("[".into())),
            ("absent", None),
            (
                "derived-title",
                Some(format!("{head}metadata: {{id: derived-title}}\n{steps}")),
            ),
            (
                "empty-title",
                Some(format!(
                    "{head}metadata: {{id: empty-title, title: ''}}\n{steps}"
                )),
            ),
            (
                "tagged",
                Some(format!(
                    "{head}metadata:\n  id: tagged\n  title: Tagged\n  tags: ['a,b', ' prod ', prod, '', \
                     \"q\\\"uote\", \"back\\\\slash\", \"nul\\0x\", \"tab\\tx\", 7]\n{steps}"
                )),
            ),
            (
                "long",
                Some(format!(
                    "{head}metadata: {{id: long, title: Long, description: '{long}'}}\n{steps}"
                )),
            ),
            (
                "unicode",
                Some(format!(
                    "{head}metadata:\n  id: unicode\n  title: \"İstanbul ΣΑΣ Éclair\\0 tail\"\n  \
                     description: \"Ünïcödé 🚀 e\\u0301 ß {long}\"\n  tags: [ÄPI, 数据, ΣΑΣ]\n{steps}"
                )),
            ),
            // UI-1c: a `Σ` at the searched cut followed by a letter, and an
            // `İ` (two characters lowercased) just before it.
            (
                "sigma-cut",
                Some(format!(
                    "{head}metadata: {{id: sigma-cut, title: '{}ΣA', description: '{}İz'}}\n{steps}",
                    "X".repeat(1_999),
                    "y".repeat(1_998)
                )),
            ),
            (
                "odd-types",
                Some(format!(
                    "{head}metadata: {{id: odd-types, title: Odd, description: 5, tags: prod}}\n{steps}"
                )),
            ),
        ]
    }

    /// The (format, digest) pairs the derivation has had, oldest first;
    /// append one (never edit one) when [`super::LISTING_FORMAT`] is bumped.
    /// `listing::tests::golden` here pins this crate's derivation; the D1
    /// store keeps its own table for its stored encoding.
    pub const DERIVATION: &[(i64, &str)] = &[
        (
            1,
            "1639b869a8cccc74c60a16b6addb7699a9e4a752d2294ad92a6c20ef569aebce",
        ),
        (
            2,
            "6fd0e26f065cf087cab1d3b9dd8496522439438fa892cb7daceec9d22d2d6c66",
        ),
    ];

    /// Check a derivation digest against a golden table for the current
    /// format: the error says what to do.
    pub fn check(format: i64, digest: &str, table: &[(i64, &str)]) -> Result<(), String> {
        if table
            .windows(2)
            .any(|w| w[0].0 >= w[1].0 || w[0].1 == w[1].1)
        {
            return Err("the golden table must list each format once, increasing, \
                        each with a new digest"
                .into());
        }
        let Some(&(last, recorded)) = table.last() else {
            return Err(format!(
                "append ({format}, \"{digest}\") to the golden table"
            ));
        };
        if format == last && digest == recorded {
            return Ok(());
        }
        if format == last {
            return Err(format!(
                "the listing derivation's output changed (digest {digest}, recorded \
                 {recorded} for format {format}): bump LISTING_FORMAT \
                 (runbook_core::listing) to {} so stored listings are recomputed, and \
                 append ({}, \"{digest}\") to the golden table",
                format + 1,
                format + 1
            ));
        }
        if format > last {
            return Err(format!(
                "LISTING_FORMAT is {format}: append ({format}, \"{digest}\") to the golden table"
            ));
        }
        Err(format!(
            "LISTING_FORMAT ({format}) is older than the golden table's last format ({last})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::RunbookOrigin;

    fn record(n: u8, slug: &str) -> RunbookRecord {
        RunbookRecord {
            runbook_id: format!("rb_{n:026}").parse().unwrap(),
            slug: slug.into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        }
    }

    #[test]
    fn the_draft_gives_title_description_tags_and_digest() {
        let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: deploy, title: Deploy API, description: Ships it, tags: [prod, api]}\nspec:\n  steps: [{id: work, title: Work}]\n";
        let listing = DraftListing::of("deploy", Some(source));
        assert_eq!(listing.title, "Deploy API");
        assert_eq!(listing.description.as_deref(), Some("Ships it"));
        assert_eq!(listing.tags, ["prod", "api"]);
        assert_eq!(
            listing.digest,
            Some(crate::portable::sha256(source.as_bytes()))
        );
        assert_eq!(
            listing.search_terms(),
            ["deploy api", "ships it", "prod", "api"]
        );
        // No draft: the slug, no digest.
        let none = DraftListing::of("x", None);
        assert_eq!((none.title.as_str(), none.digest), ("x", None));
    }

    #[test]
    fn the_reference_page_filters_cuts_and_orders() {
        let rows = vec![
            (record(1, "one"), DraftListing::of("one", Some("["))),
            (
                record(3, "three"),
                DraftListing::of(
                    "three",
                    Some("metadata: {id: three, title: Éclair, tags: [ops]}\nspec: {steps: []}\n"),
                ),
            ),
            (record(2, "two"), DraftListing::of("two", None)),
        ];
        let page = |q: ListingQuery| {
            q.page(rows.clone())
                .into_iter()
                .map(|r| r.record.slug)
                .collect::<Vec<_>>()
        };
        let all = ListingQuery {
            limit: 10,
            ..Default::default()
        };
        assert_eq!(page(all.clone()), ["three", "two", "one"]);
        assert_eq!(
            page(ListingQuery {
                limit: 2,
                ..all.clone()
            }),
            ["three", "two"]
        );
        assert_eq!(
            page(ListingQuery {
                after: Some(format!("rb_{:026}", 3)),
                ..all.clone()
            }),
            ["two", "one"]
        );
        assert_eq!(
            page(ListingQuery {
                q: Some("éclair".into()),
                ..all.clone()
            }),
            ["three"]
        );
        assert_eq!(
            page(ListingQuery {
                tags: vec!["ops".into()],
                ..all
            }),
            ["three"]
        );
    }

    /// The derivation's output for the golden fixtures, as one digest.
    fn derivation_digest() -> String {
        let out: Vec<serde_json::Value> = golden::fixtures()
            .into_iter()
            .map(|(slug, source)| {
                let listing = DraftListing::of(slug, source.as_deref());
                let terms = searched_terms(slug, &listing);
                let listed = listed_description(listing.description.clone());
                serde_json::json!({
                    "title": listing.title,
                    "description": listing.description,
                    "tags": listing.tags,
                    "digest": listing.digest.map(|d| d.iter().map(|b| format!("{b:02x}")).collect::<String>()),
                    "searched": terms,
                    "listed": listed,
                })
            })
            .collect();
        crate::portable::sha256(serde_json::to_string(&out).unwrap().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// UI-1b review round 1, finding 2: the derivation's output is pinned
    /// to LISTING_FORMAT; changing it without bumping the format fails here.
    #[test]
    fn golden() {
        if let Err(message) =
            golden::check(LISTING_FORMAT, &derivation_digest(), golden::DERIVATION)
        {
            panic!("{message}");
        }
    }

    #[test]
    fn the_golden_check_asks_for_a_bump() {
        let table: &[(i64, &str)] = &[(1, "aa"), (2, "bb")];
        assert!(golden::check(2, "bb", table).is_ok());
        let changed = golden::check(2, "cc", table).unwrap_err();
        assert!(changed.contains("bump LISTING_FORMAT"), "{changed}");
        assert!(changed.contains("to 3"), "{changed}");
        assert!(
            golden::check(3, "cc", table)
                .unwrap_err()
                .contains("append (3")
        );
        assert!(golden::check(1, "aa", table).is_err());
        assert!(golden::check(2, "bb", &[(2, "bb"), (1, "aa")]).is_err());
        assert!(golden::check(2, "aa", &[(1, "aa"), (2, "aa")]).is_err());
    }

    /// UI-1c: a field is lowercased whole, then cut, so the cut never
    /// changes how a character lowercases. A `Σ` at character 2,000 followed
    /// by a letter is `σ` in the field (it is not word-final); cut first, it
    /// ended the text and lowercased to the final `ς`, so `q` "xσ" missed a
    /// runbook whose title reads "…XΣA". An `İ` (one character, two
    /// lowercased) keeps the window to 2,000 characters of the lowercased
    /// text.
    #[test]
    fn a_field_is_lowercased_before_it_is_cut() {
        let title = format!("{}ΣA", "X".repeat(SEARCHED_CHARS - 1));
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: d, title: '{title}', \
             description: '{}İz'}}\nspec: {{steps: []}}\n",
            "y".repeat(SEARCHED_CHARS - 2)
        );
        let listing = DraftListing::of("d", Some(&source));
        let terms = searched_terms("d", &listing);
        assert!(
            terms[1].ends_with("xσ"),
            "{}",
            &terms[1][terms[1].len() - 8..]
        );
        assert_eq!(terms[1].chars().count(), SEARCHED_CHARS);
        assert!(
            terms[2].ends_with("yi\u{307}"),
            "{:?}",
            &terms[2][terms[2].len() - 6..]
        );
        assert_eq!(terms[2].chars().count(), SEARCHED_CHARS);
        let q = |q: &str| ListingQuery {
            q: Some(q.into()),
            limit: 1,
            ..Default::default()
        };
        assert!(q("xσ").matches("d", &listing));
        assert!(!q("xς").matches("d", &listing));
    }

    #[test]
    fn search_covers_the_first_searched_chars_of_each_field() {
        let description = format!("{} needle", "y".repeat(SEARCHED_CHARS));
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: d, title: İx, description: '{description}', tags: [\"a\\0b\"]}}\nspec: {{steps: []}}\n"
        );
        let listing = DraftListing::of("d", Some(&source));
        assert_eq!(
            listing.description.as_deref(),
            Some(description.as_str()),
            "kept whole"
        );
        let terms = searched_terms("d", &listing);
        assert_eq!(terms[0], "d");
        assert_eq!(terms[1], "İx".to_lowercase());
        assert_eq!(terms[2].chars().count(), SEARCHED_CHARS);
        assert_eq!(terms[3], "a\0b");
        let q = |q: &str| ListingQuery {
            q: Some(q.into()),
            limit: 1,
            ..Default::default()
        };
        assert!(q("yyy").matches("d", &listing));
        assert!(
            !q("needle").matches("d", &listing),
            "past the searched characters"
        );
        assert!(q("\0b").matches("d", &listing));
    }
}
