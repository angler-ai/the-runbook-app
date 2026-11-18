//! Daily archive index (PRD §14 "Retention and daily archives"; D53–D54).
//! T0 froze the index records and trait; ARC1 adds the builder
//! ([`build`]), the range reader ([`read`]) and the import conversion
//! ([`import`]); ARC2 adds `0029_archives_retention` and the daily job
//! ([`job`]).
//!
//! The ZIP is an immutable blob under `BlobKey::Archive { runbook_id, day,
//! digest }`, one key per content; the index row names the digest of the
//! current file, so readers always follow the index. A rewrite puts the new
//! file under its own key, then [`ArchiveIndex::record_archive`] swings the
//! row to it, conditional on the previous digest, and only then is the old
//! file deleted. Days are UTC day numbers since the Unix epoch, the same
//! half-open interval as `RunStore::finished_on`.
//!
//! Stale marks live in the stores, inside the transaction that makes the file
//! out of date, so a crash can never leave a changed run behind an unmarked
//! file:
//! - a commit that appends events to a run listed in an archive (an
//!   amendment) marks that file stale;
//! - finalizing evidence on a listed run marks it stale (the manifest lists
//!   evidence);
//! - a deletion whose reason is not [`RETENTION_REASON`] (a single-run
//!   deletion, a privacy request) marks the file stale and drops the run's
//!   summary row, so the next rewrite leaves the run out; for a run retention
//!   already deleted, [`ArchiveIndex::purge_retained_run`] (or the same
//!   deletion call) does the same;
//! - retention's own deletion keeps the summary row and the run's uploaded
//!   evidence (PRD §14: "when the live window ends, only the live copy is
//!   deleted"), and is refused unless the run's file is current.
//!
//! Redaction happens before anything reaches a store (D34: logs at write),
//! so an archive built from stored rows never holds a redacted value. There
//! is no later-redaction operation yet; one would be a commit to the run and
//! mark its file stale like an amendment.
use crate::{StoreError, catalog::TenantScope};
use runbook_model::{ExecutionId, ExecutionStatus, RunbookId, VersionId};
use serde::{Deserialize, Serialize};
use std::{future::Future, num::NonZeroU32};

pub mod build;
pub mod bundle;
pub mod import;
pub mod job;
pub mod read;
pub mod zip;

/// D53: the per-day archive layout's schema version.
pub const ARCHIVE_SCHEMA_VERSION: u32 = 1;
/// `manifest.json`'s `format`.
pub const DAILY_FORMAT: &str = "runbook-daily-archive";
/// One UTC day in milliseconds.
pub const DAY_MS: i64 = 86_400_000;
/// The tombstone reason retention's live deletion uses. Any other reason is
/// a single-run deletion, which takes the run out of its archive too.
pub const RETENTION_REASON: &str = "retention.live_expired";
/// A stored version of a day's file the index does not name and that is
/// older than this is a leftover (a write takes seconds); younger ones may
/// be another writer's in-flight file and are never touched.
pub const VERSION_GRACE_MS: i64 = 3_600_000;

/// One runbook's ZIP for one UTC day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRecord {
    pub scope: TenantScope,
    pub runbook_id: RunbookId,
    pub day: i64,
    pub run_count: u32,
    pub size_bytes: u64,
    /// SHA-256 of the ZIP bytes (deterministic per D53).
    pub digest: [u8; 32],
    pub schema_version: u32,
    pub written_at_ms: i64,
    /// An amendment, redaction or single-run deletion after the write marks it
    /// stale; the next daily job rewrites it.
    pub stale: bool,
    /// Retention deadline for the file; `None` keeps it indefinitely (D54).
    pub expires_at_ms: Option<i64>,
    /// How many stale marks the file's row has seen. A writer passes the
    /// count it read; if a mark landed since, the new file is recorded stale
    /// (a record never clears a concurrent mark).
    #[serde(default)]
    pub stale_marks: u64,
}

/// The PRD §14 summary row: what lists need once live detail is gone
/// (runbook@version, status, who started it, when, duration).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedRunSummary {
    pub version_id: VersionId,
    pub version: Option<NonZeroU32>,
    pub status: ExecutionStatus,
    pub title: String,
    pub created_by: String,
    pub created_at_ms: i64,
    pub completed_at_ms: i64,
}

/// Where one run lives inside its day's file. It outlives live detail, so an
/// archived run opens read-only through a range read of just this folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedRun {
    pub execution_id: ExecutionId,
    pub runbook_id: RunbookId,
    pub day: i64,
    /// Folder prefix inside the ZIP, `<exe>/`.
    pub folder: String,
    /// The run's event sequence the file holds. A run committed past it
    /// while the file was being written is recorded stale (see
    /// [`ArchiveIndex::record_archive`]).
    pub revision: u64,
    /// ARC2: the summary row's content, with the archive location above.
    pub summary: ArchivedRunSummary,
    /// The finalized evidence the file lists for the run; a record whose
    /// list differs from the run's finalized evidence is stale.
    #[serde(default)]
    pub evidence: Vec<runbook_model::EvidenceId>,
    /// The writer built this folder from the live run. If retention deleted
    /// the run before the record, the record refuses (`archive.conflict`):
    /// the writer starts over and copies the stored folder instead.
    #[serde(default)]
    pub gathered_live: bool,
}

/// Tenant-scoped archive index. Every call filters by `scope`; a foreign file
/// or run reads as absent. Implementations keep each call atomic; futures
/// need not be Send.
pub trait ArchiveIndex {
    /// Point the day's index row at the file whose digest `record` names and
    /// replace its run list, in one step, only if the row's current digest
    /// is `expected` (`None`: no row yet); otherwise
    /// `InvalidBatch("archive.conflict")` and nothing changes. The named
    /// file must be stored (`InvalidBatch("archive.blob_missing")`). Clears
    /// the day's failure backoff, and `stale` unless a mark landed since the
    /// writer read the row (`record.stale_marks`) or a listed run changed.
    /// In the same transaction, under the row's lock, deletes the day's
    /// other stored versions that are older than the named one or than
    /// [`VERSION_GRACE_MS`]: never a younger in-flight write.
    ///
    /// The file's members (every listed run, whether or not it keeps a
    /// summary row) are recorded until the file is deleted, so later stale
    /// marks and single-run deletions always find the file.
    ///
    /// A listed run must still be live or deleted by retention; anything else
    /// was deleted meanwhile and is `InvalidBatch("archive.run_gone")`. A
    /// live run whose revision moved past the listed one (an amendment
    /// committed while the file was built) is recorded, but the row stays
    /// stale so the next pass rewrites it. A run retention deleted keeps its
    /// summary row only if it still has one (a purged summary is not
    /// restored). Implementations hold the listed live runs against
    /// concurrent commits and deletions until the record commits, so a stale
    /// mark is never lost.
    fn record_archive(
        &self,
        scope: &TenantScope,
        record: ArchiveRecord,
        runs: Vec<ArchivedRun>,
        expected: Option<[u8; 32]>,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// True when the file existed and was not already stale.
    fn mark_archive_stale(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn get_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> impl Future<Output = Result<Option<ArchiveRecord>, StoreError>>;
    /// Newest day first, within the inclusive `from_day..=to_day` bounds.
    fn list_archives(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        from_day: Option<i64>,
        to_day: Option<i64>,
    ) -> impl Future<Output = Result<Vec<ArchiveRecord>, StoreError>>;
    /// A run's summary row. It outlives the file (`summary_days` may exceed
    /// `archive_days`); only [`ArchiveIndex::purge_summaries`] and a
    /// single-run deletion remove it.
    fn archived_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> impl Future<Output = Result<Option<ArchivedRun>, StoreError>>;
    /// Up to `limit` stale files not backing off at `now_ms`, oldest day
    /// first, for the daily job.
    fn stale_archives(
        &self,
        scope: &TenantScope,
        now_ms: i64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ArchiveRecord>, StoreError>>;
    /// Delete the day's file: the index row, every stored version, its
    /// members, and the uploads (and their counted bytes) of member runs
    /// retention deleted, found by key under each run, in one transaction.
    /// Summary rows stay. Under the row's lock the row must still name
    /// `expected` (`InvalidBatch("archive.conflict")`) and no member run may
    /// be live (`InvalidBatch("archive.member_live")`). False when absent.
    fn delete_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        expected: [u8; 32],
    ) -> impl Future<Output = Result<bool, StoreError>>;
    /// The runs a day's file lists (its members), whether or not they keep a
    /// summary row. Empty when the day has no file.
    fn file_members(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> impl Future<Output = Result<Vec<ExecutionId>, StoreError>>;
    /// Time-driven cleanup: for up to `limit` days that hold a stored
    /// version the index does not name and that is older than
    /// [`VERSION_GRACE_MS`] (store clock), sweep the day under its row's
    /// lock. A losing write's leftover goes even if nothing revisits the
    /// day. Returns how many versions went.
    fn sweep_leftover_versions(
        &self,
        scope: &TenantScope,
        limit: usize,
    ) -> impl Future<Output = Result<usize, StoreError>>;
    /// ARC2: up to `limit` (runbook, day) pairs before `before_day` that hold
    /// a finished live run no file lists yet and are not backing off at
    /// `now_ms`, oldest day first.
    fn unarchived_days(
        &self,
        scope: &TenantScope,
        before_day: i64,
        now_ms: i64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<(RunbookId, i64)>, StoreError>>;
    /// ARC2: up to `limit` finished live runs that completed before
    /// `completed_before_ms` and are listed in a file that is not stale,
    /// oldest first: the runs retention may delete. The deletion itself
    /// rechecks this under the run's lock (D54: only after the archive
    /// exists, and never while a rewrite is owed).
    fn archived_live_runs(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ExecutionId>, StoreError>>;
    /// ARC2: up to `limit` files of days before `before_day`, oldest first.
    /// The job derives the cutoff from the current policy, so a changed
    /// `RUNBOOK_RETENTION_ARCHIVE_DAYS` applies to files already written.
    fn expired_archives(
        &self,
        scope: &TenantScope,
        before_day: i64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ArchiveRecord>, StoreError>>;
    /// ARC2: drop up to `limit` summary rows of runs that completed before
    /// `completed_before_ms` and are no longer live. Returns how many went.
    fn purge_summaries(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> impl Future<Output = Result<usize, StoreError>>;
    /// A write of this day failed: back it off (attempt count and next
    /// attempt time) so later days still progress. Returns the attempt count.
    fn archive_failed(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        now_ms: i64,
    ) -> impl Future<Output = Result<u32, StoreError>>;
    /// Delete the day's stored versions the index does not name that are
    /// older than the named one or than [`VERSION_GRACE_MS`] (leftovers of
    /// an interrupted or losing write), under the index row's lock.
    fn sweep_archive_versions(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// A single-run deletion of a run retention already deleted: in one
    /// transaction, set the tombstone's reason to `reason`, drop the summary
    /// row, mark its file stale (found through its membership, even after
    /// the summary row was purged) and delete its retained evidence objects
    /// and their counted bytes. `delete_execution` with a non-retention
    /// reason on a retained run does the same.
    /// `None` when the run has no retention tombstone in this tenant.
    fn purge_retained_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
        reason: &str,
    ) -> impl Future<Output = Result<Option<crate::retention::Tombstone>, StoreError>>;
}

/// The day's backoff after `attempts` failures: one minute doubling, at most
/// a day.
pub fn backoff_ms(attempts: u32) -> i64 {
    (60_000i64 << attempts.saturating_sub(1).min(11)).min(DAY_MS)
}

/// UTC day number of an epoch-millisecond time.
pub fn day_of(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS)
}

/// `YYYY-MM-DD` for a UTC day number (Howard Hinnant's civil_from_days).
pub fn date_of(day: i64) -> String {
    let (year, month, d) = civil(day);
    format!("{year:04}-{month:02}-{d:02}")
}

pub(crate) fn civil(day: i64) -> (i64, i64, i64) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, d)
}

/// The UTC day number of a strict `YYYY-MM-DD` date (years 1970–9999), or
/// `None` for anything else, including impossible dates like `2026-02-30`.
pub fn parse_date(date: &str) -> Option<i64> {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        date.get(r.clone())
            .filter(|s| s.bytes().all(|c| c.is_ascii_digit()))?
            .parse()
            .ok()
    };
    let (year, month, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    if year < 1970 || !(1..=12).contains(&month) || d < 1 {
        return None;
    }
    // days_from_civil
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let day = era * 146_097 + doe - 719_468;
    (civil(day) == (year, month, d)).then_some(day)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates_round_trip_and_reject_impossible_days() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(date_of(20_404), "2025-11-12");
        for day in [0, 59, 60, 365, 11_016, 19_782, 20_404, 2_932_896] {
            assert_eq!(parse_date(&date_of(day)), Some(day), "{day}");
        }
        for bad in [
            "2026-02-30",
            "2026-13-01",
            "2026-00-10",
            "2026-1-01",
            "20260101",
            "1969-12-31",
            "２０２６-01-01",
            "2026-01-0a",
            "+026-01-01",
        ] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
        assert_eq!(parse_date("2024-02-29"), Some(19_782));
        assert_eq!(day_of(-1), -1);
        assert_eq!(day_of(DAY_MS), 1);
    }
}
