//! CF6: monthly execution quotas (PRD §17, D25, D56).
//!
//! - **What counts.** A run counts toward its organization's month (UTC, by
//!   the store clock at its start) when its start commits. A start
//!   replayed under its idempotency key counts once; a start the store
//!   refuses (validation, `on_conflict: reject`, the quota itself) never
//!   counts; a run cancelled while queued is refunded, so in the end a run
//!   counts exactly when it was admitted (D25).
//! - **The limit.** `QuotaLimits::executions_monthly` caps the month: the
//!   start that would exceed it is refused with
//!   `InvalidBatch(`[`MONTHLY_CODE`]`)` (HTTP 429) and writes nothing.
//!   Running executions continue. Limits come from configuration; `None`
//!   (the self-hosted default) counts but never refuses.
//! - **Months roll over lazily.** Each month has its own counter, created by
//!   its first start, so nothing runs at midnight; old months stay for
//!   billing.
//! - **Running.** The running count (admitted, not finished) is derived from
//!   the runs themselves, never kept as a counter that could drift; it is
//!   reported, not enforced.
//!
//! Adapters reserve inside their start transaction and refund inside the
//! one that releases a waiter. On Cloudflare the counters are one OrgDO per
//! organization, so the reservation is idempotent per execution and
//! compensated (see the CF6 decisions).
use crate::StoreError;
use runbook_model::OrganizationId;
use serde::{Deserialize, Serialize};
use std::future::Future;

/// The problem code of a start over its organization's monthly quota.
pub const MONTHLY_CODE: &str = "limit.executions_monthly";

/// An organization's quota limits; `None` is unlimited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLimits {
    pub executions_monthly: Option<u32>,
}

/// One organization's usage in the store clock's current month.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgUsage {
    /// `YYYY-MM`, UTC.
    pub month: String,
    pub executions_started: u64,
    /// Admitted and not finished.
    pub running: u64,
    pub limits: QuotaLimits,
}

/// `YYYY-MM` (UTC) of an epoch-millisecond time.
pub fn month_of(ms: i64) -> String {
    let (year, month, _) = crate::archives::civil(crate::archives::day_of(ms));
    format!("{year:04}-{month:02}")
}

/// Whether a month already holding `started` executions has room for one
/// more under `limits`.
pub fn has_room(limits: &QuotaLimits, started: u64) -> bool {
    limits
        .executions_monthly
        .is_none_or(|cap| started < u64::from(cap))
}

/// Reads of an organization's usage (the reservation itself lives in each
/// adapter's start transaction).
pub trait QuotaStore {
    fn org_usage(&self, org: &OrganizationId)
    -> impl Future<Output = Result<OrgUsage, StoreError>>;
}

/// Read a count limit from a variable's value: empty or `off` is none,
/// otherwise a positive integer.
pub fn parse_limit(value: Option<&str>) -> Result<Option<u32>, &'static str> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some("off") => Ok(None),
        Some(v) => v
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or("config.invalid_quota"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_are_utc_calendar_months() {
        assert_eq!(month_of(0), "1970-01");
        // 2026-01-31T23:59:59.999Z and the next millisecond.
        assert_eq!(month_of(1_769_903_999_999), "2026-01");
        assert_eq!(month_of(1_769_904_000_000), "2026-02");
        assert_eq!(month_of(1_700_000_000_000), "2023-11");
    }

    #[test]
    fn limits_allow_up_to_the_cap() {
        let two = QuotaLimits {
            executions_monthly: Some(2),
        };
        assert!(has_room(&two, 1));
        assert!(!has_room(&two, 2));
        assert!(has_room(&QuotaLimits::default(), u64::MAX));
        assert_eq!(parse_limit(None), Ok(None));
        assert_eq!(parse_limit(Some("off")), Ok(None));
        assert_eq!(parse_limit(Some("1000")), Ok(Some(1000)));
        assert!(parse_limit(Some("0")).is_err());
        assert!(parse_limit(Some("x")).is_err());
    }
}
