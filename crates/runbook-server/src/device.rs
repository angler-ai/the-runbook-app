//! AUTH3 (D47) on this host: the per-IP limit of `POST /v1/device/code`,
//! keyed on the transport side where the client address is known (the TCP
//! peer, or the client a trusted proxy names: [`crate::client_ip`]) and
//! counted by the worker in the shared Postgres windows (HOST1), like the
//! portable routes' other limits (`runbook_api::device`).
//!
//! HOST1 review: a key this instance saw refused is refused again from
//! memory until its window frees a slot ([`Refused`]), so a flood of
//! refused requests costs no database work. Only when the database cannot
//! be reached at all does an instance count in its own windows
//! ([`Limits`]); a busy pool or a failing query refuses (503).
pub use runbook_api::device::{CODE, CODES_PER_HOUR, TOKEN};
use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

/// Sliding-window counters in this instance's memory: the fallback while
/// the shared windows in Postgres are unreachable.
#[derive(Default)]
pub struct Limits {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}
impl Limits {
    /// Count one hit for `key`; `Err(retry_after_seconds)` once `limit`
    /// hits were already counted in the last `window`.
    pub fn hit(&self, key: &str, window: Duration, limit: u32) -> Result<(), u64> {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        hits.retain(|_, times| {
            times.retain(|t| now.duration_since(*t) < window);
            !times.is_empty()
        });
        let times = hits.entry(key.to_owned()).or_default();
        if times.len() >= limit as usize {
            let oldest = times.first().copied().unwrap_or(now);
            return Err((window.saturating_sub(now.duration_since(oldest)))
                .as_secs()
                .max(1));
        }
        times.push(now);
        Ok(())
    }
}

/// Keys refused by the shared windows, until the moment their window
/// frees a slot. At most [`Refused::CAPACITY`] keys are remembered.
#[derive(Default)]
pub struct Refused {
    until: Mutex<HashMap<String, Instant>>,
}
impl Refused {
    pub const CAPACITY: usize = 10_000;
    /// Seconds until `key` may be counted again, if it is known refused.
    pub fn check(&self, key: &str) -> Option<u64> {
        let now = Instant::now();
        let until = *self
            .until
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)?;
        (until > now).then(|| until.duration_since(now).as_secs_f64().ceil() as u64)
    }
    /// Remember that `key` stays refused for `retry_after` seconds.
    pub fn remember(&self, key: &str, retry_after: u64) {
        let now = Instant::now();
        let mut until = self.until.lock().unwrap_or_else(PoisonError::into_inner);
        until.retain(|_, at| *at > now);
        if until.len() < Self::CAPACITY {
            until.insert(key.to_owned(), now + Duration::from_secs(retry_after));
        }
    }
}

/// The window of `POST /v1/device/code`: one hour.
pub const CODE_WINDOW_MS: u64 = 3_600_000;

/// The rate-limit key of a device-code request from `client`.
pub fn code_key(client: Option<std::net::IpAddr>) -> String {
    format!("code:{}", crate::client_ip::rate_key(client))
}
