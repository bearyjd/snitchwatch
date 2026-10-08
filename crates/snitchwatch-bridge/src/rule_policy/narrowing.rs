//! Which conditions actually narrow a rule, and which tie it to programs
//! (rule import, P2.7 review H1).
//!
//! A shape `validate_operator` accepts can still match every connection:
//! `true`, a `/0` network, or a regexp that matches every realistic subject
//! (`/` on a path, `.+` on a host). The daemon searches a regexp unanchored
//! and lowercases both pattern and subject unless `sensitive`
//! (`operator.go` `reCmp`), so a pattern is probed the same way against a
//! few representative subjects of its operand; one that matches them all
//! is treated as not narrowing. That is a heuristic, not a proof: the
//! probes are what a reviewer would try first.
//!
//! A rule is tied to programs only by a `simple` `process.path` that names a
//! real program file, or a non-empty `simple` `process.command`. A
//! `process.id` (reused after a reboot), a `process.parent.path` (the
//! daemon walks every ancestor, up to PID 1), a `process.env.*` value or a
//! path regexp can't be shown to name particular programs.

use snitchwatch_proto::protocol::Operator;

const PATHS: &[&str] = &[
    "/usr/bin/curl",
    "/usr/lib64/firefox/firefox",
    "/home/user/.local/bin/tool",
    "/opt/app name/bin/app",
    "/tmp/x",
];
const COMMANDS: &[&str] = &[
    "curl https://example.com",
    "/usr/bin/python3 -m http.server 8000",
    "bash",
];
const HOSTS: &[&str] = &[
    "example.com",
    "github.com",
    "a.b.example.org",
    "localhost",
    "xn--bcher-kva.example",
];
const IPS: &[&str] = &[
    "10.0.0.1",
    "192.168.1.20",
    "93.184.216.34",
    "::1",
    "2001:db8::1",
];
const PORTS: &[&str] = &["22", "53", "443", "8080", "65535"];
const PROTOCOLS: &[&str] = &["tcp", "udp", "icmp", "tcp6", "udp6"];
const IDS: &[&str] = &["0", "1", "1000", "4242", "65534"];
const INTERFACES: &[&str] = &["eth0", "wlan0", "lo", "enp3s0"];
const ENV_VALUES: &[&str] = &["", "1", "/home/user", "en_US.UTF-8"];
/// Compiled-size cap for a probe, as `regexp.rs` uses.
const SIZE_LIMIT: usize = 1 << 20;

fn probes(operand: &str) -> &'static [&'static str] {
    match operand {
        "process.path" | "process.parent.path" => PATHS,
        "process.command" => COMMANDS,
        "dest.host" => HOSTS,
        "dest.ip" | "source.ip" => IPS,
        "dest.port" | "source.port" => PORTS,
        "protocol" => PROTOCOLS,
        "process.id" | "user.id" => IDS,
        "iface.in" | "iface.out" => INTERFACES,
        _ if operand.starts_with("process.env.") => ENV_VALUES,
        _ => &[],
    }
}

/// Whether a leaf condition limits what its rule matches.
pub fn narrows(leaf: &Operator) -> bool {
    if leaf.operand == "true" {
        return false;
    }
    match leaf.r#type.as_str() {
        "network" => !is_zero_prefix(&leaf.data),
        "regexp" => !matches_every_probe(leaf),
        _ => true,
    }
}

/// A `/0` network: every address of its family. An IPv4-mapped IPv6
/// network counts as the IPv4 `/(prefix - 96)` the daemon reads it as.
fn is_zero_prefix(cidr: &str) -> bool {
    let Some((addr, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    let mapped = addr
        .parse::<std::net::Ipv6Addr>()
        .is_ok_and(|v6| v6.to_ipv4_mapped().is_some());
    if mapped {
        prefix <= 96
    } else {
        prefix == 0
    }
}

fn matches_every_probe(leaf: &Operator) -> bool {
    let subjects = probes(&leaf.operand);
    if subjects.is_empty() {
        return false;
    }
    let pattern = if leaf.sensitive {
        leaf.data.clone()
    } else {
        leaf.data.to_lowercase()
    };
    let Ok(compiled) = regex::RegexBuilder::new(&pattern)
        .size_limit(SIZE_LIMIT)
        .build()
    else {
        return false;
    };
    subjects.iter().all(|subject| {
        if leaf.sensitive {
            compiled.is_match(subject)
        } else {
            compiled.is_match(&subject.to_lowercase())
        }
    })
}

/// Whether a leaf names particular programs (see the module doc): a real
/// program file's path (not the daemon's "Kernel connection" placeholder)
/// or a command line. A pid names whatever process gets it next.
fn binds_programs(leaf: &Operator) -> bool {
    if leaf.r#type != "simple" || leaf.data.is_empty() {
        return false;
    }
    match leaf.operand.as_str() {
        "process.path" => crate::translator::process_binding::is_bindable_process_path(&leaf.data),
        "process.command" => true,
        _ => false,
    }
}

/// Whether an operator (a leaf, or a list ANDing leaves) ties its rule to
/// particular programs. Otherwise the rule applies to every app.
pub fn binds_to_programs(op: &Operator) -> bool {
    if op.r#type == "list" {
        op.list.iter().any(binds_programs)
    } else {
        binds_programs(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(r#type: &str, operand: &str, data: &str) -> Operator {
        Operator {
            r#type: r#type.into(),
            operand: operand.into(),
            data: data.into(),
            ..Default::default()
        }
    }

    /// An IPv4-mapped network the daemon reads as `/(prefix - 96)` of IPv4
    /// doesn't narrow when that is `/0` (re-review, defence in depth:
    /// `validate_operator` refuses these anyway).
    #[test]
    fn a_mapped_network_wider_than_ipv4_does_not_narrow() {
        for data in ["::ffff:0:0/96", "::ffff:0.0.0.0/90", "0.0.0.0/0", "::/0"] {
            assert!(!narrows(&leaf("network", "dest.network", data)), "{data}");
        }
        for data in ["::ffff:10.0.0.0/104", "10.0.0.0/8", "2001:db8::/32"] {
            assert!(narrows(&leaf("network", "dest.network", data)), "{data}");
        }
    }

    /// Re-review: a pid names an arbitrary process after a reboot, and the
    /// daemon's "Kernel connection" placeholder names none.
    #[test]
    fn only_a_real_program_identity_ties_a_rule_to_programs() {
        assert!(binds_to_programs(&leaf(
            "simple",
            "process.path",
            "/usr/bin/curl"
        )));
        assert!(binds_to_programs(&leaf(
            "simple",
            "process.command",
            "curl x"
        )));
        for (operand, data) in [
            ("process.id", "4242"),
            ("process.path", "Kernel connection"),
            ("process.path", "/proc/self/exe"),
        ] {
            assert!(
                !binds_to_programs(&leaf("simple", operand, data)),
                "{operand}={data}"
            );
        }
    }
}
