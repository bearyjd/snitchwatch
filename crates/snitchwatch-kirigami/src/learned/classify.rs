//! The objective facts the aggregator sorts connections by: the destination a
//! row names, whether it is a bare IP, whether the port is an uncommon one and
//! whether a program runs from a place an attacker can write to. Pure.

use crate::connections::grouping::is_ip;

/// Ports that are not a flag: DNS, HTTP, NTP, HTTPS, DNS over TLS and mDNS.
const COMMON_PORTS: [u16; 6] = [53, 80, 123, 443, 853, 5353];

/// Directories a user (or an attacker running as the user) can write to, or
/// that are temporary. `/var/home` and `/var/roothome` are the atomic-OS homes.
const FLAGGED_DIRS: [&str; 12] = [
    "/home",
    "/var/home",
    "/root",
    "/var/roothome",
    "/tmp",
    "/var/tmp",
    "/dev/shm",
    "/run/user",
    "/run/media",
    "/var/mnt",
    "/mnt",
    "/srv",
];

/// The suffix `/proc/<pid>/exe` gets once the file was removed.
const DELETED_SUFFIX: &str = "(deleted)";

/// A host as written in a row, normalised: trimmed and lowercased; an IPv6
/// address loses its brackets and `%zone`; a trailing `:port` is dropped; one
/// trailing dot is removed. So `[::1]:443`, `fe80::1%eth0` and `1.2.3.4:80`
/// are seen as the addresses they are.
fn normalise_host(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    let is_port = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let mut host = lower.as_str();
    if let Some((inside, tail)) = host.strip_prefix('[').and_then(|r| r.split_once(']')) {
        if tail.is_empty() || tail.strip_prefix(':').is_some_and(is_port) {
            host = inside;
        }
    } else if let Some((name, port)) = host.split_once(':') {
        // One colon only: more are an IPv6 address.
        if is_port(port) && !port.contains(':') {
            host = name;
        }
    }
    if let Some((address, _zone)) = host.split_once('%') {
        if address.parse::<std::net::Ipv6Addr>().is_ok() {
            host = address;
        }
    }
    host.strip_suffix('.').unwrap_or(host).to_string()
}

/// The destination of a row: its normalised hostname, else its normalised
/// address. A bare IP is kept as such.
pub(super) fn destination(dst_host: &str, dst_ip: &str) -> String {
    let host = normalise_host(dst_host);
    if host.is_empty() {
        normalise_host(dst_ip)
    } else {
        host
    }
}

/// Whether `destination` is a literal address, not a name.
pub(super) fn is_bare_ip(destination: &str) -> bool {
    is_ip(destination)
}

/// Whether `port` is one that is not 53, 80, 123, 443, 853 or 5353. Port 0 of
/// an ICMP row is no port at all, so it is not "uncommon".
pub(super) fn is_uncommon_port(port: u16, protocol: &str) -> bool {
    if port == 0 && protocol.trim().to_lowercase().starts_with("icmp") {
        return false;
    }
    !COMMON_PORTS.contains(&port)
}

/// Whether a program at `path` runs from a user-writable or temporary place,
/// or from a file that no longer exists.
pub(super) fn is_path_flagged(path: &str) -> bool {
    path.trim_end().ends_with(DELETED_SUFFIX)
        || FLAGGED_DIRS.iter().any(|dir| {
            path.strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// The directory part of `path` (`/` for a file at the root, the path itself
/// when it has no `/`).
pub(super) fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(at) => path[..at].to_string(),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostname_is_lowercased_and_loses_one_trailing_dot() {
        assert_eq!(destination("Example.COM.", "1.2.3.4"), "example.com");
        assert_eq!(destination("a.b..", "1.2.3.4"), "a.b.");
        assert_eq!(destination("  Host.Example ", "1.2.3.4"), "host.example");
    }

    #[test]
    fn brackets_zone_ids_and_ports_do_not_hide_a_bare_ip() {
        for (host, expected) in [
            ("[2001:db8::1]", "2001:db8::1"),
            ("[::1]:443", "::1"),
            ("fe80::1%eth0", "fe80::1"),
            ("[FE80::1%eth0]:8080", "fe80::1"),
            ("1.2.3.4:80", "1.2.3.4"),
            ("2001:db8::1", "2001:db8::1"),
            ("1.2.3.4.", "1.2.3.4"),
        ] {
            let seen = destination(host, "");
            assert_eq!(seen, expected, "{host}");
            assert!(is_bare_ip(&seen), "{host}");
        }
        // The same in the address column when the name is empty.
        assert_eq!(destination("", "[::1]:443"), "::1");
        // A name keeps its name, minus a port.
        assert_eq!(destination("Example.COM:443", "1.2.3.4"), "example.com");
        assert!(!is_bare_ip(&destination("example.com:8080", "1.2.3.4")));
    }

    #[test]
    fn without_a_hostname_the_address_is_the_destination() {
        assert_eq!(destination("", "203.0.113.9"), "203.0.113.9");
        assert_eq!(destination(" . ", "2001:DB8::1"), "2001:db8::1".to_string());
        assert!(is_bare_ip(&destination("", "203.0.113.9")));
        // A row whose "hostname" is an address is a bare IP too.
        assert!(is_bare_ip(&destination("203.0.113.9", "203.0.113.9")));
        assert!(!is_bare_ip("example.com"));
    }

    #[test]
    fn only_the_six_common_ports_are_unflagged() {
        for port in [53, 80, 123, 443, 853, 5353] {
            assert!(!is_uncommon_port(port, "tcp"), "{port}");
        }
        for port in [22, 4444, 8080, 8443, 1, 65535] {
            assert!(is_uncommon_port(port, "tcp"), "{port}");
        }
        assert!(is_uncommon_port(0, "tcp"));
        assert!(!is_uncommon_port(0, "ICMP"));
        assert!(!is_uncommon_port(0, "icmpv6"));
    }

    #[test]
    fn user_writable_and_temporary_directories_are_flagged_by_whole_component() {
        for path in [
            "/home/u/bin/x",
            "/var/home/u/x",
            "/root/x",
            "/var/roothome/x",
            "/tmp/x",
            "/var/tmp/x",
            "/dev/shm/x",
            "/run/user/1000/x",
            "/run/media/u/stick/x",
            "/var/mnt/disk/x",
            "/mnt/x",
            "/srv/x",
            "/usr/bin/gone (deleted)",
            "/usr/bin/gone(deleted)",
        ] {
            assert!(is_path_flagged(path), "{path}");
        }
        for path in [
            "/usr/bin/curl",
            "/opt/app/x",
            "/homebrew/x",
            "/tmpfiles/x",
            "/home",
            "/usr/local/bin/tmp/x",
            "/rootfs/x",
        ] {
            assert!(!is_path_flagged(path), "{path}");
        }
    }

    #[test]
    fn the_parent_directory_is_everything_before_the_last_slash() {
        assert_eq!(parent_dir("/tmp/a/b"), "/tmp/a");
        assert_eq!(parent_dir("/x"), "/");
        assert_eq!(parent_dir("noslash"), "noslash");
    }
}
