//! What the page says about the record as a whole (facts, no verdicts).

use crate::learned::{Aggregate, MAX_DIR_PROGRAMS};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notices {
    pub programs: usize,
    /// Connections were missed under load.
    pub incomplete: bool,
    /// Programs refused or evicted, and how many of them carried a flag.
    pub dropped: u64,
    pub dropped_flagged: usize,
    /// The flagged class is full, so newcomers are refused.
    pub class_a_full: bool,
    /// Directory entries that list the most programs they hold.
    pub dirs_full: usize,
    /// Connections of programs a full directory entry only counted.
    pub dir_unlisted_connections: u64,
    /// Whether the rules list was read (else nothing is skipped as present
    /// here; the sequencer still skips identical rules).
    pub rules_read: bool,
}

impl Notices {
    pub fn of(aggregate: &Aggregate, rules_read: bool) -> Self {
        Self {
            programs: aggregate.programs_seen(),
            incomplete: aggregate.incomplete,
            dropped: aggregate.dropped(),
            dropped_flagged: aggregate.ring.iter().filter(|d| d.flagged).count(),
            class_a_full: aggregate.a.len() >= crate::learned::CAP_A,
            dirs_full: aggregate
                .b
                .values()
                .filter(|dir| dir.programs.len() >= MAX_DIR_PROGRAMS)
                .count(),
            dir_unlisted_connections: aggregate
                .b
                .values()
                .map(|dir| dir.unlisted_connections)
                .fold(0, u64::saturating_add),
            rules_read,
        }
    }
}
