//! `network` operands: CIDR membership and the daemon's alias table.
//!
//! `Operator.Compile` takes the operator's data as an alias from
//! `AliasIPCache` if it is one (exact, case-sensitive key), else as a CIDR
//! (`net.ParseCIDR`); the compared address is the connection's `DstIP` /
//! `SrcIP`. Everything here uses `std::net` plus prefix math.

use std::net::{IpAddr, Ipv6Addr};

/// `vendor/opensnitch/daemon/data/network_aliases.json` as `LoadAliases`
/// leaves it in `AliasIPCache`: entries `net.ParseCIDR` rejects are skipped.
/// The shipped file lists `"::1"` (no prefix length) under `LAN`, so the
/// daemon's `LAN` does **not** contain the IPv6 loopback address. A test pins
/// this table to the vendored file.
///
/// The daemon host reads its own copy (`/etc/opensnitchd/network_aliases.json`),
/// which can be edited; only the shipped aliases are known here.
const ALIASES: &[(&str, &[&str])] = &[
    (
        "LAN",
        &[
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "127.0.0.0/8",
            "fc00::/7",
        ],
    ),
    ("MULTICAST", &["224.0.0.0/4", "ff00::/8"]),
];

/// An address as Go's `net.IP` prints it: a v4-mapped IPv6 address is the
/// plain IPv4 one. `None` if `text` isn't an IP address.
pub(super) fn parse_ip(text: &str) -> Option<IpAddr> {
    match text.parse::<IpAddr>().ok()? {
        IpAddr::V6(v6) => Some(v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4)),
        v4 => Some(v4),
    }
}

/// Whether `ip` is inside the network `data` names. `None` when `data` is
/// neither a known alias nor a CIDR (the daemon would not have loaded the
/// rule, or its alias file differs from the shipped one).
pub(super) fn contains(data: &str, ip: IpAddr) -> Option<bool> {
    if let Some((_, cidrs)) = ALIASES.iter().find(|(alias, _)| *alias == data) {
        return Some(
            cidrs
                .iter()
                .filter_map(|c| parse_cidr(c))
                .any(|net| net.contains(ip)),
        );
    }
    parse_cidr(data).map(|net| net.contains(ip))
}

struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

/// `net.ParseCIDR`: an address, `/`, and a decimal prefix length for that
/// family.
fn parse_cidr(text: &str) -> Option<Cidr> {
    let (addr, prefix) = text.split_once('/')?;
    let addr: IpAddr = addr.parse().ok()?;
    if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let prefix: u8 = prefix.parse().ok()?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    (prefix <= max).then(|| Cidr::new(addr, prefix))
}

impl Cidr {
    /// `ParseCIDR` masks the host bits, and `IPNet.Contains` then reads a
    /// network whose masked address is v4-mapped (`::ffff:a.b.c.d`) as the
    /// IPv4 network it names: `To4()` of the address and the low 32 bits of
    /// the mask. A prefix too short to keep the `ffff` marker stays IPv6.
    fn new(addr: IpAddr, prefix: u8) -> Self {
        if let IpAddr::V6(v6) = addr {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            if let Some(v4) = Ipv6Addr::from(u128::from(v6) & mask).to_ipv4_mapped() {
                return Self {
                    addr: IpAddr::V4(v4),
                    prefix: prefix.saturating_sub(96),
                };
            }
        }
        Self { addr, prefix }
    }

    /// `IPNet.Contains`: same family, and the first `prefix` bits agree (so
    /// the host bits typed in the CIDR don't matter).
    fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VENDORED_ALIASES: &str =
        include_str!("../../../../../vendor/opensnitch/daemon/data/network_aliases.json");

    /// The embedded table is what opensnitchd v1.8.0 ends up with after
    /// loading the vendored `network_aliases.json`.
    #[test]
    fn the_embedded_aliases_match_the_vendored_json() {
        let vendored: std::collections::BTreeMap<String, Vec<String>> =
            serde_json::from_str(VENDORED_ALIASES).expect("vendored aliases parse");
        let embedded: std::collections::BTreeMap<String, Vec<String>> = ALIASES
            .iter()
            .map(|(alias, cidrs)| {
                (
                    alias.to_string(),
                    cidrs.iter().map(|c| c.to_string()).collect(),
                )
            })
            .collect();
        // `LoadAliases`: `continue` on a `net.ParseCIDR` error.
        let loaded: std::collections::BTreeMap<String, Vec<String>> = vendored
            .into_iter()
            .map(|(alias, cidrs)| {
                let kept = cidrs.into_iter().filter(|c| parse_cidr(c).is_some());
                (alias, kept.collect())
            })
            .collect();
        assert_eq!(embedded, loaded);
    }

    #[test]
    fn the_vendored_file_really_has_an_entry_the_daemon_drops() {
        // Documents why `ALIASES` has no `::1`: it is in the file, without a
        // prefix length, and `ParseCIDR` rejects it.
        assert!(VENDORED_ALIASES.contains("\"::1\""));
        assert!(parse_cidr("::1").is_none());
    }

    #[test]
    fn cidr_parsing_follows_parse_cidr() {
        for good in [
            "10.0.0.0/8",
            "0.0.0.0/0",
            "192.168.1.1/32",
            "::/0",
            "2001:db8::/32",
            "10.1.2.3/8",
        ] {
            assert!(parse_cidr(good).is_some(), "{good}");
        }
        for bad in [
            "10.0.0.0",
            "10.0.0.0/",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/+8",
            "10.0.0.0/-1",
            "10.0.0.0/8 ",
            "x/8",
            "/8",
            "10.0.0.0/8/8",
            "",
        ] {
            assert!(parse_cidr(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn prefix_boundaries_do_not_overflow() {
        let ip = |s: &str| parse_ip(s).unwrap();
        assert_eq!(contains("0.0.0.0/0", ip("255.255.255.255")), Some(true));
        assert_eq!(contains("1.2.3.4/32", ip("1.2.3.4")), Some(true));
        assert_eq!(contains("1.2.3.4/32", ip("1.2.3.5")), Some(false));
        assert_eq!(contains("::/0", ip("2001:db8::1")), Some(true));
        assert_eq!(contains("::1/128", ip("::1")), Some(true));
        assert_eq!(contains("::1/128", ip("::2")), Some(false));
    }

    #[test]
    fn parse_ip_prints_v4_mapped_addresses_as_ipv4() {
        assert_eq!(parse_ip("::ffff:1.2.3.4").unwrap().to_string(), "1.2.3.4");
        assert_eq!(
            parse_ip("2001:DB8:0:0:0:0:0:1").unwrap().to_string(),
            "2001:db8::1"
        );
        assert!(parse_ip("1.2.3").is_none());
        assert!(parse_ip("fe80::1%eth0").is_none());
    }
}
