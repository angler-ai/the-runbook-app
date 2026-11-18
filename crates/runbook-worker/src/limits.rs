//! Gate fix B2: the AUTH3 sliding-window limits. Each api Worker request
//! builds its own backends, so windows kept per request never filled. On
//! Workers the windows live in a Durable Object per key (`LimitsDO`, edge),
//! which every isolate shares; [`MemoryLimits`] keeps them in memory for
//! native test hosts.
use std::{cell::RefCell, collections::BTreeMap, future::Future, rc::Rc};

/// Where a host keeps its rate-limit windows.
pub trait RateLimits: Clone + 'static {
    /// Count one hit for `key`, or `Err(retry_after_seconds)` once `limit`
    /// hits were already counted in the last `window_ms`.
    fn hit(
        &self,
        key: &str,
        window_ms: u64,
        limit: u32,
        now_ms: i64,
    ) -> impl Future<Output = Result<(), u64>>;
}

/// One key's window: drop hits older than `window_ms`, then count this one
/// unless `limit` remain.
pub fn slide(hits: &mut Vec<i64>, window_ms: u64, limit: u32, now_ms: i64) -> Result<(), u64> {
    let window = i64::try_from(window_ms).unwrap_or(i64::MAX);
    hits.retain(|at| now_ms - *at < window);
    if hits.len() >= limit as usize {
        let oldest = hits.first().copied().unwrap_or(now_ms);
        let wait = (oldest.saturating_add(window) - now_ms).max(0);
        return Err(u64::try_from(wait).unwrap_or(0).div_ceil(1000).max(1));
    }
    hits.push(now_ms);
    Ok(())
}

/// Gate round 2: the rate-limit key of a client address. An IPv6 client
/// controls a whole /64, so it is keyed by that prefix; an IPv4 address
/// (or an IPv4-mapped one) by itself; anything else is `unknown`.
pub fn client_key(address: &str) -> String {
    match address.trim().parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.to_string(),
        Ok(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                let prefix = std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0);
                format!("{prefix}/64")
            }
        },
        Err(_) => "unknown".to_owned(),
    }
}

/// Windows in memory; clones share them.
#[derive(Clone, Default)]
pub struct MemoryLimits(Rc<RefCell<BTreeMap<String, Vec<i64>>>>);
impl RateLimits for MemoryLimits {
    async fn hit(&self, key: &str, window_ms: u64, limit: u32, now_ms: i64) -> Result<(), u64> {
        slide(
            self.0.borrow_mut().entry(key.to_owned()).or_default(),
            window_ms,
            limit,
            now_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gate round 2: one IPv6 client owns a whole /64, so its limit is the
    /// /64's; an IPv4 address is its own key.
    #[test]
    fn ipv6_clients_are_keyed_by_their_slash_64() {
        assert_eq!(client_key("203.0.113.7"), "203.0.113.7");
        assert_eq!(
            client_key("2001:db8:1:2:aaaa:bbbb:cccc:dddd"),
            client_key("2001:db8:1:2::1")
        );
        assert_eq!(client_key("2001:db8:1:2::1"), "2001:db8:1:2::/64");
        assert_ne!(client_key("2001:db8:1:3::1"), client_key("2001:db8:1:2::1"));
        // An IPv4-mapped address is its IPv4 address.
        assert_eq!(client_key("::ffff:203.0.113.7"), "203.0.113.7");
        assert_eq!(client_key("not an address"), "unknown");
    }

    #[test]
    fn the_window_slides_and_tells_how_long_to_wait() {
        let mut hits = vec![];
        for at in [0, 10, 20] {
            assert_eq!(slide(&mut hits, 60_000, 3, at), Ok(()));
        }
        assert_eq!(slide(&mut hits, 60_000, 3, 30), Err(60));
        assert_eq!(slide(&mut hits, 60_000, 3, 59_999), Err(1));
        assert_eq!(
            slide(&mut hits, 60_000, 3, 60_000),
            Ok(()),
            "the first hit left"
        );
    }
}
