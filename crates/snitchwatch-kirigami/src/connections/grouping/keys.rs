//! Group keys: the registrable-domain heuristic and the process, domain and host keys.

use snitchwatch_bridge::ws_messages::ConnectionRow;

/// Two-part public suffixes this heuristic special-cases so e.g.
/// `www.example.co.uk` groups under `example.co.uk`, not `co.uk`.
///
/// Not exhaustive (a full Public Suffix List is explicitly out of scope) —
/// this is deliberately a small, documented set of the most common
/// second-level-registration TLDs.
pub(super) const TWO_PART_TLDS: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "net.uk", "co.jp", "or.jp", "ne.jp", "co.kr", "co.nz",
    "co.za", "co.il", "co.in", "co.id", "com.au", "net.au", "org.au", "com.br", "com.cn", "com.mx",
    "com.sg", "com.tw", "co.th",
];

/// True when `host` parses as a literal IPv4/IPv6 address.
pub(crate) fn is_ip(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

/// Derive a pragmatic "registrable domain" (eTLD+1) from a hostname.
///
/// Heuristic, not a full Public Suffix List implementation (per the design
/// spec, exact PSL handling is not required):
///   * 1-2 labels (`localhost`, `example.com`) — returned as-is.
///   * Last two labels match a known two-part TLD (see [`TWO_PART_TLDS`]) —
///     the last *three* labels are kept (e.g. `example.co.uk`).
///   * Otherwise — the last two labels are kept (e.g. `github.com`).
///
/// Callers are expected to route bare IPs and empty hosts to the IP address
/// instead of this function (see [`domain_key`]) — "bare IPs group under the
/// IP itself" per the design spec.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim().to_lowercase();
    if host.is_empty() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    if labels.len() <= 2 {
        return host;
    }
    let last_two = format!("{}.{}", labels[labels.len() - 2], labels[labels.len() - 1]);
    if TWO_PART_TLDS.contains(&last_two.as_str()) && labels.len() >= 3 {
        labels[labels.len() - 3..].join(".")
    } else {
        last_two
    }
}

/// The process-group key for `row`: its executable path when known
/// (disambiguates identical process names at different install locations),
/// falling back to the bare process name.
pub(super) fn process_key(row: &ConnectionRow) -> String {
    row.process_path
        .clone()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| row.process.clone())
}

/// The domain-group key/label for `row`: its registrable domain, or the
/// destination IP for bare-IP / empty hosts.
pub(super) fn domain_key(row: &ConnectionRow) -> String {
    let host = row.dst_host.trim();
    if host.is_empty() || is_ip(host) {
        row.dst_ip.clone()
    } else {
        registrable_domain(host)
    }
}

/// The host-group key for `row`: its lowercased hostname, or the destination
/// IP for bare-IP / empty hosts (so such a host row equals its domain row).
pub(super) fn host_key(row: &ConnectionRow) -> String {
    let host = row.dst_host.trim();
    if host.is_empty() || is_ip(host) {
        row.dst_ip.clone()
    } else {
        host.to_lowercase()
    }
}

pub(super) fn composite_key(process_key: &str, domain_key: &str) -> String {
    format!("{process_key}\u{1}{domain_key}")
}

pub(super) fn host_composite_key(process_key: &str, domain_key: &str, host_key: &str) -> String {
    format!("{process_key}\u{1}{domain_key}\u{1}{host_key}")
}
