//! List format detection and parsing.
//!
//! We support three real-world blocklist formats:
//!
//! 1. **Hosts** — `0.0.0.0 doubleclick.net` lines (StevenBlack/hosts).
//! 2. **Domains** — bare `doubleclick.net` one-per-line (Pi-hole style).
//! 3. **AdblockPlus** — `||doubleclick.net^` filter rules (EasyList style).
//!
//! `sniff_format` looks at the first ~20 non-comment lines and picks the most
//! likely format. Comments (`#`, `!`) and blank lines are skipped during sniffing
//! and during parsing.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    Hosts,
    Domains,
    AdblockPlus,
}

pub fn sniff_format(body: &str) -> ListFormat {
    let mut hosts_hits = 0u32;
    let mut abp_hits = 0u32;
    let mut domain_hits = 0u32;
    for line in body.lines().take(64) {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with('!')
            || line.starts_with('[')
        {
            continue;
        }
        if line.starts_with("||") && line.contains('^') {
            abp_hits += 1;
        } else if line.starts_with("0.0.0.0") || line.starts_with("127.0.0.1") {
            hosts_hits += 1;
        } else if is_valid_hostname(line) {
            domain_hits += 1;
        }
    }
    if abp_hits >= hosts_hits && abp_hits >= domain_hits && abp_hits > 0 {
        ListFormat::AdblockPlus
    } else if hosts_hits >= domain_hits && hosts_hits > 0 {
        ListFormat::Hosts
    } else {
        ListFormat::Domains
    }
}

pub fn parse(format: ListFormat, body: &str) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with('!')
            || line.starts_with('[')
        {
            continue;
        }
        let host_opt = match format {
            ListFormat::Hosts => parse_hosts_line(line),
            ListFormat::Domains => parse_domains_line(line),
            ListFormat::AdblockPlus => parse_abp_line(line),
        };
        if let Some(host) = host_opt {
            if is_valid_hostname(&host) && !is_local_loopback(&host) && seen.insert(host.clone()) {
                out.push(host);
            }
        }
    }
    out
}

fn parse_hosts_line(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let _ip = parts.next()?;
    let host = parts.next()?;
    Some(host.to_ascii_lowercase())
}

fn parse_domains_line(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?;
    Some(token.to_ascii_lowercase())
}

fn parse_abp_line(line: &str) -> Option<String> {
    let stripped = line.strip_prefix("||")?;
    let end = stripped.find(['^', '$', '/']).unwrap_or(stripped.len());
    let host = &stripped[..end];
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

fn is_valid_hostname(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    s.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    }) && s.contains('.')
}

fn is_local_loopback(host: &str) -> bool {
    matches!(
        host,
        "localhost"
            | "localhost.localdomain"
            | "local"
            | "broadcasthost"
            | "ip6-localhost"
            | "ip6-loopback"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_hosts_format() {
        let body = "127.0.0.1 localhost\n0.0.0.0 doubleclick.net\n0.0.0.0 google-analytics.com\n";
        assert_eq!(sniff_format(body), ListFormat::Hosts);
    }

    #[test]
    fn sniffs_domains_format() {
        let body = "doubleclick.net\ngoogle-analytics.com\nfacebook.net\n";
        assert_eq!(sniff_format(body), ListFormat::Domains);
    }

    #[test]
    fn sniffs_abp_format() {
        let body = "[Adblock Plus 2.0]\n||doubleclick.net^\n||tracker.example^\n";
        assert_eq!(sniff_format(body), ListFormat::AdblockPlus);
    }

    #[test]
    fn parses_hosts_skipping_localhost_and_comments() {
        let body = "# StevenBlack tiny\n127.0.0.1 localhost\n0.0.0.0 doubleclick.net\n0.0.0.0 google-analytics.com\n# trailing comment\n0.0.0.0 facebook.net\n";
        let parsed = parse(ListFormat::Hosts, body);
        assert_eq!(
            parsed,
            vec!["doubleclick.net", "google-analytics.com", "facebook.net"]
        );
    }

    #[test]
    fn parses_domains_one_per_line() {
        let body = "doubleclick.net\n# comment\n\ngoogle-analytics.com\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com"]);
    }

    #[test]
    fn parses_abp_extracts_domain_between_pipes_and_caret() {
        let body =
            "[Adblock Plus 2.0]\n||doubleclick.net^\n!comment\n||tracker.example^$third-party\n";
        let parsed = parse(ListFormat::AdblockPlus, body);
        assert_eq!(parsed, vec!["doubleclick.net", "tracker.example"]);
    }

    #[test]
    fn rejects_invalid_hostnames() {
        let body = "doubleclick.net\nnot a hostname\n   \n--bad--\nvalid.example\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "valid.example"]);
    }

    #[test]
    fn deduplicates_entries() {
        let body = "doubleclick.net\ndoubleclick.net\ngoogle-analytics.com\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com"]);
    }
}
