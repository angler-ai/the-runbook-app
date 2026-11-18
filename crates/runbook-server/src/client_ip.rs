//! Sign-in gate fix 1: the client address behind a reverse proxy.
//!
//! `RUNBOOK_TRUSTED_PROXIES` is a comma list of addresses or CIDR blocks
//! (`10.0.0.0/8, 172.16.0.0/12, ::1`); empty or unset trusts none. When the
//! TCP peer is trusted, the client is the right-most `X-Forwarded-For` hop
//! that is not itself a trusted proxy (or, without `X-Forwarded-For`, the
//! right-most such `for=` of `Forwarded`, RFC 7239). Otherwise those
//! headers are ignored and the client is the peer. Every per-IP limit and
//! the `x-runbook-client-ip` the auth child sees use this address.
//! `X-Real-IP` and `CF-Connecting-IP` are never read: the native edition
//! does not know which proxy set them (the Cloudflare edition reads
//! `cf-connecting-ip`, which Cloudflare itself sets).
use http::HeaderMap;
use std::net::{IpAddr, Ipv6Addr};

/// The trusted proxies: `(network, prefix length)` pairs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<(IpAddr, u8)>);

/// IPv4-mapped IPv6 (`::ffff:a.b.c.d`, from a dual-stack socket) as IPv4.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn masked(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V4((bits & mask).into())
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V6((bits & mask).into())
        }
    }
}

impl TrustedProxies {
    /// Parse `RUNBOOK_TRUSTED_PROXIES`. `None` for any malformed entry.
    pub fn parse(value: &str) -> Option<Self> {
        let mut out = Vec::new();
        for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (ip, prefix) = match entry.split_once('/') {
                Some((ip, prefix)) => (ip, Some(prefix)),
                None => (entry, None),
            };
            let ip = canonical(ip.parse::<IpAddr>().ok()?);
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let prefix = match prefix {
                Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                    p.parse::<u8>().ok().filter(|p| *p <= max)?
                }
                Some(_) => return None,
                None => max,
            };
            out.push((masked(ip, prefix), prefix));
        }
        Some(Self(out))
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.0
            .iter()
            .any(|(net, prefix)| net.is_ipv4() == ip.is_ipv4() && masked(ip, *prefix) == *net)
    }
    /// The client address for a request from `peer` (see the module docs).
    pub fn client_ip(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
        let peer = canonical(peer?);
        if !self.contains(peer) {
            return Some(peer);
        }
        let hops = forwarded_hops(headers);
        // Right to left: the first hop no trusted proxy accounts for. An
        // unreadable hop (`unknown`, an obfuscated name) ends the walk at
        // the trusted proxy that reported it.
        let mut client = peer;
        for hop in hops.iter().rev() {
            let Some(ip) = hop else {
                return Some(client);
            };
            client = *ip;
            if !self.contains(*ip) {
                return Some(*ip);
            }
        }
        Some(client)
    }
}

/// The forwarded-for chain, left (origin) to right (nearest), each hop
/// parsed or `None`. `X-Forwarded-For` wins over `Forwarded` when present.
fn forwarded_hops(headers: &HeaderMap) -> Vec<Option<IpAddr>> {
    let values = |name: &str| -> Vec<String> {
        headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().map(str::to_owned).unwrap_or_default())
            .collect()
    };
    let xff = values("x-forwarded-for");
    if !xff.is_empty() {
        return xff
            .iter()
            .flat_map(|v| v.split(','))
            .map(|hop| parse_hop(hop.trim()))
            .collect();
    }
    values("forwarded")
        .iter()
        .flat_map(|v| v.split(','))
        .map(|element| {
            element
                .split(';')
                .filter_map(|pair| pair.trim().split_once('='))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("for"))
                .and_then(|(_, v)| parse_hop(v.trim().trim_matches('"')))
        })
        .collect()
}

/// One hop: an address, `ip:port`, or `[v6]` / `[v6]:port`.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    if let Ok(ip) = hop.parse::<IpAddr>() {
        return Some(canonical(ip));
    }
    if let Ok(socket) = hop.parse::<std::net::SocketAddr>() {
        return Some(canonical(socket.ip()));
    }
    let inner = hop.strip_prefix('[')?.split(']').next()?;
    inner
        .parse::<Ipv6Addr>()
        .ok()
        .map(|v6| canonical(v6.into()))
}

/// The rate-limit key of a client address: an IPv6 client controls a whole
/// /64, so it is keyed by that prefix (as the Cloudflare edition does).
pub fn rate_key(ip: Option<IpAddr>) -> String {
    match ip.map(canonical) {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(ip @ IpAddr::V6(_)) => format!("{}/64", masked(ip, 64)),
        None => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(
                http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        map
    }
    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn parses_addresses_and_blocks() {
        let t = TrustedProxies::parse(" 10.0.0.0/8, 192.0.2.1 ,::1, 2001:db8::/32,").unwrap();
        assert!(t.contains("10.200.3.4".parse().unwrap()));
        assert!(t.contains("192.0.2.1".parse().unwrap()));
        assert!(!t.contains("192.0.2.2".parse().unwrap()));
        assert!(t.contains("::1".parse().unwrap()));
        assert!(t.contains("2001:db8:ffff::9".parse().unwrap()));
        assert!(t.contains("::ffff:10.1.1.1".parse().unwrap()));
        assert!(!t.contains("11.0.0.1".parse().unwrap()));
        assert!(TrustedProxies::parse("").unwrap().is_empty());
        for bad in [
            "10.0.0.0/33",
            "::/129",
            "proxy.local",
            "10.0.0.0/",
            "1.2.3.4/x",
        ] {
            assert_eq!(TrustedProxies::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_trusted_proxy_names_each_client() {
        let t = TrustedProxies::parse("10.0.0.0/8").unwrap();
        let peer = ip("10.0.0.5");
        assert_eq!(
            t.client_ip(peer, &headers(&[("x-forwarded-for", "198.51.100.7")])),
            ip("198.51.100.7")
        );
        assert_eq!(
            t.client_ip(peer, &headers(&[("x-forwarded-for", "198.51.100.8")])),
            ip("198.51.100.8")
        );
        // The client may prepend anything; the right-most untrusted hop wins.
        assert_eq!(
            t.client_ip(
                peer,
                &headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.7, 10.0.0.9")])
            ),
            ip("198.51.100.7")
        );
        // Several header lines are one list, in order.
        assert_eq!(
            t.client_ip(
                peer,
                &headers(&[
                    ("x-forwarded-for", "1.2.3.4"),
                    ("x-forwarded-for", "198.51.100.9")
                ])
            ),
            ip("198.51.100.9")
        );
        // RFC 7239 when there is no X-Forwarded-For.
        assert_eq!(
            t.client_ip(
                peer,
                &headers(&[(
                    "forwarded",
                    r#"for=1.2.3.4, For="[2001:db8::7]:4711";proto=https"#
                )])
            ),
            ip("2001:db8::7")
        );
        // An unreadable hop stops at the proxy that reported it.
        assert_eq!(
            t.client_ip(peer, &headers(&[("forwarded", "for=unknown")])),
            peer
        );
        assert_eq!(
            t.client_ip(peer, &headers(&[("x-forwarded-for", "1.2.3.4, _hidden")])),
            peer
        );
        // Every hop trusted: the left-most is the best we know.
        assert_eq!(
            t.client_ip(peer, &headers(&[("x-forwarded-for", "10.9.9.9, 10.0.0.1")])),
            ip("10.9.9.9")
        );
        // No header: the proxy itself.
        assert_eq!(t.client_ip(peer, &headers(&[])), peer);
        assert_eq!(
            t.client_ip(None, &headers(&[("x-forwarded-for", "1.2.3.4")])),
            None
        );
    }

    #[test]
    fn an_untrusted_peer_cannot_claim_an_address() {
        for t in [
            TrustedProxies::default(),
            TrustedProxies::parse("10.0.0.0/8").unwrap(),
        ] {
            let peer = ip("203.0.113.5");
            for (k, v) in [
                ("x-forwarded-for", "198.51.100.7"),
                ("forwarded", "for=198.51.100.7"),
                ("x-real-ip", "198.51.100.7"),
                ("cf-connecting-ip", "198.51.100.7"),
            ] {
                assert_eq!(t.client_ip(peer, &headers(&[(k, v)])), peer, "{k}");
            }
        }
        // A trusted proxy's X-Real-IP / CF-Connecting-IP are never read.
        let t = TrustedProxies::parse("10.0.0.0/8").unwrap();
        assert_eq!(
            t.client_ip(
                ip("10.0.0.5"),
                &headers(&[
                    ("x-real-ip", "198.51.100.7"),
                    ("cf-connecting-ip", "198.51.100.7")
                ])
            ),
            ip("10.0.0.5")
        );
        // A dual-stack socket's mapped address is the IPv4 one.
        assert_eq!(
            TrustedProxies::default().client_ip(ip("::ffff:203.0.113.5"), &headers(&[])),
            ip("203.0.113.5")
        );
    }

    #[test]
    fn ipv6_clients_share_a_64() {
        assert_eq!(rate_key(ip("203.0.113.5")), "203.0.113.5");
        assert_eq!(rate_key(ip("2001:db8:1:2:3:4:5:6")), "2001:db8:1:2::/64");
        assert_eq!(rate_key(ip("::ffff:203.0.113.5")), "203.0.113.5");
        assert_eq!(rate_key(None), "unknown");
    }
}
