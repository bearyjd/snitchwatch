//! Why a previewed change starts unticked (P2.7 review H2), in plain words.
//!
//! An add or replace starts ticked unless it can let more traffic through,
//! or quietly changes a rule that blocks traffic:
//!
//! - any allow that overrides other rules (`precedence`) where the rule it
//!   replaces didn't, or that applies to every app;
//! - a replace of a deny/reject that turns it into an allow, turns it on or
//!   off, changes its conditions, or changes how long it lasts (`always` to
//!   `until restart` deletes its file on the daemon host);
//! - a replace of an allow that changes its conditions, turns it on, or
//!   stops its logging.
//!
//! Tightening (allow to deny) and cosmetic changes (a description, deny to
//! reject) start ticked.

use super::preview::PreviousRule;
use crate::translator::verdict::strip_display_hazards;
use snitchwatch_proto::protocol::{Operator, Rule};

const ALLOW_OVERRIDES: &str = "This allow overrides other rules, including ones that block.";
const ALLOW_ALL_APPS: &str = "This allow applies to every app.";
const DENY_TO_ALLOW: &str = "This turns a blocking rule into an allow.";
const DENY_OFF: &str = "This turns a blocking rule off.";
const DENY_ON: &str = "This turns a blocking rule on.";
const DENY_CONDITIONS: &str = "This changes what a blocking rule matches.";
const ALLOW_CONDITIONS: &str = "This changes what an allow rule matches.";
const ALLOW_ON: &str = "This turns on an allow rule that was off.";
const ALLOW_UNLOGGED: &str = "This stops logging the connections an allow rule lets through.";

fn blocks(rule: &Rule) -> bool {
    matches!(rule.action.as_str(), "deny" | "reject")
}

/// `duration` in words.
pub(super) fn lasting(duration: &str) -> &'static str {
    match duration {
        "always" => "always",
        "until restart" => "until the firewall restarts",
        "once" => "once",
        _ => "for a limited time",
    }
}

/// The cautions for importing `new` over `old` (`None`: an add).
/// `same_conditions` compares the normalised operators.
pub(super) fn cautions(
    old: Option<&Rule>,
    new: &Rule,
    all_apps: bool,
    same_conditions: bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let allow = new.action == "allow";
    let overrode = old.is_some_and(|o| o.action == "allow" && o.precedence);
    if allow && new.precedence && !overrode {
        out.push(ALLOW_OVERRIDES.into());
    }
    if allow && all_apps {
        out.push(ALLOW_ALL_APPS.into());
    }
    let Some(old) = old else { return out };
    if blocks(old) {
        if allow {
            out.push(DENY_TO_ALLOW.into());
        }
        match (old.enabled, new.enabled) {
            (true, false) => out.push(DENY_OFF.into()),
            (false, true) => out.push(DENY_ON.into()),
            _ => {}
        }
        if !same_conditions {
            out.push(DENY_CONDITIONS.into());
        }
        if old.duration != new.duration {
            out.push(format!(
                "This changes how long a blocking rule lasts, from {} to {}.",
                lasting(&old.duration),
                lasting(&new.duration)
            ));
        }
    } else if old.action == "allow" && allow {
        if !same_conditions {
            out.push(ALLOW_CONDITIONS.into());
        }
        if !old.enabled && new.enabled {
            out.push(ALLOW_ON.into());
        }
        if new.nolog && !old.nolog {
            out.push(ALLOW_UNLOGGED.into());
        }
    }
    out
}

/// A changed field, in plain words.
pub(super) fn plain_field(field: &str) -> &'static str {
    match field {
        "enabled" => "on or off",
        "action" => "action",
        "duration" => "how long it lasts",
        "description" => "description",
        "precedence" => "whether it overrides other rules",
        "nolog" => "logging",
        _ => "conditions",
    }
}

/// `old` as the preview shows it.
pub(super) fn previous(old: &Rule, conditions: impl Fn(&Operator) -> Vec<String>) -> PreviousRule {
    PreviousRule {
        enabled: old.enabled,
        action: old.action.clone(),
        duration: old.duration.clone(),
        description: strip_display_hazards(&old.description),
        precedence: old.precedence,
        nolog: old.nolog,
        conditions: old.operator.as_ref().map(conditions).unwrap_or_default(),
    }
}
