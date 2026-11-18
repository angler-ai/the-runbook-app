//! UI-1b: the runbook list on D1 from rows only. Each runbook keeps its
//! current draft's listing beside the draft (D1 `1018`): what the list shows
//! (`listing`), what `q` matches (`listing_search`), the draft's digest
//! (`listing_digest`) and the draft revision and derivation it describes.
//! Every draft write sets them in the statement that writes the draft, so a
//! list page is one query that filters, orders and cuts in SQL and reads no
//! source; rows written before 1018, or by an older release after a
//! rollback, are derived from their drafts as a request reaches them, at
//! most [`STALE_PER_REQUEST`] a request (review round 1).
//!
//! The answer is [`ListingQuery::page`]'s (the reference the listing
//! conformance case holds every edition to):
//! - `q` is matched with `instr` over `listing_search`, a BLOB of the terms
//!   [`searched_terms`] gives (lowercased by Rust, each cut to
//!   `SEARCHED_CHARS`), separated by the byte 0xFF, which UTF-8 never
//!   contains: so a match never spans two fields, and BLOB `instr` compares
//!   bytes, so a NUL in a term is a byte like any other (review round 1;
//!   SQLite's `json_each` before 3.45 ends a string at `\u0000`);
//! - every required tag must equal one of the row's tags: SQL keeps the
//!   rows `json_each` says have them (it may keep too many where an old
//!   SQLite cuts a tag at a NUL, never too few), and Rust decides from the
//!   tags in `listing`;
//! - runbook IDs compare as bytes (`BINARY`), as Rust compares strings.
use crate::catalog::{D1Catalog, err};
use crate::sql::{Columns, D1, Statement, prepare, uint};
use runbook_core::{
    StoreError,
    catalog::{RunbookOrigin, RunbookRecord},
    listing::{
        DraftListing, LISTING_FORMAT, ListedRunbook, ListingPage, ListingQuery, listed_description,
        searched_terms,
    },
};
use runbook_model::RunbookId;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

/// The listing columns a draft write sets, in this order (an `INSERT`'s
/// column list; [`LISTING_SET`] is the same for an `UPDATE`).
pub const LISTING_COLUMNS: &str =
    "listing, listing_search, listing_digest, listing_revision, listing_format";
pub const LISTING_SET: &str = "listing = ?, listing_search = ?, listing_digest = ?, \
     listing_revision = ?, listing_format = ?";

/// UI-1b review round 1: the stale rows (no listing, or one for another
/// draft revision or format) one list request derives at most. Each costs
/// at most one subrequest besides D1's (its draft from R2, or a long inline
/// draft read alone), so a request stays far inside the Workers limits
/// whatever the project's size; past it, the page stops short and its
/// cursor continues after the last row examined.
pub const STALE_PER_REQUEST: usize = 100;
/// Rounds (a select and, when rows were stale, a batch) one request makes
/// at most. The first asks for the page; the second, when stale rows did
/// not fill it, also for as many rows as the budget has left, so it spends
/// the budget or completes the page; a third only follows rows SQL kept
/// that Rust then refused (a tag cut at a NUL by an old SQLite).
const ROUNDS: usize = 3;
/// UI-1c: derived listings are written a batch of this many at a time, as
/// they are derived, not once a round: a request that fails part-way (on
/// Workers Free, at the 50-subrequest limit, each draft being an R2 read)
/// still keeps what it derived, so the next request reads fewer drafts.
/// Each batch is one D1 query (and one subrequest).
pub const LISTING_FLUSH: usize = 10;
/// A stale row's inline draft comes back with the page when it is at most
/// this many bytes (a draft in R2 has none); a longer one is read alone.
/// So a select holds at most a few MiB of drafts.
const INLINE_IN_PAGE: usize = 16_384;
/// The most bytes `listing` and `listing_search` together may hold. A
/// listing that would be larger (only a pathological draft: thousands of
/// tags, a title of hundreds of KiB) is not stored, so no draft write
/// fails on D1's 2,000,000-byte row limit; such a row is listed from its
/// draft each time a request reaches it (counted in [`STALE_PER_REQUEST`]).
pub const STORED_LISTING_BYTES: usize = 65_536;
/// Separates the terms in `listing_search`; never a byte of UTF-8.
const TERM_SEPARATOR: u8 = 0xFF;

/// UI-1c: one runbook by its slug. Binds: org, project, slug. A point read
/// of the unique `(org_id, project_id, slug)` index D1 `0015` declares
/// (`runbook_list_plan.rs` checks the plan); `=` on TEXT is BINARY, so the
/// slug matches byte for byte. Columns: the record (0–3).
pub const RUNBOOK_BY_SLUG_SQL: &str = "SELECT runbook_id, slug, origin, archived FROM runbook \
     WHERE org_id = ? AND project_id = ? AND slug = ?";

/// What the list shows, as `listing` stores it.
#[derive(Debug, Serialize, Deserialize)]
struct Shown {
    title: String,
    description: Option<String>,
    description_truncated: bool,
    tags: Vec<String>,
}

/// A draft's listing as its row stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredListing {
    /// `listing` (JSON) and `listing_search`; `None` when they would
    /// exceed [`STORED_LISTING_BYTES`] (the row then keeps no listing).
    pub stored: Option<(String, Vec<u8>)>,
    pub digest: Vec<u8>,
}
impl StoredListing {
    pub fn of(slug: &str, source: &str) -> Result<Self, StoreError> {
        Self::derived(slug, &DraftListing::of(slug, Some(source)))
    }
    fn derived(slug: &str, listing: &DraftListing) -> Result<Self, StoreError> {
        let search = encode_terms(&searched_terms(slug, listing));
        let (description, description_truncated) = listed_description(listing.description.clone());
        let shown = serde_json::to_string(&Shown {
            title: listing.title.clone(),
            description,
            description_truncated,
            tags: listing.tags.clone(),
        })
        .map_err(|_| StoreError::Backend("d1.encode"))?;
        Ok(Self {
            stored: (shown.len() + search.len() <= STORED_LISTING_BYTES).then_some((shown, search)),
            digest: listing.digest.map(|d| d.to_vec()).unwrap_or_default(),
        })
    }
    /// Bind the five [`LISTING_COLUMNS`] values (the listing describes the
    /// draft at `revision`).
    pub fn bind(&self, statement: Statement, revision: u64) -> Result<Statement, StoreError> {
        let (listing, search) = match &self.stored {
            Some((listing, search)) => (Some(listing.as_str()), Some(search.clone())),
            None => (None, None),
        };
        Ok(statement
            .bind(listing)
            .bind(search)
            .bind(self.digest.clone())
            .bind(uint(revision).map_err(err)?)
            .bind(LISTING_FORMAT))
    }
}

/// `listing_search`: the terms, separated by [`TERM_SEPARATOR`].
fn encode_terms(terms: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(terms.iter().map(|t| t.len() + 1).sum());
    for (at, term) in terms.iter().enumerate() {
        if at > 0 {
            out.push(TERM_SEPARATOR);
        }
        out.extend_from_slice(term.as_bytes());
    }
    out
}

/// A row's listing is stale when it describes another draft revision or
/// another derivation, or there is none (written before 1018, or too
/// large to store).
fn stale() -> String {
    format!(
        "(listing IS NULL OR listing_revision IS NOT draft_revision \
         OR listing_format IS NOT {LISTING_FORMAT})"
    )
}

/// One round of a page. A row passes when it matches the search (the
/// tags as `json_each` reads them) or its listing is stale (so the caller
/// sees and derives every stale row that could belong to the page).
/// Binds: org, project, after twice, q (a BLOB) twice, the required tags
/// (a JSON array), the limit. Columns: the record (0–3), `listing` (4),
/// `listing_digest` (5), whether stale (6) and, for a stale row only, its
/// inline draft when at most [`INLINE_IN_PAGE`] bytes (7) and the draft's
/// digest (8); then `draft_revision` (9). Exposed for tests.
#[doc(hidden)]
pub fn listing_page_sql() -> String {
    let stale = stale();
    format!(
        "SELECT runbook_id, slug, origin, archived, listing, listing_digest, \
           CASE WHEN {stale} THEN 1 ELSE 0 END, \
           CASE WHEN {stale} AND length(CAST(draft_source AS BLOB)) <= {INLINE_IN_PAGE} \
             THEN draft_source END, \
           CASE WHEN {stale} THEN draft_digest END, draft_revision \
         FROM runbook r WHERE org_id = ? AND project_id = ? \
           AND (? IS NULL OR runbook_id < ?) \
           AND ({stale} OR ( \
             (? IS NULL OR instr(r.listing_search, ?) > 0) \
             AND NOT EXISTS (SELECT 1 FROM json_each(?) w WHERE NOT EXISTS ( \
               SELECT 1 FROM json_each(r.listing, '$.tags') t WHERE t.value = w.value)))) \
         ORDER BY runbook_id DESC LIMIT ?"
    )
}

fn origin_of(text: &str) -> RunbookOrigin {
    match text {
        "git" => RunbookOrigin::Git,
        _ => RunbookOrigin::Ui,
    }
}

fn record_of(row: &crate::sql::Row) -> Result<RunbookRecord, StoreError> {
    Ok(RunbookRecord {
        runbook_id: row.parse(0).map_err(err)?,
        slug: row.string(1).map_err(err)?,
        origin: origin_of(row.text(2).map_err(err)?),
        archived: row.flag(3).map_err(err)?,
    })
}

fn listed_of(row: &crate::sql::Row, record: RunbookRecord) -> Result<ListedRunbook, StoreError> {
    let shown: Shown = row.json(4).map_err(err)?;
    let draft_digest = row
        .optional_blob(5)
        .map_err(err)?
        .map(|d| <[u8; 32]>::try_from(d.as_slice()))
        .transpose()
        .map_err(|_| StoreError::Backend("d1.listing_digest"))?;
    Ok(ListedRunbook {
        record,
        title: shown.title,
        description: shown.description,
        description_truncated: shown.description_truncated,
        tags: shown.tags,
        draft_digest,
    })
}

impl<D: D1> D1Catalog<D> {
    /// UI-1b: one page of the runbook list ([`ListingQuery`]), from rows:
    /// one query in the steady state.
    ///
    /// Review round 1: the rows are walked in the list's order from the
    /// cursor, and Rust decides each: a row with a current listing as SQL
    /// filtered it (its tags checked again), a stale row from its draft
    /// (read from R2 when external), derived here and matched as the
    /// reference matches. Derived listings are written in batches (UI-1c,
    /// below), each guarded by the draft revision it describes; the writes
    /// only spare later requests the work, so the answer never depends on
    /// them (a write lost to a concurrent save, or refused, changes
    /// nothing here). At most [`STALE_PER_REQUEST`] stale rows and
    /// [`ROUNDS`] rounds a request: when either runs out before the page
    /// is full and before the end of the list, the page says how far it
    /// examined ([`ListingPage::examined_to`]) and the next continues there.
    ///
    /// UI-1c: the writes go out a batch of [`LISTING_FLUSH`] at a time as
    /// listings are derived, and before a failed draft read's error is
    /// returned, so a request that fails part-way keeps what it derived.
    pub async fn runbook_listing_page(
        &self,
        query: &ListingQuery,
    ) -> Result<ListingPage, StoreError> {
        let tags =
            serde_json::to_string(&query.tags).map_err(|_| StoreError::Backend("d1.encode"))?;
        let q = query.q.as_deref().map(str::as_bytes);
        let mut out = vec![];
        let mut after = query.after.clone();
        let mut budget = STALE_PER_REQUEST;
        for round in 0..ROUNDS {
            let need = query.limit - out.len();
            let fetch = if round == 0 { need } else { need + budget };
            let rows = self
                .db
                .all(
                    prepare(listing_page_sql())
                        .bind(self.org())
                        .bind(self.project())
                        .bind(after.as_deref())
                        .bind(after.as_deref())
                        .bind(q)
                        .bind(q)
                        .bind(tags.as_str())
                        .bind(crate::catalog::count(fetch)?),
                )
                .await
                .map_err(err)?;
            let mut writes = vec![];
            let mut stopped = false;
            for row in &rows {
                if out.len() == query.limit {
                    break;
                }
                let record = record_of(row)?;
                let id = record.runbook_id.to_string();
                if row.flag(6).map_err(err)? {
                    if budget == 0 {
                        stopped = true;
                        break;
                    }
                    budget -= 1;
                    // UI-1c: a draft that cannot be read fails the request
                    // as before (a guess would be a wrong answer), after the
                    // listings derived so far are written.
                    let draft = match self.stale_draft(row, &record).await {
                        Ok(draft) => draft,
                        Err(error) => {
                            self.store_listings(writes).await;
                            return Err(error);
                        }
                    };
                    if let Some((source, revision)) = draft {
                        let listing = DraftListing::of(&record.slug, Some(&source));
                        let stored = StoredListing::derived(&record.slug, &listing)?;
                        if stored.stored.is_some() {
                            let statement = prepare(format!(
                                "UPDATE runbook SET {LISTING_SET} WHERE org_id = ? \
                                 AND project_id = ? AND runbook_id = ? AND draft_revision = ?"
                            ));
                            writes.push(
                                stored
                                    .bind(statement, revision)?
                                    .bind(self.org())
                                    .bind(self.project())
                                    .bind(id.as_str())
                                    .bind(uint(revision).map_err(err)?),
                            );
                            // UI-1c: a full batch goes out at once, before
                            // the next draft is read.
                            if writes.len() == LISTING_FLUSH {
                                self.store_listings(std::mem::take(&mut writes)).await;
                            }
                        }
                        if query.matches(&record.slug, &listing) {
                            out.push(listing.listed(record));
                        }
                    }
                } else {
                    let listed = listed_of(row, record)?;
                    if query.tags.iter().all(|t| listed.tags.contains(t)) {
                        out.push(listed);
                    }
                }
                after = Some(id);
            }
            self.store_listings(writes).await;
            if out.len() == query.limit || (!stopped && rows.len() < fetch) {
                return Ok(ListingPage::complete(out));
            }
            if stopped {
                break;
            }
        }
        Ok(ListingPage {
            rows: out,
            examined_to: after,
        })
    }

    /// A stale row's draft and the revision it is: inline from the page
    /// (or read alone when long; `None` when the runbook is gone), then
    /// from R2 when external.
    async fn stale_draft(
        &self,
        row: &crate::sql::Row,
        record: &RunbookRecord,
    ) -> Result<Option<(String, u64)>, StoreError> {
        let (inline, digest, revision) = match row.optional_blob(7).map_err(err)? {
            Some(inline) => (
                inline,
                row.optional_blob(8).map_err(err)?,
                row.unsigned(9).map_err(err)?,
            ),
            None => {
                let alone = self
                    .db
                    .all(
                        prepare(
                            "SELECT draft_source, draft_digest, draft_revision FROM runbook \
                             WHERE org_id = ? AND project_id = ? AND runbook_id = ?",
                        )
                        .bind(self.org())
                        .bind(self.project())
                        .bind(record.runbook_id.as_str()),
                    )
                    .await
                    .map_err(err)?;
                let Some(alone) = alone.first() else {
                    return Ok(None);
                };
                (
                    alone.blob(0).map_err(err)?.to_vec(),
                    alone.optional_blob(1).map_err(err)?,
                    alone.unsigned(2).map_err(err)?,
                )
            }
        };
        let inline = String::from_utf8(inline)
            .map_err(|_| StoreError::Backend("catalog.invalid_draft_utf8"))?;
        let source = crate::sources::stitch_source(self.sources(), inline, digest).await?;
        Ok(Some((source, revision)))
    }

    /// Write derived listings: one batch; when it fails (one row's write
    /// refused fails the whole batch), each alone, so one row never keeps
    /// the others from being stored. Failures are dropped: the page was
    /// answered from the derived listings, and a row left stale is derived
    /// again by a later request.
    async fn store_listings(&self, writes: Vec<Statement>) {
        if writes.is_empty() {
            return;
        }
        if self.db.batch(writes.clone()).await.is_ok() {
            return;
        }
        for write in writes {
            let _ = self.db.run(write).await;
        }
    }

    /// UI-1c: this project's runbook with exactly this slug: one point read
    /// of the unique `(org_id, project_id, slug)` index ([`RUNBOOK_BY_SLUG_SQL`]),
    /// no draft read.
    pub async fn runbook_by_slug(&self, slug: &str) -> Result<Option<RunbookRecord>, StoreError> {
        self.db
            .all(
                prepare(RUNBOOK_BY_SLUG_SQL)
                    .bind(self.org())
                    .bind(self.project())
                    .bind(slug),
            )
            .await
            .map_err(err)?
            .first()
            .map(record_of)
            .transpose()
    }

    /// UI-1b: for each of these runbooks with a published version, its
    /// highest number and the SHA-256 of that version's source: the stored
    /// `source_digest` (every version written since 1010 with a blob
    /// store), else the digest of the inline text. No blob read. One probe
    /// of `runbook_version_latest` (runbook_id, number DESC) per ID.
    pub async fn latest_published_digests(
        &self,
        runbook_ids: &[RunbookId],
    ) -> Result<std::collections::BTreeMap<RunbookId, (NonZeroU32, [u8; 32])>, StoreError> {
        if runbook_ids.is_empty() {
            return Ok(Default::default());
        }
        let ids =
            serde_json::to_string(&runbook_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>())
                .map_err(|_| StoreError::Backend("d1.encode"))?;
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT v.runbook_id, v.number, v.source_digest, \
                       CASE WHEN v.source_digest IS NULL THEN v.source END \
                     FROM json_each(?) k JOIN runbook_version v ON v.version_id = ( \
                       SELECT x.version_id FROM runbook_version x \
                       WHERE x.org_id = ? AND x.project_id = ? AND x.runbook_id = k.value \
                         AND x.number IS NOT NULL \
                       ORDER BY x.number DESC LIMIT 1)",
                )
                .bind(ids)
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?;
        let mut out = std::collections::BTreeMap::new();
        for row in &rows {
            let id: RunbookId = row.parse(0).map_err(err)?;
            let number = u32::try_from(row.integer(1).map_err(err)?)
                .ok()
                .and_then(NonZeroU32::new)
                .ok_or(StoreError::Backend("d1.invalid_version_number"))?;
            let digest = match row.optional_blob(2).map_err(err)? {
                Some(digest) => digest
                    .try_into()
                    .map_err(|_| StoreError::Backend("d1.source_digest"))?,
                None => crate::sources::digest(row.text(3).map_err(err)?),
            };
            out.insert(id, (number, digest));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runbook_core::listing::golden;

    /// The (format, digest) pairs of the stored encoding (`listing`,
    /// `listing_search`, `listing_digest`), oldest first; append one (never
    /// edit one) when `LISTING_FORMAT` is bumped.
    const STORED: &[(i64, &str)] = &[
        (
            1,
            "09ae8f2d5420daea061be162ead9f8eab7739f2a236f185b399baf51ad374197",
        ),
        (
            2,
            "72a22daf2de670e633126ebfb0ff582ea6f6dfff6143a4feb2255900e7a93bca",
        ),
    ];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// UI-1b review round 1, finding 2: what a row stores for the golden
    /// drafts is pinned to `LISTING_FORMAT`, so a change to the derivation
    /// or to its encoding here fails until the format is bumped (and every
    /// stored listing is recomputed).
    #[test]
    fn golden() {
        let out: Vec<serde_json::Value> = golden::fixtures()
            .into_iter()
            .filter_map(|(slug, source)| Some((slug, source?)))
            .map(|(slug, source)| {
                let stored = StoredListing::of(slug, &source).unwrap();
                serde_json::json!({
                    "stored": stored.stored.as_ref().map(|(l, s)| (l.clone(), hex(s))),
                    "digest": hex(&stored.digest),
                })
            })
            .collect();
        let digest = hex(&runbook_core::portable::sha256(
            serde_json::to_string(&out).unwrap().as_bytes(),
        ));
        if let Err(message) = golden::check(LISTING_FORMAT, &digest, STORED) {
            panic!("{message}");
        }
    }

    #[test]
    fn search_terms_are_separated_by_a_byte_utf8_never_has() {
        let listing = DraftListing::of(
            "s",
            Some(
                "apiVersion: therunbook.app/v1\nkind: Runbook\n\
                 metadata: {id: s, title: \"A\\0B\", tags: [x]}\nspec: {steps: []}\n",
            ),
        );
        let stored = StoredListing::derived("s", &listing).unwrap();
        assert_eq!(stored.stored.unwrap().1, b"s\xffa\0b\xffx");
    }
}
