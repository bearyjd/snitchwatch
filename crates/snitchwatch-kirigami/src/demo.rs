//! Demo mode: fake connection rows for reviewing the UI without a bridge.
//!
//! Enabled only by the process environment (`SNITCHWATCH_DEMO=1`), read here
//! and nowhere from bridge data: nothing in this module consumes or produces
//! a bridge message, and the bridge runtime is never consulted. The
//! screenshot directory is honoured only together with demo mode, so a stray
//! `SNITCHWATCH_SCREENSHOT_DIR` on a real session does nothing.

use std::path::PathBuf;

use snitchwatch_bridge::ws_messages::ConnectionRow;

/// Env var that turns demo mode on (exactly `1`).
pub const ENV_DEMO: &str = "SNITCHWATCH_DEMO";
/// Env var naming the directory screenshots are written to (demo mode only).
pub const ENV_SCREENSHOT_DIR: &str = "SNITCHWATCH_SCREENSHOT_DIR";

fn demo_flag(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Whether the process was started in demo mode.
pub fn enabled() -> bool {
    demo_flag(std::env::var(ENV_DEMO).ok().as_deref())
}

fn screenshot_dir_from(demo: bool, value: Option<&str>) -> Option<PathBuf> {
    value.filter(|v| demo && !v.is_empty()).map(PathBuf::from)
}

/// Where to write screenshots, only in demo mode.
pub fn screenshot_dir() -> Option<PathBuf> {
    screenshot_dir_from(enabled(), std::env::var(ENV_SCREENSHOT_DIR).ok().as_deref())
}

const FIREFOX: &str = "/usr/lib64/firefox/firefox";
const NM: &str = "/usr/sbin/NetworkManager";
const RESOLVED: &str = "/usr/lib/systemd/systemd-resolved";
const SYNCTHING: &str = "/usr/bin/syncthing";
const TAILSCALED: &str = "/usr/sbin/tailscaled";
const SPOTIFY: &str =
    "/var/lib/flatpak/app/com.spotify.Client/x86_64/stable/9f3c/files/extra/share/spotify/spotify";
const CURL: &str = "/usr/bin/curl";
const SSH: &str = "/usr/bin/ssh";
/// Recommended (`snitchwatch-default-`) rules, so the exception-first
/// default (0c) has a "Background and system" group to show.
const NM_RULE: &str = "snitchwatch-default-networkmanager";
const RESOLVED_RULE: &str = "snitchwatch-default-resolved";

struct Spec {
    process: &'static str,
    path: &'static str,
    host: &'static str,
    ip: &'static str,
    port: u16,
    proto: &'static str,
    direction: &'static str,
    /// `None` = pending.
    action: Option<&'static str>,
    rule: Option<&'static str>,
    age_secs: i64,
}

// Row-table shorthand: one argument per column of `SPECS`.
#[allow(clippy::too_many_arguments)]
const fn s(
    process: &'static str,
    path: &'static str,
    host: &'static str,
    ip: &'static str,
    port: u16,
    proto: &'static str,
    direction: &'static str,
    action: Option<&'static str>,
    rule: Option<&'static str>,
    age_secs: i64,
) -> Spec {
    Spec {
        process,
        path,
        host,
        ip,
        port,
        proto,
        direction,
        action,
        rule,
        age_secs,
    }
}

const OUT: &str = "outgoing";
const IN: &str = "incoming";

const SPECS: [Spec; 26] = [
    s(
        "firefox",
        FIREFOX,
        "www.mozilla.org",
        "151.101.65.91",
        443,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-firefox"),
        12,
    ),
    s(
        "firefox",
        FIREFOX,
        "github.com",
        "140.82.121.4",
        443,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-firefox"),
        40,
    ),
    s(
        "firefox",
        FIREFOX,
        "fonts.gstatic.com",
        "2a00:1450:4001:82b::2003",
        443,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-firefox"),
        95,
    ),
    s(
        "firefox",
        FIREFOX,
        "stats.g.doubleclick.net",
        "142.250.80.34",
        443,
        "tcp",
        OUT,
        Some("blocklist"),
        Some("blocklist-ads"),
        130,
    ),
    s(
        "firefox",
        FIREFOX,
        "telemetry.example.net",
        "203.0.113.50",
        443,
        "tcp",
        OUT,
        Some("deny"),
        Some("deny-telemetry"),
        210,
    ),
    s(
        "firefox",
        FIREFOX,
        "new-site.example.org",
        "198.51.100.23",
        443,
        "tcp",
        OUT,
        None,
        None,
        4,
    ),
    s(
        "NetworkManager",
        NM,
        "connectivity-check.ubuntu.com",
        "185.125.190.97",
        80,
        "tcp",
        OUT,
        Some("allow"),
        Some(NM_RULE),
        300,
    ),
    s(
        "NetworkManager",
        NM,
        "",
        "192.168.1.1",
        67,
        "udp",
        OUT,
        Some("allow"),
        Some(NM_RULE),
        600,
    ),
    s(
        "NetworkManager",
        NM,
        "",
        "fe80::1",
        547,
        "udp",
        OUT,
        Some("allow"),
        Some(NM_RULE),
        610,
    ),
    s(
        "systemd-resolved",
        RESOLVED,
        "dns.quad9.net",
        "9.9.9.9",
        853,
        "tcp",
        OUT,
        Some("allow"),
        Some(RESOLVED_RULE),
        20,
    ),
    s(
        "systemd-resolved",
        RESOLVED,
        "dns.quad9.net",
        "2620:fe::fe",
        853,
        "tcp",
        OUT,
        Some("allow"),
        Some(RESOLVED_RULE),
        21,
    ),
    s(
        "systemd-resolved",
        RESOLVED,
        "",
        "224.0.0.251",
        5353,
        "udp",
        IN,
        Some("deny"),
        Some("deny-mdns-inbound"),
        150,
    ),
    s(
        "syncthing",
        SYNCTHING,
        "relays.syncthing.net",
        "139.162.10.9",
        22067,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-syncthing"),
        55,
    ),
    s(
        "syncthing",
        SYNCTHING,
        "",
        "192.168.1.42",
        22000,
        "tcp",
        IN,
        Some("allow"),
        Some("allow-syncthing-lan"),
        70,
    ),
    s(
        "syncthing",
        SYNCTHING,
        "",
        "2001:db8:1::42",
        22000,
        "tcp",
        IN,
        Some("allow"),
        Some("allow-syncthing-lan"),
        71,
    ),
    s(
        "syncthing",
        SYNCTHING,
        "",
        "239.192.0.0",
        21027,
        "udp",
        IN,
        None,
        None,
        9,
    ),
    s(
        "tailscaled",
        TAILSCALED,
        "controlplane.tailscale.com",
        "192.200.0.100",
        443,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-tailscale"),
        33,
    ),
    s(
        "tailscaled",
        TAILSCALED,
        "derp1.tailscale.com",
        "2a05:d014:386:202::1",
        443,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-tailscale"),
        34,
    ),
    s(
        "tailscaled",
        TAILSCALED,
        "",
        "100.101.102.103",
        41641,
        "udp",
        IN,
        Some("allow"),
        Some("allow-tailscale"),
        35,
    ),
    s(
        "spotify",
        SPOTIFY,
        "apresolve.spotify.com",
        "35.186.224.53",
        443,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-spotify"),
        88,
    ),
    s(
        "spotify",
        SPOTIFY,
        "spclient.wg.spotify.com",
        "2a03:b0c0:3:d0::1",
        443,
        "tcp",
        OUT,
        None,
        None,
        6,
    ),
    s(
        "curl",
        CURL,
        "api.example.com",
        "93.184.216.34",
        443,
        "tcp",
        OUT,
        None,
        None,
        2,
    ),
    s(
        "curl",
        CURL,
        "ifconfig.me",
        "34.160.111.145",
        80,
        "tcp",
        OUT,
        Some("deny"),
        Some("deny-curl-plain-http"),
        400,
    ),
    s(
        "ssh",
        SSH,
        "git.example.org",
        "198.51.100.77",
        22,
        "tcp",
        OUT,
        Some("allow"),
        Some("allow-ssh"),
        500,
    ),
    s(
        "ssh",
        SSH,
        "unknown-host.example.net",
        "203.0.113.9",
        22,
        "tcp",
        OUT,
        Some("deny"),
        Some("deny-ssh-unknown"),
        520,
    ),
    // No program file known: its rule button shows "Can't add rule".
    s(
        "dhclient",
        "",
        "ntp.example.org",
        "192.0.2.123",
        123,
        "udp",
        OUT,
        Some("allow"),
        None,
        3,
    ),
];

/// Demo-only country lookup for the fake rows' addresses (most are
/// documentation ranges with no real country), so the map shows a spread of
/// countries. Installed by `geo::shared_geo` only when [`enabled`].
pub struct DemoGeo;

impl DemoGeo {
    const COUNTRIES: [(&'static str, &'static str, &'static str); 19] = [
        ("151.101.65.91", "US", "United States"),
        ("140.82.121.4", "US", "United States"),
        ("2a00:1450:4001:82b::2003", "IE", "Ireland"),
        ("142.250.80.34", "US", "United States"),
        ("203.0.113.50", "CN", "China"),
        ("198.51.100.23", "JP", "Japan"),
        ("185.125.190.97", "GB", "United Kingdom"),
        ("9.9.9.9", "CH", "Switzerland"),
        ("2620:fe::fe", "CH", "Switzerland"),
        ("139.162.10.9", "DE", "Germany"),
        ("192.200.0.100", "CA", "Canada"),
        ("2a05:d014:386:202::1", "FR", "France"),
        ("35.186.224.53", "SE", "Sweden"),
        ("2a03:b0c0:3:d0::1", "NL", "Netherlands"),
        ("93.184.216.34", "AU", "Australia"),
        ("34.160.111.145", "BR", "Brazil"),
        ("198.51.100.77", "IN", "India"),
        ("203.0.113.9", "RU", "Russia"),
        ("192.0.2.123", "ZA", "South Africa"),
    ];
}

impl crate::geo::resolver::CountryLookup for DemoGeo {
    fn lookup(&self, addr: std::net::IpAddr) -> Option<crate::geo::resolver::CountryInfo> {
        let ip = addr.to_string();
        let (_, code, name) = Self::COUNTRIES.iter().find(|(known, _, _)| *known == ip)?;
        Some(crate::geo::resolver::CountryInfo {
            code: code.to_string(),
            name: name.to_string(),
        })
    }
}

/// ~26 realistic fake rows: allowed, denied, pending and blocklisted;
/// several programs (one a Flatpak app), IPv4 and IPv6, both directions.
pub fn demo_rows(now_ms: i64) -> Vec<ConnectionRow> {
    SPECS
        .iter()
        .enumerate()
        .map(|(i, sp)| ConnectionRow {
            id: format!("demo-{i}"),
            process: sp.process.to_string(),
            process_path: Some(sp.path.to_string()),
            dst_host: sp.host.to_string(),
            dst_ip: sp.ip.to_string(),
            dst_port: sp.port,
            protocol: sp.proto.to_string(),
            direction: sp.direction.to_string(),
            action: sp.action.map(str::to_string),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: now_ms - sp.age_secs * 1000,
            matched_rule: sp.rule.map(str::to_string),
            auto_answer: None,
            // A pending row shows the bridge's 30 s countdown, as a live one.
            answer_deadline_ms: sp.action.is_none().then_some(now_ms + 30_000),
            deferred: false,
            decided_by_default: false,
        })
        .collect()
}

/// The demo rows' recommended rules, as `SetRules` carries them.
pub fn demo_rules() -> Vec<serde_json::Value> {
    [NM_RULE, RESOLVED_RULE]
        .into_iter()
        .map(|name| serde_json::json!({ "name": name, "enabled": true, "action": "allow", "toggleable": true }))
        .collect()
}

/// Fake grouped recommended rules, as the two messages a bridge that offers
/// them sends (JSON, in order), so the first-run "Recommended rules" step can
/// be reviewed without one. Texts follow the reviewed data file's wording.
pub fn curated_group_messages() -> Vec<String> {
    let entry = |id: &str, group: &str, program: &str, allows: &str, why: &str, broad: bool| {
        let mut e = serde_json::json!({
            "id": id, "group": group, "program": program, "allows": allows,
            "why": why, "on": false, "status": "off",
        });
        if broad {
            e["broad"] = true.into();
        }
        e
    };
    let entries = [
        entry(
            "networkmanager-connectivity-check",
            "system-services",
            "/usr/bin/NetworkManager",
            "/usr/bin/NetworkManager may connect to fedoraproject.org on TCP port 80.",
            "NetworkManager checks that this network reaches the internet by loading a test page.",
            false,
        ),
        entry(
            "chronyd-ntp",
            "system-services",
            "/usr/bin/chronyd",
            "/usr/bin/chronyd may connect to 2.fedora.pool.ntp.org on UDP port 123.",
            "chronyd sets the clock by asking Fedora's pool of time servers.",
            false,
        ),
        entry(
            "flatpak-flathub",
            "app-updates",
            "/usr/bin/flatpak",
            "/usr/bin/flatpak may connect to dl.flathub.org on TCP port 443.",
            "Flatpak downloads app updates and app information from Flathub.",
            false,
        ),
        entry(
            "tailscale-control",
            "tailscale",
            "/usr/bin/tailscaled",
            "/usr/bin/tailscaled may connect to controlplane.tailscale.com on TCP port 443.",
            "Tailscale signs in and fetches your network's settings from its coordination server.",
            false,
        ),
        entry(
            "dns-resolved",
            "name-resolution",
            "/usr/lib/systemd/systemd-resolved",
            "/usr/lib/systemd/systemd-resolved may connect to any address on port 53.",
            "The system resolver needs to reach its DNS server, which differs on every network.",
            true,
        ),
    ];
    let groups = [
        ("system-services", "System services", "Background services that keep the clock and the network working. Each is limited to the one place it needs."),
        ("app-updates", "App updates", "Flatpak fetching app updates and app information from Flathub."),
        ("tailscale", "Tailscale", "For people who use Tailscale: its sign-in server only."),
        ("name-resolution", "Name resolution", "The system resolver reaching its DNS server. This allows any address, so it is a separate choice with its own confirmation."),
    ];
    vec![
        serde_json::json!({
            "action": "authenticated",
            "capabilities": ["curatedDefaults", "curatedGroupsV1", "curatedBroadOptInV1"],
        })
        .to_string(),
        serde_json::json!({
            "action": "setCuratedDefaults",
            "entries": entries,
            "groups": groups.iter().map(|(id, title, description)| serde_json::json!({
                "id": id, "title": title, "description": description,
            })).collect::<Vec<_>>(),
            "storage": { "persistent": true },
        })
        .to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn demo_geo_places_the_public_demo_rows_on_the_map() {
        use crate::geo::resolver::{Bucket, SharedResolver};
        let resolver = SharedResolver::new(Some(std::sync::Arc::new(DemoGeo)));
        let mut codes = HashSet::new();
        for row in demo_rows(0) {
            if let Bucket::Country(code) = resolver.resolve(&row.dst_ip).bucket {
                assert!(crate::geo::world_map::lookup(&code).is_some(), "{code}");
                codes.insert(code);
            }
        }
        assert!(
            codes.len() >= 10,
            "demo map should show many countries: {codes:?}"
        );
    }

    #[test]
    fn only_exactly_one_enables_demo() {
        assert!(demo_flag(Some("1")));
        for v in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
            assert!(!demo_flag(v), "{v:?}");
        }
    }

    #[test]
    fn screenshot_dir_needs_demo_mode() {
        assert_eq!(screenshot_dir_from(false, Some("/tmp/x")), None);
        assert_eq!(screenshot_dir_from(true, Some("")), None);
        assert_eq!(screenshot_dir_from(true, None), None);
        assert_eq!(
            screenshot_dir_from(true, Some("/tmp/x")),
            Some(PathBuf::from("/tmp/x"))
        );
    }

    #[test]
    fn rows_cover_the_requested_mix() {
        let rows = demo_rows(1_760_000_000_000);
        assert_eq!(rows.len(), 26);
        let ids: HashSet<_> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), rows.len(), "row ids must be unique");
        let actions: HashSet<_> = rows.iter().map(|r| r.action.as_deref()).collect();
        for a in [None, Some("allow"), Some("deny"), Some("blocklist")] {
            assert!(actions.contains(&a), "missing {a:?}");
        }
        let programs: HashSet<_> = rows.iter().map(|r| r.process.as_str()).collect();
        for p in [
            "firefox",
            "NetworkManager",
            "systemd-resolved",
            "syncthing",
            "tailscaled",
        ] {
            assert!(programs.contains(p), "missing {p}");
        }
        assert!(rows.iter().any(|r| r.dst_ip.contains(':')), "needs IPv6");
        assert!(rows.iter().any(|r| r.dst_ip.contains('.')), "needs IPv4");
        assert!(rows.iter().any(|r| r.direction == "incoming"));
        assert!(rows.iter().any(|r| r.direction == "outgoing"));
        assert!(rows.iter().any(|r| r
            .process_path
            .as_deref()
            .is_some_and(|p| p.contains("/flatpak/app/"))));
        assert!(rows.iter().all(|r| r.started_at_ms < 1_760_000_000_000));
        for r in &rows {
            assert_eq!(
                r.answer_deadline_ms.is_some(),
                r.action.is_none(),
                "{}: only pending rows carry a deadline",
                r.id
            );
        }
    }

    #[test]
    fn module_never_touches_the_bridge() {
        let src = include_str!("demo.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "bridge_runtime",
            "ClientMessage",
            "ServerMessage",
            "BridgeHandles",
        ] {
            assert!(
                !code.contains(forbidden),
                "demo.rs must not use {forbidden}"
            );
        }
    }

    #[test]
    fn demo_groups_parse_as_a_grouped_offer() {
        use snitchwatch_bridge::ws_messages::ServerMessage;
        let msgs: Vec<ServerMessage> = curated_group_messages()
            .iter()
            .map(|m| serde_json::from_str(m).unwrap())
            .collect();
        assert!(matches!(msgs[0], ServerMessage::Authenticated { .. }));
        let ServerMessage::SetCuratedDefaults {
            entries, groups, ..
        } = &msgs[1]
        else {
            panic!("second message offers the rules");
        };
        assert_eq!(groups.len(), 4);
        assert!(entries
            .iter()
            .all(|e| groups.iter().any(|g| g.id == e.group)));
        assert!(entries.iter().filter(|e| e.broad).count() == 1);
    }
}
