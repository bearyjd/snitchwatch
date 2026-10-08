//! Which addresses the blocklist fetcher may connect to (issue #45, SSRF).
//!
//! In system mode any `snitchwatch-ui` member chooses the URLs the
//! `snitchwatch` account downloads, so the fetcher must not reach the host
//! itself or link-local / carrier / reserved networks. Every connection is
//! checked here:
//! - a host name is resolved by [`GuardedResolver`], which drops refused
//!   addresses and hands reqwest only the rest, so the connection is pinned
//!   to checked addresses (no second lookup to rebind);
//! - an IP-literal host never reaches a resolver, so
//!   [`check_literal_host`] checks it at validation and on every redirect.
//!
//! [`is_allowed_fetch_target`] is the single address policy.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// RFC 1918 (10/8, 172.16/12, 192.168/16) and ULA (fc00::/7) addresses.
/// Allowed for now: LAN-hosted lists are legitimate. Owner decision pending
/// (issue #45); setting this to `false` blocks them everywhere.
const ALLOW_LAN_TARGETS: bool = true;

/// Shown when a URL names a refused address (no detail, see the module doc).
pub const REFUSED_ADDRESS_REASON: &str =
    "URL not allowed: it points to a local or reserved address";

/// The address policy: false for loopback, unspecified, link-local,
/// multicast, broadcast, 0.0.0.0/8, CGNAT (100.64/10), 240/4, and IPv4
/// embedded in IPv6 (mapped, compatible, NAT64) when the IPv4 is refused.
pub fn is_allowed_fetch_target(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_allowed_v4(v4),
        IpAddr::V6(v6) => match embedded_ipv4(v6) {
            Some(v4) => is_allowed_v4(v4),
            None => is_allowed_v6(v6),
        },
    }
}

fn is_allowed_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    let refused = ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || a == 0
        || (a == 100 && (b & 0xc0) == 64)
        || a >= 240;
    if refused {
        return false;
    }
    !ip.is_private() || ALLOW_LAN_TARGETS
}

fn is_allowed_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    let refused = ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0;
    if refused {
        return false;
    }
    (first & 0xfe00) != 0xfc00 || ALLOW_LAN_TARGETS
}

/// The IPv4 address inside an IPv4-mapped (`::ffff:a.b.c.d`),
/// IPv4-compatible (`::a.b.c.d`, including `::` and `::1`) or NAT64
/// (`64:ff9b::/96`) address.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return Some(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    ip.to_ipv4()
}

/// The URL's host as an IP address, if it is an IP literal.
fn literal_ip(url: &reqwest::Url) -> Option<IpAddr> {
    let host = url.host_str()?;
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

/// Refuse an IP-literal host that `allow` refuses. Host names are checked by
/// [`GuardedResolver`] when they are resolved.
pub fn check_literal_host(url: &reqwest::Url, allow: fn(IpAddr) -> bool) -> Result<(), String> {
    match literal_ip(url) {
        Some(ip) if !allow(ip) => Err(REFUSED_ADDRESS_REASON.to_string()),
        _ => Ok(()),
    }
}

/// Keep only the addresses `allow` accepts.
pub fn filter_addrs(
    addrs: impl IntoIterator<Item = SocketAddr>,
    allow: fn(IpAddr) -> bool,
) -> Vec<SocketAddr> {
    addrs.into_iter().filter(|a| allow(a.ip())).collect()
}

/// System DNS, minus refused addresses. reqwest connects only to what this
/// returns, so a name can't be checked against one answer and connected
/// with another.
pub struct GuardedResolver {
    pub allow: fn(IpAddr) -> bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow = self.allow;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let allowed = filter_addrs(resolved, allow);
            if allowed.is_empty() {
                let refused: Box<dyn std::error::Error + Send + Sync> =
                    format!("{host} resolves only to refused addresses").into();
                return Err(refused);
            }
            let addrs: Addrs = Box::new(allowed.into_iter());
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_policy() {
        let refused = [
            "127.0.0.1",
            "127.9.9.9",
            "0.0.0.0",
            "0.1.2.3",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.254",
            "224.0.0.1",
            "239.255.255.250",
            "255.255.255.255",
            "240.0.0.1",
            "::",
            "::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
        ];
        for ip in refused {
            assert!(
                !is_allowed_fetch_target(ip.parse().unwrap()),
                "{ip} must be refused"
            );
        }
        let allowed = [
            "93.184.216.34",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "2001:4860:4860::8888",
            "::ffff:93.184.216.34",
            "64:ff9b::5db8:d822",
        ];
        for ip in allowed {
            assert!(
                is_allowed_fetch_target(ip.parse().unwrap()),
                "{ip} must be allowed"
            );
        }
    }

    /// LAN addresses follow the one switch, [`ALLOW_LAN_TARGETS`].
    #[test]
    fn lan_addresses_follow_the_owner_switch() {
        for ip in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "fd00::1",
            "fc00::1",
        ] {
            assert_eq!(
                is_allowed_fetch_target(ip.parse().unwrap()),
                ALLOW_LAN_TARGETS,
                "{ip}"
            );
        }
    }

    #[test]
    fn ip_literal_hosts_are_checked_in_every_spelling() {
        for url in [
            "https://127.0.0.1/x",
            "https://2130706433/x",
            "https://0x7f.0.0.1/x",
            "https://0177.0.0.1/x",
            "https://[::1]/x",
            "https://[::ffff:127.0.0.1]/x",
            "https://169.254.169.254/latest/meta-data",
        ] {
            let parsed = reqwest::Url::parse(url).unwrap();
            assert_eq!(
                check_literal_host(&parsed, is_allowed_fetch_target),
                Err(REFUSED_ADDRESS_REASON.to_string()),
                "{url}"
            );
        }
        for url in ["https://93.184.216.34/x", "https://lists.example/x"] {
            let parsed = reqwest::Url::parse(url).unwrap();
            assert!(check_literal_host(&parsed, is_allowed_fetch_target).is_ok());
        }
    }

    /// Mixed answers are pinned to the allowed addresses only.
    #[test]
    fn only_allowed_resolved_addresses_are_kept() {
        let addrs: Vec<SocketAddr> = ["127.0.0.1:0", "93.184.216.34:0", "[::1]:0"]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect();
        assert_eq!(
            filter_addrs(addrs, is_allowed_fetch_target),
            vec!["93.184.216.34:0".parse::<SocketAddr>().unwrap()]
        );
    }

    #[tokio::test]
    async fn localhost_resolves_to_nothing_allowed() {
        let resolver = GuardedResolver {
            allow: is_allowed_fetch_target,
        };
        let name: Name = "localhost".parse().unwrap();
        assert!(resolver.resolve(name).await.is_err());
    }
}
