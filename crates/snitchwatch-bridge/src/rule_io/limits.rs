//! Whether an import keeps the daemon's rule list loadable (P2.7 review M3).
//!
//! The bridge stages at most [`MAX_SNAPSHOT_RULES`] rules from the daemon's
//! `Subscribe` snapshot, and tonic refuses a message over
//! `grpc_server::MAX_DAEMON_MESSAGE_BYTES`: an import that grows the list
//! past either would leave the Rules page empty at the next reconnect. The
//! size is estimated conservatively from each rule's protobuf size plus a
//! per-rule allowance for framing and the daemon's own fields, and a margin
//! for the rest of `ClientConfig`.

use super::PreviewError;
use crate::cache::rules::MAX_SNAPSHOT_RULES;
use snitchwatch_proto::protocol::Rule;
use std::collections::BTreeMap;

/// Framing and fields the daemon adds per rule (tag, length, `created`).
const PER_RULE_BYTES: usize = 16;
/// The rest of `ClientConfig` (its config JSON, name, version…).
const SNAPSHOT_MARGIN_BYTES: usize = 256 * 1024;

fn framed(rule: &Rule) -> usize {
    prost::Message::encoded_len(rule) + PER_RULE_BYTES
}

/// Refuse when adding or replacing `applicable` would make the daemon's
/// snapshot too long or too large.
pub(super) fn check_totals(
    cached: &BTreeMap<String, Rule>,
    left_out: &BTreeMap<String, usize>,
    applicable: &BTreeMap<String, Rule>,
) -> Result<(), PreviewError> {
    let adds = applicable
        .keys()
        .filter(|name| !cached.contains_key(*name))
        .count();
    if cached.len() + left_out.len() + adds > MAX_SNAPSHOT_RULES {
        return Err(PreviewError::TooManyRules);
    }
    let current: usize = cached.values().map(framed).sum::<usize>()
        + left_out
            .values()
            .map(|size| size + PER_RULE_BYTES)
            .sum::<usize>();
    let growth: usize = applicable
        .iter()
        .map(|(name, new)| {
            let old = cached.get(name).map_or(0, framed);
            framed(new).saturating_sub(old)
        })
        .sum();
    let limit = crate::grpc_server::MAX_DAEMON_MESSAGE_BYTES - SNAPSHOT_MARGIN_BYTES;
    if current + growth > limit {
        return Err(PreviewError::SnapshotTooLarge);
    }
    Ok(())
}
