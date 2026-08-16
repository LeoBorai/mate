//! Resolved-IP validation (§8.2, `M10-2`): the pure range checks every hop of the
//! `http_request` tool's manual redirect loop runs before it lets a connection through.
//! Kept as free functions over `IpAddr` — no network, no config — so they're exhaustively
//! table-testable and reusable from both the first request and every redirect hop.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use ipnet::{Ipv4Net, Ipv6Net};

/// Fixed IPv4 blocklist (§8.2): loopback, private (RFC 1918), link-local (cloud metadata
/// lives at `169.254.169.254`), unspecified, multicast, and CGNAT (RFC 6598).
static BLOCKED_V4: LazyLock<Vec<(Ipv4Net, &'static str)>> = LazyLock::new(|| {
    vec![
        (net4(0, 0, 0, 0, 32), "unspecified"),
        (net4(10, 0, 0, 0, 8), "private"),
        (net4(100, 64, 0, 0, 10), "cgnat"),
        (net4(127, 0, 0, 0, 8), "loopback"),
        (net4(169, 254, 0, 0, 16), "link-local"),
        (net4(172, 16, 0, 0, 12), "private"),
        (net4(192, 168, 0, 0, 16), "private"),
        (net4(224, 0, 0, 0, 4), "multicast"),
    ]
});

/// Fixed IPv6 blocklist (§8.2): loopback, unique-local (`fc00::/7`, IPv6's rough equivalent
/// of RFC 1918 space), link-local, unspecified, multicast. No IPv6 CGNAT range exists.
static BLOCKED_V6: LazyLock<Vec<(Ipv6Net, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Ipv6Net::new(Ipv6Addr::UNSPECIFIED, 128).unwrap(),
            "unspecified",
        ),
        (Ipv6Net::new(Ipv6Addr::LOCALHOST, 128).unwrap(), "loopback"),
        (
            Ipv6Net::new(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7).unwrap(),
            "private",
        ),
        (
            Ipv6Net::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10).unwrap(),
            "link-local",
        ),
        (
            Ipv6Net::new(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8).unwrap(),
            "multicast",
        ),
    ]
});

fn net4(a: u8, b: u8, c: u8, d: u8, prefix: u8) -> Ipv4Net {
    Ipv4Net::new(Ipv4Addr::new(a, b, c, d), prefix).expect("fixed prefix is always valid")
}

/// Why `ip` is refused, or `None` if it's safe to connect to. `allow_localhost` is the
/// `--http-allow-localhost` escape hatch (§10): it lifts the loopback block only — every
/// other category (private, link-local/cloud-metadata, unspecified, multicast, CGNAT) stays
/// blocked regardless, since the flag is meant for "I'm running a dev server on 127.0.0.1",
/// not "disable SSRF protection".
///
/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is unwrapped to its embedded IPv4 address
/// and re-checked — otherwise a resolver answering with the mapped form would walk straight
/// past the IPv4 table.
pub fn blocked_reason(ip: IpAddr, allow_localhost: bool) -> Option<&'static str> {
    let reason = match ip {
        IpAddr::V4(v4) => lookup_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(mapped) => return blocked_reason(IpAddr::V4(mapped), allow_localhost),
            None => lookup_v6(v6),
        },
    };
    match reason {
        Some("loopback") if allow_localhost => None,
        other => other,
    }
}

fn lookup_v4(ip: Ipv4Addr) -> Option<&'static str> {
    BLOCKED_V4
        .iter()
        .find(|(net, _)| net.contains(&ip))
        .map(|(_, reason)| *reason)
}

fn lookup_v6(ip: Ipv6Addr) -> Option<&'static str> {
    BLOCKED_V6
        .iter()
        .find(|(net, _)| net.contains(&ip))
        .map(|(_, reason)| *reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn v6(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn table_of_blocked_and_allowed_ipv4_addresses() {
        let cases: &[(&str, Option<&str>)] = &[
            ("8.8.8.8", None),
            ("1.1.1.1", None),
            ("93.184.216.34", None),
            ("0.0.0.0", Some("unspecified")),
            ("10.0.0.1", Some("private")),
            ("10.255.255.255", Some("private")),
            ("100.64.0.1", Some("cgnat")),
            ("100.127.255.255", Some("cgnat")),
            ("127.0.0.1", Some("loopback")),
            ("127.255.255.255", Some("loopback")),
            ("169.254.169.254", Some("link-local")),
            ("172.16.0.1", Some("private")),
            ("172.31.255.255", Some("private")),
            ("172.32.0.1", None),
            ("192.168.0.1", Some("private")),
            ("192.168.255.255", Some("private")),
            ("224.0.0.1", Some("multicast")),
        ];
        for (addr, expected) in cases {
            assert_eq!(
                blocked_reason(v4(addr), false),
                *expected,
                "{addr} classified wrong"
            );
        }
    }

    #[test]
    fn table_of_blocked_and_allowed_ipv6_addresses() {
        let cases: &[(&str, Option<&str>)] = &[
            ("2606:4700:4700::1111", None),
            ("::", Some("unspecified")),
            ("::1", Some("loopback")),
            ("fc00::1", Some("private")),
            ("fd12:3456:789a::1", Some("private")),
            ("fe80::1", Some("link-local")),
            ("ff02::1", Some("multicast")),
        ];
        for (addr, expected) in cases {
            assert_eq!(
                blocked_reason(v6(addr), false),
                *expected,
                "{addr} classified wrong"
            );
        }
    }

    #[test]
    fn allow_localhost_lifts_only_the_loopback_block() {
        assert_eq!(blocked_reason(v4("127.0.0.1"), true), None);
        assert_eq!(blocked_reason(v6("::1"), true), None);
        // Every other category stays blocked even with the flag set.
        assert_eq!(
            blocked_reason(v4("169.254.169.254"), true),
            Some("link-local"),
            "cloud metadata must stay blocked even with --http-allow-localhost"
        );
        assert_eq!(blocked_reason(v4("10.0.0.1"), true), Some("private"));
    }

    #[test]
    fn an_ipv4_mapped_ipv6_address_is_checked_against_the_v4_table() {
        // ::ffff:169.254.169.254 — a rebinding-style attempt to reach cloud metadata via
        // the IPv6-mapped form of a blocked IPv4 address.
        let mapped = v6("::ffff:169.254.169.254");
        assert_eq!(blocked_reason(mapped, false), Some("link-local"));
    }
}
