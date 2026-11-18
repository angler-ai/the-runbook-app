//! Webhook target rules (SSRF). Syntax and literal-address checks are pure;
//! hosts resolve names and must apply [`forbidden_address`] to every
//! resolved address on every delivery, then connect only to those vetted
//! addresses (DNS-rebinding defense). Self-hosted operators may allow
//! private targets, which also permits plain http; link-local targets
//! (cloud metadata at 169.254.169.254, fe80::/10) need a separate flag.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const URL_MAX_BYTES: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebhookTarget {
    pub https: bool,
    /// Lowercased host without IPv6 brackets.
    pub host: String,
    pub port: u16,
    /// Set when the host is an address literal.
    pub literal: Option<IpAddr>,
}

/// Which non-public targets an operator allows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TargetPolicy {
    /// Self-hosted: private, loopback and plain-http targets.
    pub allow_private: bool,
    /// Separately: link-local targets (169.254.0.0/16, fe80::/10).
    pub allow_link_local: bool,
}

/// Whether a resolved or literal address may be contacted under `policy`.
pub fn address_allowed(ip: IpAddr, policy: TargetPolicy) -> bool {
    if link_local_address(ip) {
        return policy.allow_link_local && policy.allow_private;
    }
    policy.allow_private || !forbidden_address(ip)
}

/// 169.254.0.0/16 and fe80::/10, including IPv4-mapped forms.
pub fn link_local_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.is_link_local(),
            None => (v6.segments()[0] & 0xffc0) == 0xfe80,
        },
    }
}

/// Parse and police a subscription URL. Error codes are stable problem codes.
pub fn parse_target(url: &str, policy: TargetPolicy) -> Result<WebhookTarget, &'static str> {
    let allow_private = policy.allow_private;
    if url.is_empty()
        || url.len() > URL_MAX_BYTES
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("webhooks.url_invalid");
    }
    let uri = fluent_uri::Uri::parse(url).map_err(|_| "webhooks.url_invalid")?;
    let scheme = uri.scheme().as_str().to_ascii_lowercase();
    let https = match scheme.as_str() {
        "https" => true,
        "http" if allow_private => false,
        "http" => return Err("webhooks.url_https_required"),
        _ => return Err("webhooks.url_invalid"),
    };
    if uri.fragment().is_some() {
        return Err("webhooks.url_invalid");
    }
    let authority = uri.authority().ok_or("webhooks.url_invalid")?;
    if authority.userinfo().is_some() {
        return Err("webhooks.url_credentials");
    }
    let raw_host = authority.host();
    if raw_host.is_empty() {
        return Err("webhooks.url_invalid");
    }
    let port = match authority.port_to_u16() {
        Ok(Some(0)) | Err(_) => return Err("webhooks.url_invalid"),
        Ok(Some(port)) => port,
        Ok(None) if https => 443,
        Ok(None) => 80,
    };
    let host = raw_host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(raw_host)
        .to_ascii_lowercase();
    let literal = host.parse::<IpAddr>().ok();
    if raw_host.starts_with('[') && literal.is_none() {
        return Err("webhooks.url_invalid");
    }
    // Gate fix B3: a WHATWG URL parser (fetch on Workers) reads a host
    // whose last label is numeric as an IPv4 address (`2130706433`,
    // `0x7f.1`, `127.1`, `127.0.0.1.`) and decodes percent-escapes, so such
    // a host could reach an address the literal check never saw.
    if literal.is_none() && (host.contains('%') || numeric_last_label(&host)) {
        return Err("webhooks.url_invalid");
    }
    let local_name = host == "localhost" || host.ends_with(".localhost");
    if (local_name && !allow_private) || literal.is_some_and(|ip| !address_allowed(ip, policy)) {
        return Err("webhooks.target_forbidden");
    }
    Ok(WebhookTarget {
        https,
        host,
        port,
        literal,
    })
}

/// Whether a host's last label (one trailing dot ignored) is a number, in
/// decimal or `0x` hex: the WHATWG URL parser then reads the host as IPv4.
fn numeric_last_label(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    let last = host.rsplit('.').next().unwrap_or("");
    if let Some(hex) = last.strip_prefix("0x") {
        return hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    !last.is_empty() && last.chars().all(|c| c.is_ascii_digit())
}

/// Private, loopback, link-local, unspecified, multicast, broadcast and
/// other non-public ranges. IPv4-mapped and -compatible IPv6 addresses are
/// judged by their IPv4 address.
pub fn forbidden_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => forbidden_v6(v6),
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // shared address space (CGNAT)
        || (a == 192 && b == 0 && ip.octets()[2] == 0) // IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || a >= 240 // reserved
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_v4(v4);
    }
    let segments = ip.segments();
    // Deprecated IPv4-compatible ::a.b.c.d (but not :: or ::1, handled below).
    if segments[..6] == [0; 6] && !ip.is_unspecified() && !ip.is_loopback() {
        return forbidden_v4(Ipv4Addr::from_bits(ip.to_bits() as u32));
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00 // unique local
        || (segments[0] & 0xffc0) == 0xfe80 // link-local
        || (segments[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
        || (segments[0] == 0x64 && segments[1] == 0xff9b) // NAT64 can reach private v4
        || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
        || segments[0] == 0x2002 // 6to4 embeds an arbitrary IPv4 address
        || (segments[0] == 0x2001 && segments[1] == 0) // Teredo
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_are_classified() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::1",
            "2001:0:4136:e378::1",
        ] {
            assert!(forbidden_address(bad.parse().unwrap()), "{bad}");
        }
        for good in ["93.184.216.34", "8.8.8.8", "2606:4700::1111"] {
            assert!(!forbidden_address(good.parse().unwrap()), "{good}");
        }
    }

    /// Gate fix B3: hosts a WHATWG URL parser (fetch on Workers, browsers)
    /// reads as an IPv4 address but `IpAddr` does not: decimal, hex,
    /// octal and short forms, and percent-encoded hosts. They are refused
    /// on every edition, whatever the policy; real names still pass.
    #[test]
    fn hosts_a_url_parser_reads_as_addresses_are_refused() {
        let strict = TargetPolicy::default();
        for url in [
            "https://2130706433/",
            "https://0x7f.1/",
            "https://0X7F000001/",
            "https://127.1/",
            "https://0177.0.0.1/",
            "https://127.0.0.1./",
            "https://10.0.0.1./hook",
            "https://%31%32%37.0.0.1/",
            "https://example%2ecom/",
            "https://host.0x10/",
            "https://example.123/",
        ] {
            assert_eq!(
                parse_target(url, strict).map(|_| ()),
                Err("webhooks.url_invalid"),
                "{url}"
            );
            assert_eq!(
                parse_target(
                    url,
                    TargetPolicy {
                        allow_private: true,
                        allow_link_local: false
                    }
                )
                .map(|_| ()),
                Err("webhooks.url_invalid"),
                "{url} (private allowed)"
            );
        }
        for url in [
            "https://hooks.example.com/runbook",
            "https://8.8.8.8/x",
            "https://[2606:4700::1111]/x",
            "https://a1.example/",
            "https://123.example/",
            "https://example.com./",
        ] {
            assert!(parse_target(url, strict).is_ok(), "{url}");
        }
        assert_eq!(
            parse_target("https://127.0.0.1/", strict).map(|_| ()),
            Err("webhooks.target_forbidden")
        );
    }
}
