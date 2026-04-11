//! Rule specificity scoring.
//!
//! LS implicitly resolves rule conflicts by "most specific wins". OpenSnitch
//! evaluates rules in alphabetical order by filename. The bridge translates
//! LS specificity into explicit alphabetical prefixes.
//!
//! Score formula:
//!   100 * has(process)
//! +  50 * has(remote_host_exact)
//! +  30 * has(remote_host_glob)
//! +  40 * has(port)
//! +  20 * has(protocol)
//!
//! Filename = f"{999 - score:03d}-{slug}.json"
//! Higher specificity → lower number → evaluated first.
//!
//! Blocklist rules use an alpha prefix ("z00".."z99") so every blocklist
//! filename sorts AFTER every user filename — user rules always win. See
//! `BLOCKLIST_BAND_PREFIX`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecificityInputs {
    pub has_process: bool,
    pub has_remote_host_exact: bool,
    pub has_remote_host_glob: bool,
    pub has_port: bool,
    pub has_protocol: bool,
}

pub const SCORE_PROCESS: u32 = 100;
pub const SCORE_HOST_EXACT: u32 = 50;
pub const SCORE_HOST_GLOB: u32 = 30;
pub const SCORE_PORT: u32 = 40;
pub const SCORE_PROTOCOL: u32 = 20;
pub const SCORE_MAX: u32 = SCORE_PROCESS + SCORE_HOST_EXACT + SCORE_PORT + SCORE_PROTOCOL; // 210

/// Filename prefix used for blocklist rules. Alpha char ('z') sorts after
/// any 3-digit numeric prefix used by user rules ("000".."999"), so every
/// blocklist filename is evaluated *after* every user filename. This is
/// how user rules always win conflicts with blocklists.
pub const BLOCKLIST_BAND_PREFIX: &str = "z";

pub fn score(inputs: &SpecificityInputs) -> u32 {
    let mut s = 0;
    if inputs.has_process {
        s += SCORE_PROCESS;
    }
    if inputs.has_remote_host_exact {
        s += SCORE_HOST_EXACT;
    } else if inputs.has_remote_host_glob {
        s += SCORE_HOST_GLOB;
    }
    if inputs.has_port {
        s += SCORE_PORT;
    }
    if inputs.has_protocol {
        s += SCORE_PROTOCOL;
    }
    s
}

/// Compute a filename prefix for a user rule. Lower prefix = higher priority.
pub fn user_rule_prefix(inputs: &SpecificityInputs) -> String {
    let s = score(inputs).min(999);
    format!("{:03}", 999u32.saturating_sub(s))
}

/// Compute a filename prefix for a blocklist rule. Always starts with the
/// alpha `BLOCKLIST_BAND_PREFIX` so it sorts after every numeric user prefix.
/// `entry_index` lets multiple blocklist rules order deterministically within
/// the band.
pub fn blocklist_rule_prefix(entry_index: u32) -> String {
    // Cap at 99 so the suffix stays fixed-width and sorts correctly.
    let offset = entry_index.min(99);
    format!("{}{:02}", BLOCKLIST_BAND_PREFIX, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(p: bool, he: bool, hg: bool, port: bool, proto: bool) -> SpecificityInputs {
        SpecificityInputs {
            has_process: p,
            has_remote_host_exact: he,
            has_remote_host_glob: hg,
            has_port: port,
            has_protocol: proto,
        }
    }

    #[test]
    fn empty_rule_scores_zero() {
        assert_eq!(score(&inputs(false, false, false, false, false)), 0);
    }

    #[test]
    fn process_only_scores_100() {
        assert_eq!(score(&inputs(true, false, false, false, false)), 100);
    }

    #[test]
    fn process_plus_exact_host_plus_port_plus_proto() {
        // 100 + 50 + 40 + 20 = 210
        assert_eq!(score(&inputs(true, true, false, true, true)), 210);
    }

    #[test]
    fn exact_host_beats_glob_host() {
        let exact = score(&inputs(true, true, false, true, true));
        let glob = score(&inputs(true, false, true, true, true));
        assert!(exact > glob, "exact should beat glob");
    }

    #[test]
    fn exact_and_glob_set_uses_exact_only() {
        // If both flags are accidentally set, exact wins (we don't double-count).
        let s = score(&inputs(false, true, true, false, false));
        assert_eq!(s, SCORE_HOST_EXACT);
    }

    #[test]
    fn user_prefix_more_specific_sorts_first() {
        let specific = user_rule_prefix(&inputs(true, true, false, true, true));
        let generic = user_rule_prefix(&inputs(true, false, false, false, false));
        assert!(
            specific < generic,
            "{} should sort before {}",
            specific,
            generic
        );
    }

    #[test]
    fn blocklist_prefix_sorts_after_every_user_rule() {
        let p = blocklist_rule_prefix(42);
        assert!(
            p.starts_with(BLOCKLIST_BAND_PREFIX),
            "blocklist prefix should start with {}, got {}",
            BLOCKLIST_BAND_PREFIX,
            p
        );
        // User rules always win: every blocklist prefix sorts AFTER the
        // weakest user prefix ("999"), so user rules are evaluated first.
        let weakest_user = user_rule_prefix(&inputs(false, false, false, false, false));
        assert!(
            blocklist_rule_prefix(0) > weakest_user,
            "blocklist(0)={} must sort after weakest user={}",
            blocklist_rule_prefix(0),
            weakest_user
        );
        // And the strongest user rule as well.
        let strongest_user = user_rule_prefix(&inputs(true, true, false, true, true));
        assert!(blocklist_rule_prefix(99) > strongest_user);
    }

    #[test]
    fn user_prefix_zero_score_is_999() {
        assert_eq!(
            user_rule_prefix(&inputs(false, false, false, false, false)),
            "999"
        );
    }

    #[test]
    fn user_prefix_max_score_is_smallest() {
        let p = user_rule_prefix(&inputs(true, true, false, true, true));
        // 999 - 210 = 789
        assert_eq!(p, "789");
    }
}
