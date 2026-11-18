//! CF12: request replay rows in `directory_request`. A replay row keeps the
//! request's payload, to tell a retry (the same payload: the recorded
//! response) from a reused key (another payload), and the response the
//! retry answers. D1 holds at most 2,000,000 bytes in a row on every plan
//! ([`crate::sql::D1_MAX_LENGTH`]), and an authoring request and its
//! response may each be up to about 2 MB (a runbook source at the
//! compiler's 1 MiB ceiling, escaped), so neither is kept whole once its
//! JSON is over [`INLINE_BYTES`]:
//!
//! - **payload**: its JSON, or `sha256:` and the hex SHA-256 of its JSON.
//!   That is all the comparison needs. The tag is not JSON, so no inline
//!   payload can be read as a digest (CF12 review round 1).
//! - **response**: its JSON, or `replay:{project}:{name}`, an object the
//!   writer stored first under the project's replay prefix (R2 on Workers,
//!   CF12b), named for the response's SHA-256 and the row. Only the
//!   authoring writer, which has the project's blobs, writes a reference;
//!   a reader resolves it through [`ReplayBlobs`].
//!
//! CF12b: the replay objects are not the project's blobs. A project export
//! never reads them, and each goes with its row: [`sweep`] deletes the
//! objects of expired rows, then the rows; every other sweep of the table
//! keeps such rows ([`SWEEPABLE`]) for it.
//!
//! Rows written before read as before: JSON in both columns, and CF12's
//! `blob:{project}:{sha256 hex}`, a response kept in the project's
//! content-addressed (shared) blobs, until the row expires.
use crate::{D1, D1Error, Statement, prepare, sources::SourceFuture, sql::Columns};
use runbook_core::directory_operations::StoredResponse;
use runbook_model::{OrganizationId, ProjectId};
use serde_json::Value;

/// The longest payload or response kept as it is (its JSON, in bytes).
pub const INLINE_BYTES: usize = 65_536;
/// The tag of a payload kept as its digest.
const DIGEST_TAG: &str = "sha256:";
/// CF12's tag of a response kept in a project's shared blobs (read only).
const BLOB_TAG: &str = "blob:";
/// CF12b: the tag of a response kept under a project's replay prefix.
const REPLY_TAG: &str = "replay:";
/// CF12b: the condition every sweep of `directory_request` adds, so that
/// only [`sweep`], which deletes their objects first, removes rows whose
/// response is kept in R2.
pub const SWEEPABLE: &str = "response NOT LIKE 'replay:%'";
/// CF12b: the most expired rows one batch of a [`sweep`] takes (one
/// object delete each).
pub const SWEEP_BATCH: i64 = 50;
/// CF12b review round 1: the most batches the daily cron pass sweeps (up
/// to 1,000 rows). CF12b review round 2: each batch is one SELECT and one
/// `batch()` of at most two statements (one DELETE, one UPDATE), so the
/// pass makes at most 40 D1 queries as Cloudflare counts them (a
/// `batch()` is one query toward the 1,000 an invocation may make) and 60
/// even if each statement counted, plus up to 1,000 R2 deletes: well
/// inside one invocation's 1,000 D1 queries and 10,000 subrequests
/// (Workers Paid) beside the archive job. An authoring write sweeps one
/// batch of its own organization first.
pub const CRON_SWEEP_PASSES: usize = 20;
/// CF12b review round 2: a row whose object delete failed is not tried
/// again before this long has passed (`sweep_after`, migration 1015),
/// doubling per failure up to [`SWEEP_BACKOFF_MAX_MS`], so later sweeps
/// move past it.
pub const SWEEP_BACKOFF_MS: i64 = 3_600_000;
/// The longest a failing row waits between tries: a week.
pub const SWEEP_BACKOFF_MAX_MS: i64 = 7 * 86_400_000;

/// A project's blobs, as a replay reader and sweeper reach them.
pub trait ReplayBlobs {
    /// The bytes stored under `digest` in `project`'s shared blobs, if any
    /// (CF12 rows).
    fn get(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
        digest: [u8; 32],
    ) -> SourceFuture<'_, Option<Vec<u8>>>;
    /// The replay object `name` under `project`'s replay prefix, if any.
    fn get_reply(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
        name: &str,
    ) -> SourceFuture<'_, Option<Vec<u8>>>;
    /// Delete the replay object `name` (absent is fine).
    fn delete_reply(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
        name: &str,
    ) -> SourceFuture<'_, ()>;
}

/// What the row's `payload` column keeps for `payload`.
pub fn stored(payload: &Value) -> String {
    let text = payload.to_string();
    if text.len() <= INLINE_BYTES {
        return text;
    }
    format!(
        "{DIGEST_TAG}{}",
        runbook_core::portable::sha256_hex(text.as_bytes())
    )
}

/// Whether a row's `saved` payload column records `payload`. A column that
/// is neither a digest nor JSON records nothing a request can send.
pub fn matches(saved: &str, payload: &Value) -> bool {
    if let Some(digest) = saved.strip_prefix(DIGEST_TAG) {
        let text = payload.to_string();
        return text.len() > INLINE_BYTES
            && digest == runbook_core::portable::sha256_hex(text.as_bytes());
    }
    serde_json::from_str::<Value>(saved).is_ok_and(|saved| saved == *payload)
}

/// A response as its row keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredReply {
    /// The `response` column.
    pub column: String,
    /// For a reference: the object to store under the project's replay
    /// prefix before the row is written.
    pub blob: Option<ReplyObject>,
}

/// CF12b: one replay response kept in R2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyObject {
    pub project: ProjectId,
    /// `{sha256 hex of bytes}.{row}.{nonce}`: unique to its row, so a sweep
    /// never takes another row's object, even one with the same response;
    /// and (CF12b review round 2) unique to its writer, so two writers of
    /// one key with the same response in the same millisecond never share
    /// an object that one's `release_unrecorded` could delete under the
    /// other's row.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// The row a response is recorded for (its key and its expiry).
#[derive(Clone, Copy, Debug)]
pub struct ReplyRow<'a> {
    pub principal: &'a str,
    pub operation: &'a str,
    pub key: &'a str,
    pub expires_at: i64,
    /// CF12b review round 2: random bytes of this writer (the host's
    /// entropy), in the object's name.
    pub nonce: [u8; 16],
}
impl ReplyRow<'_> {
    /// 16 bytes of SHA-256 over the row's key and expiry, in hex.
    fn token(&self) -> String {
        let text = format!(
            "{}\0{}\0{}\0{}",
            self.principal, self.operation, self.key, self.expires_at
        );
        hex(&runbook_core::portable::sha256(text.as_bytes())[..16])
    }
}

/// What the row's `response` column keeps for `response`, for a writer
/// with `project`'s blobs: its JSON, or a reference to an object under the
/// project's replay prefix.
pub fn stored_reply(
    response: &StoredResponse,
    project: &ProjectId,
    row: ReplyRow<'_>,
) -> Result<StoredReply, serde_json::Error> {
    let text = serde_json::to_string(response)?;
    if text.len() <= INLINE_BYTES {
        return Ok(StoredReply {
            column: text,
            blob: None,
        });
    }
    let bytes = text.into_bytes();
    let name = format!(
        "{}.{}.{}",
        hex(&runbook_core::portable::sha256(&bytes)),
        row.token(),
        hex(&row.nonce)
    );
    Ok(StoredReply {
        column: format!("{REPLY_TAG}{project}:{name}"),
        blob: Some(ReplyObject {
            project: project.clone(),
            name,
            bytes,
        }),
    })
}

/// The project and object name a `replay:` column names, if it is one.
fn reply_reference(column: &str) -> Option<(ProjectId, &str)> {
    let (project, name) = column.strip_prefix(REPLY_TAG)?.split_once(':')?;
    let project = project.parse().ok()?;
    unhex32(name.get(..64)?)?;
    Some((project, name))
}

/// Why a recorded response could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// Neither JSON nor a reference.
    Invalid,
    /// A reference with no blob store to resolve it.
    NoBlobs,
    /// The referenced blob is missing or not the bytes it names.
    Missing,
    /// The blob store failed.
    Store,
}

/// The response a row's `response` column records. A reference is
/// resolved through `blobs` and checked against its digest.
pub async fn read_reply(
    column: &str,
    org: &OrganizationId,
    blobs: Option<&dyn ReplayBlobs>,
) -> Result<StoredResponse, ReplyError> {
    let (digest, found) = if column.starts_with(REPLY_TAG) {
        let (project, name) = reply_reference(column).ok_or(ReplyError::Invalid)?;
        let digest = unhex32(&name[..64]).ok_or(ReplyError::Invalid)?;
        let blobs = blobs.ok_or(ReplyError::NoBlobs)?;
        (digest, blobs.get_reply(org, &project, name).await)
    } else if let Some(reference) = column.strip_prefix(BLOB_TAG) {
        let (project, digest) = reference.split_once(':').ok_or(ReplyError::Invalid)?;
        let project: ProjectId = project.parse().map_err(|_| ReplyError::Invalid)?;
        let digest = unhex32(digest).ok_or(ReplyError::Invalid)?;
        let blobs = blobs.ok_or(ReplyError::NoBlobs)?;
        (digest, blobs.get(org, &project, digest).await)
    } else {
        return serde_json::from_str(column).map_err(|_| ReplyError::Invalid);
    };
    let bytes = found
        .map_err(|_| ReplyError::Store)?
        .ok_or(ReplyError::Missing)?;
    if runbook_core::portable::sha256(&bytes) != digest {
        return Err(ReplyError::Missing);
    }
    serde_json::from_slice(&bytes).map_err(|_| ReplyError::Invalid)
}

/// What a [`sweep`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Rows removed, with their objects.
    pub rows: usize,
    /// Rows kept because their object's delete failed.
    pub failed: usize,
}

/// CF12b: remove expired rows (of `org`, or of every organization) whose
/// response is kept in R2: each object first, then its row, so no live row
/// names a missing object and a failed delete leaves its row to a later
/// sweep.
///
/// CF12b review round 1: oldest first (`ORDER BY expires_at`), in batches
/// of [`SWEEP_BATCH`], at most `passes` of them, until a batch comes back
/// short.
///
/// CF12b review round 2: a row whose object delete failed (or that cannot
/// be parsed) is counted and backs off: its `sweep_after` moves
/// [`SWEEP_BACKOFF_MS`] on, doubling per failure up to
/// [`SWEEP_BACKOFF_MAX_MS`], and no sweep takes it before then. So failing
/// rows never hold the places sweepable rows could take, in this sweep or
/// a later one, on the daily pass or an organization's own. A batch's
/// deletes and backoffs go in one `batch()` of two statements, each naming
/// its rows by `rowid` (at most 50 bound values, inside D1's 100).
///
/// Both statements also require the row to be expired still (`expires_at
/// <= now`). A writer that replaced a row while the sweep waited on R2
/// ([`record`]) made it live again under a new response: it is neither
/// deleted under its new object nor given the old object's backoff
/// (CF12c). A new row never starts expired, so a reused `rowid` cannot
/// match either.
pub async fn sweep<D: D1>(
    db: &D,
    org: Option<&OrganizationId>,
    now: i64,
    blobs: &dyn ReplayBlobs,
    passes: usize,
) -> Result<Swept, D1Error> {
    let select = |scope: &str| {
        prepare(format!(
            "SELECT rowid, org_id, response FROM directory_request \
             WHERE {scope}expires_at <= ? AND NOT ({SWEEPABLE}) \
             AND (sweep_after IS NULL OR sweep_after <= ?) \
             ORDER BY expires_at, org_id, principal, operation, key LIMIT ?"
        ))
    };
    let list = |n: usize| vec!["?"; n].join(", ");
    let mut swept = Swept::default();
    for _ in 0..passes {
        let query = match org {
            Some(org) => select("org_id = ? AND ").bind(org.as_str()),
            None => select(""),
        }
        .bind(now)
        .bind(now)
        .bind(SWEEP_BATCH);
        let rows = db.all(query).await?;
        let (mut gone, mut failed): (Vec<i64>, Vec<i64>) = (vec![], vec![]);
        for row in &rows {
            let rowid = row.integer(0)?;
            let (Ok(org_id), Ok(column)) = (row.parse::<OrganizationId>(1), row.text(2)) else {
                failed.push(rowid);
                continue;
            };
            match reply_reference(column) {
                Some((project, name))
                    if blobs.delete_reply(&org_id, &project, name).await.is_err() =>
                {
                    failed.push(rowid);
                }
                _ => gone.push(rowid),
            }
        }
        let mut statements: Vec<Statement> = Vec::new();
        if !gone.is_empty() {
            statements.push(
                prepare(format!(
                    "DELETE FROM directory_request WHERE rowid IN ({}) AND expires_at <= ?",
                    list(gone.len())
                ))
                .binds(gone.iter().copied())
                .bind(now),
            );
        }
        if !failed.is_empty() {
            statements.push(
                prepare(format!(
                    "UPDATE directory_request SET sweep_failures = sweep_failures + 1, \
                     sweep_after = ? + min(?, ? << min(sweep_failures, 20)) \
                     WHERE rowid IN ({}) AND expires_at <= ?",
                    list(failed.len())
                ))
                .bind(now)
                .bind(SWEEP_BACKOFF_MAX_MS)
                .bind(SWEEP_BACKOFF_MS)
                .binds(failed.iter().copied())
                .bind(now),
            );
        }
        if !statements.is_empty() {
            db.batch(statements).await?;
        }
        swept.rows += gone.len();
        swept.failed += failed.len();
        if (rows.len() as i64) < SWEEP_BATCH {
            break;
        }
    }
    Ok(swept)
}

/// CF12b review round 1: the replay row's key, for the writer's statements.
#[derive(Clone, Copy, Debug)]
pub struct RowKey<'a> {
    pub org: &'a OrganizationId,
    pub principal: &'a str,
    pub operation: &'a str,
    pub key: &'a str,
}

/// CF12b review round 1: before a write records a response under `row`,
/// delete the object an expired row with that key names (one the sweep has
/// not reached yet), since [`record`] replaces that row. An expired row is
/// never read, so its object may go first; a failed delete leaves it
/// unreferenced (storage only). Returns whether such an object was found.
pub async fn forget_expired<D: D1>(
    db: &D,
    row: RowKey<'_>,
    now: i64,
    blobs: &dyn ReplayBlobs,
) -> Result<bool, D1Error> {
    let rows = db
        .all(
            prepare(
                "SELECT response FROM directory_request WHERE org_id = ? AND principal = ? \
                 AND operation = ? AND key = ? AND expires_at <= ?",
            )
            .bind(row.org.as_str())
            .bind(row.principal)
            .bind(row.operation)
            .bind(row.key)
            .bind(now),
        )
        .await?;
    let mut found = false;
    for found_row in &rows {
        if let Ok(column) = found_row.text(0)
            && let Some((project, name)) = reply_reference(column)
        {
            found = true;
            let _ = blobs.delete_reply(row.org, &project, name).await;
        }
    }
    Ok(found)
}

/// CF12b review round 1: the statement that records a response: a new row,
/// or one replacing an expired row with the same key (whose object
/// [`forget_expired`] took); a live row (a concurrent request with the
/// same key that landed first) is left alone.
pub fn record(
    row: RowKey<'_>,
    payload: &Value,
    response_column: String,
    expires_at: i64,
    now: i64,
) -> Statement {
    prepare(
        "INSERT INTO directory_request (org_id, principal, operation, key, payload, \
         response, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (org_id, principal, operation, key) DO UPDATE SET \
         payload = excluded.payload, response = excluded.response, \
         expires_at = excluded.expires_at, sweep_after = NULL, sweep_failures = 0 \
         WHERE directory_request.expires_at <= ?",
    )
    .bind(row.org.as_str())
    .bind(row.principal)
    .bind(row.operation)
    .bind(row.key)
    .bind(stored(payload))
    .bind(response_column)
    .bind(expires_at)
    .bind(now)
}

/// CF12b review round 1: after a write tried to record `column` under
/// `row`, delete `object` (the response it stored first) unless the row
/// names it: the write lost a race for its key, or its batch failed. When
/// the row cannot be read, the object stays (storage only, never read).
pub async fn release_unrecorded<D: D1>(
    db: &D,
    row: RowKey<'_>,
    column: &str,
    object: &ReplyObject,
    blobs: &dyn ReplayBlobs,
) -> bool {
    let Ok(rows) = db
        .all(
            prepare(
                "SELECT response FROM directory_request WHERE org_id = ? AND principal = ? \
                 AND operation = ? AND key = ?",
            )
            .bind(row.org.as_str())
            .bind(row.principal)
            .bind(row.operation)
            .bind(row.key),
        )
        .await
    else {
        return false;
    };
    if rows.iter().any(|r| r.text(0).is_ok_and(|c| c == column)) {
        return false;
    }
    blobs
        .delete_reply(row.org, &object.project, &object.name)
        .await
        .is_ok()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    if text.len() != 64 {
        return None;
    }
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::RefCell, collections::BTreeMap};

    #[test]
    fn small_payloads_are_kept_as_they_are() {
        let payload = json!({"body": {"source": "kind: Runbook"}, "if_match": null});
        assert_eq!(stored(&payload), payload.to_string());
        assert!(matches(&stored(&payload), &payload));
        assert!(!matches(
            &stored(&payload),
            &json!({"body": {}, "if_match": null})
        ));
    }

    #[test]
    fn large_payloads_are_kept_as_their_digest() {
        let big = |c: &str| json!({"body": {"source": c.repeat(3 * 1_048_576)}, "if_match": null});
        let payload = big("\"");
        let kept = stored(&payload);
        assert!(kept.len() < 100, "{} B", kept.len());
        assert!(kept.starts_with("sha256:"));
        assert!(matches(&kept, &payload), "a retry");
        assert!(!matches(&kept, &big("x")), "the key reused");
    }

    /// CF12 review round 1: a small payload shaped like the old digest
    /// wrapper is kept inline and matches only itself; a digest column is
    /// not JSON, so no payload can be mistaken for one.
    #[test]
    fn a_payload_shaped_like_a_digest_is_kept_and_matched_as_itself() {
        let digest = runbook_core::portable::sha256_hex(b"anything");
        let shaped = json!({ "$sha256": digest });
        let kept = stored(&shaped);
        assert_eq!(kept, shaped.to_string(), "inline");
        assert!(matches(&kept, &shaped), "its retry");
        assert!(!matches(&kept, &json!({ "$sha256": "other" })));
        // A digest column for a large payload is not that wrapper.
        let large = json!({ "s": "a".repeat(INLINE_BYTES) });
        assert!(serde_json::from_str::<Value>(&stored(&large)).is_err());
        // A small payload never matches a digest column, even its text.
        let tagged = json!(stored(&large));
        assert!(!matches(&stored(&large), &tagged));
    }

    /// Rows written before CF12 keep JSON in the payload column, including
    /// a payload that happens to be the WIP's `{"$sha256": …}` shape.
    #[test]
    fn rows_written_before_read_as_before() {
        let payload = json!({"body": {"name": "ops"}, "if_match": null});
        assert!(matches(&payload.to_string(), &payload));
        let shaped = json!({ "$sha256": "00" });
        assert!(matches(&shaped.to_string(), &shaped));
        assert!(!matches("not json", &payload));
    }

    #[test]
    fn the_boundary_is_the_inline_size() {
        let at = |n: usize| {
            let empty = json!({"s": ""}).to_string().len();
            json!({"s": "a".repeat(n - empty)})
        };
        assert_eq!(stored(&at(INLINE_BYTES)), at(INLINE_BYTES).to_string());
        assert!(stored(&at(INLINE_BYTES + 1)).starts_with("sha256:"));
    }

    /// (project, shared digest hex or replay object name) → bytes.
    type Stored = BTreeMap<(String, String), Vec<u8>>;
    struct Blobs(RefCell<Stored>);
    impl ReplayBlobs for Blobs {
        fn get(
            &self,
            _org: &OrganizationId,
            project: &ProjectId,
            digest: [u8; 32],
        ) -> SourceFuture<'_, Option<Vec<u8>>> {
            let key = (project.to_string(), format!("shared/{}", hex(&digest)));
            let found = self.0.borrow().get(&key).cloned();
            Box::pin(async move { Ok(found) })
        }
        fn get_reply(
            &self,
            _org: &OrganizationId,
            project: &ProjectId,
            name: &str,
        ) -> SourceFuture<'_, Option<Vec<u8>>> {
            let key = (project.to_string(), format!("replay/{name}"));
            let found = self.0.borrow().get(&key).cloned();
            Box::pin(async move { Ok(found) })
        }
        fn delete_reply(
            &self,
            _org: &OrganizationId,
            project: &ProjectId,
            name: &str,
        ) -> SourceFuture<'_, ()> {
            let key = (project.to_string(), format!("replay/{name}"));
            self.0.borrow_mut().remove(&key);
            Box::pin(async { Ok(()) })
        }
    }

    fn ready<T>(f: impl std::future::Future<Output = T>) -> T {
        let mut f = std::pin::pin!(f);
        match f
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("synchronous"),
        }
    }

    const ROW: ReplyRow<'static> = ReplyRow {
        principal: "usr_1",
        operation: "runbook.create",
        key: "k",
        expires_at: 1,
        nonce: [7; 16],
    };

    /// CF12b review round 2 (finding 5): two writers of one key, with the
    /// same response in the same millisecond, store different objects, so
    /// the loser's `release_unrecorded` (its batch failed before the
    /// winner's row landed) deletes only its own.
    #[test]
    fn two_writers_of_one_key_never_share_an_object() {
        let org: OrganizationId = "org_00000000000000000000000001".parse().unwrap();
        let project: ProjectId = "prj_00000000000000000000000001".parse().unwrap();
        let large = StoredResponse {
            status: 201,
            body: json!({"source": "x".repeat(INLINE_BYTES)}),
            etag: None,
        };
        let winner = stored_reply(
            &large,
            &project,
            ReplyRow {
                nonce: [1; 16],
                ..ROW
            },
        )
        .unwrap();
        let loser = stored_reply(
            &large,
            &project,
            ReplyRow {
                nonce: [2; 16],
                ..ROW
            },
        )
        .unwrap();
        let (w, l) = (winner.blob.clone().unwrap(), loser.blob.clone().unwrap());
        assert_ne!(w.name, l.name);
        assert_ne!(winner.column, loser.column);
        let blobs = Blobs(RefCell::default());
        for object in [&w, &l] {
            blobs.0.borrow_mut().insert(
                (project.to_string(), format!("replay/{}", object.name)),
                object.bytes.clone(),
            );
        }
        ready(blobs.delete_reply(&org, &project, &l.name)).unwrap();
        assert_eq!(
            ready(read_reply(&winner.column, &org, Some(&blobs))),
            Ok(large)
        );
    }

    #[test]
    fn large_responses_are_kept_under_the_projects_replay_prefix() {
        let org: OrganizationId = "org_00000000000000000000000001".parse().unwrap();
        let project: ProjectId = "prj_00000000000000000000000001".parse().unwrap();
        let small = StoredResponse {
            status: 201,
            body: json!({"source": "x"}),
            etag: Some("\"draft-1\"".into()),
        };
        let kept = stored_reply(&small, &project, ROW).unwrap();
        assert_eq!(kept.blob, None);
        assert_eq!(ready(read_reply(&kept.column, &org, None)), Ok(small));

        let large = StoredResponse {
            status: 201,
            body: json!({"source": "\"".repeat(1_048_576)}),
            etag: None,
        };
        let kept = stored_reply(&large, &project, ROW).unwrap();
        assert!(kept.column.len() < 200, "{}", kept.column);
        assert!(kept.column.starts_with("replay:"), "{}", kept.column);
        let object = kept.blob.clone().unwrap();
        assert!(object.bytes.len() > 2_000_000);
        assert_eq!(object.project, project);
        // Unresolvable without the blobs, or before they hold it.
        assert_eq!(
            ready(read_reply(&kept.column, &org, None)),
            Err(ReplyError::NoBlobs)
        );
        let blobs = Blobs(RefCell::default());
        assert_eq!(
            ready(read_reply(&kept.column, &org, Some(&blobs))),
            Err(ReplyError::Missing)
        );
        let key = (project.to_string(), format!("replay/{}", object.name));
        blobs.0.borrow_mut().insert(key.clone(), object.bytes);
        assert_eq!(
            ready(read_reply(&kept.column, &org, Some(&blobs))),
            Ok(large)
        );
        // Altered bytes under the name are not the response.
        blobs.0.borrow_mut().insert(key, b"{}".to_vec());
        assert_eq!(
            ready(read_reply(&kept.column, &org, Some(&blobs))),
            Err(ReplyError::Missing)
        );
    }

    /// CF12b: the same response for two rows (or one key reused after its
    /// row expired) is two objects, so sweeping one row never takes the
    /// other's.
    #[test]
    fn each_row_names_its_own_object() {
        let project: ProjectId = "prj_00000000000000000000000001".parse().unwrap();
        let large = StoredResponse {
            status: 201,
            body: json!({"s": "a".repeat(INLINE_BYTES)}),
            etag: None,
        };
        let name = |row| {
            stored_reply(&large, &project, row)
                .unwrap()
                .blob
                .unwrap()
                .name
        };
        let first = name(ROW);
        assert_ne!(
            first,
            name(ReplyRow {
                key: "other",
                ..ROW
            })
        );
        assert_ne!(
            first,
            name(ReplyRow {
                expires_at: 2,
                ..ROW
            })
        );
        assert_eq!(first, name(ROW));
    }

    /// CF12 kept a large response in the project's shared blobs under its
    /// digest (`blob:`); such a row reads as before until it expires.
    #[test]
    fn a_response_cf12_kept_in_shared_blobs_still_reads() {
        let org: OrganizationId = "org_00000000000000000000000001".parse().unwrap();
        let project: ProjectId = "prj_00000000000000000000000001".parse().unwrap();
        let large = StoredResponse {
            status: 201,
            body: json!({"s": "a".repeat(INLINE_BYTES)}),
            etag: None,
        };
        let bytes = serde_json::to_vec(&large).unwrap();
        let digest = runbook_core::portable::sha256(&bytes);
        let column = format!("blob:{project}:{}", hex(&digest));
        let blobs = Blobs(RefCell::default());
        blobs.0.borrow_mut().insert(
            (project.to_string(), format!("shared/{}", hex(&digest))),
            bytes,
        );
        assert_eq!(ready(read_reply(&column, &org, Some(&blobs))), Ok(large));
    }
}
