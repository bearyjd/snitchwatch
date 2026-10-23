//! From the recorded programs to proposals.

use super::directory::directory_rule;
use super::dual_use::is_dual_use;
use super::flags::is_system_service;
use super::paths::{basename, is_direct_system_path, is_system_path, selectability};
use super::present::is_present;
use super::rule::{is_usable_host, rules_for};
use super::types::{
    Choice, ChoiceKind, Destinations, Flags, ProgramProposal, ReviewConfig, BLOCK_WARNING,
    DUAL_USE_WARNING, SUMMARY_TOP,
};
use crate::app_names::{friendly_name, sanitize_display};
use crate::learned::{Aggregate, Program};
use crate::rules::row_store::Rule;

/// A flat copy of every program in `aggregate`: its three keyed classes and
/// the programs of its per-directory entries.
pub fn programs_of(aggregate: &Aggregate) -> Vec<Program> {
    aggregate
        .a
        .values()
        .chain(aggregate.b.values().flat_map(|dir| dir.programs.values()))
        .chain(aggregate.c.values())
        .chain(aggregate.d.values())
        .cloned()
        .collect()
}

/// The proposals for `programs`, busiest first (then most recent, then by
/// key). `rules` is the committed rule list, for the best-effort skip of a
/// proposal an enabled rule already makes. Nothing is ticked by default.
pub fn build_review(
    programs: &[Program],
    rules: &[Rule],
    config: &ReviewConfig,
) -> Vec<ProgramProposal> {
    let mut proposals: Vec<ProgramProposal> = programs
        .iter()
        .map(|program| proposal(program, rules, config))
        .collect();
    proposals.sort_by(|a, b| {
        b.connections
            .cmp(&a.connections)
            .then_with(|| b.last_seen_ms.cmp(&a.last_seen_ms))
            .then_with(|| a.key.cmp(&b.key))
    });
    proposals
}

/// `/a/b/c/d` as `…/b/c/d`; a short path as it is.
fn tail_of(shown: &str) -> String {
    let parts: Vec<&str> = shown.split('/').filter(|s| !s.is_empty()).collect();
    let tail = if shown.starts_with('/') && parts.len() > 3 {
        format!("…/{}", parts[parts.len() - 3..].join("/"))
    } else {
        shown.to_string()
    };
    sanitize_display(&tail)
}

/// The destinations that get a rule under "only these destinations": seen
/// at least `min_host_sightings` times, busiest first; none when there are
/// none or more than `max_hosts` (never a silent cut).
fn rule_hosts(sorted: &[(String, u64)], config: &ReviewConfig) -> Option<Vec<String>> {
    let hosts: Vec<String> = sorted
        .iter()
        .filter(|(host, count)| *count >= config.min_host_sightings && is_usable_host(host))
        .map(|(host, _)| host.clone())
        .collect();
    (!hosts.is_empty() && hosts.len() <= config.max_hosts).then_some(hosts)
}

fn offered_choices(program: &Program, hosts: Option<Vec<String>>) -> Vec<Choice> {
    let mut choices: Vec<Choice> = hosts.into_iter().map(Choice::AllowHosts).collect();
    choices.push(Choice::AllowAny);
    choices.push(Choice::Block);
    choices.extend(directory_rule(program).map(Choice::DirectoryRule));
    choices
}

fn is_covered(choice: &Choice, program: &Program, rules: &[Rule]) -> bool {
    rules_for(choice, program)
        .is_ok_and(|payloads| !payloads.is_empty() && payloads.iter().all(|p| is_present(p, rules)))
}

fn proposal(program: &Program, rules: &[Rule], config: &ReviewConfig) -> ProgramProposal {
    // The label never comes from a desktop entry: not an input to anything.
    let name = friendly_name(program.raw_path.as_deref(), &program.raw_name, None);
    let flags = Flags::of(program);
    let dual_use = is_dual_use(program);

    let mut sorted: Vec<(String, u64)> = program
        .destinations
        .iter()
        .map(|(host, count)| (host.clone(), *count))
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    // `dest_overflow` counts connections, not hosts: this is an upper bound.
    let rest = usize::try_from(program.dest_overflow).unwrap_or(usize::MAX);
    let fan_out = program.destinations.len().saturating_add(rest);
    let high_fan_out = fan_out >= config.fan_out_threshold;

    let selectable = selectability(program);
    let (choices, already_present) = match selectable {
        Err(why) => (vec![Choice::NonSelectable(why)], Vec::new()),
        Ok(_) => {
            let hosts = if high_fan_out {
                None
            } else {
                rule_hosts(&sorted, config)
            };
            offered_choices(program, hosts)
                .into_iter()
                .partition(|choice| !is_covered(choice, program, rules))
        }
    };
    let direct = selectable.is_ok_and(is_direct_system_path);
    let has_hosts = choices.iter().any(|c| c.kind() == ChoiceKind::AllowHosts);
    let selected_by_suggest = has_hosts && direct && !flags.any() && !dual_use && !high_fan_out;

    let block_warning = selectable
        .ok()
        .filter(|path| {
            is_system_path(path)
                || is_system_service(&program.raw_name)
                || is_system_service(basename(path))
        })
        .map(|_| BLOCK_WARNING.to_string());
    let allow_any_warning = (dual_use && selectable.is_ok()).then(|| DUAL_USE_WARNING.to_string());

    let shown_source = program.raw_path.as_deref().unwrap_or(&program.raw_name);
    ProgramProposal {
        key: program.key.clone(),
        marker: name.needs_marker(),
        name,
        display: program.display.clone(),
        path_tail: tail_of(shown_source),
        flags,
        connections: program.connections,
        first_seen_ms: program.first_seen_ms,
        last_seen_ms: program.last_seen_ms,
        destinations: Destinations {
            connections: program.connections,
            distinct: fan_out,
            exact: program.dest_overflow == 0,
            top: if high_fan_out {
                Vec::new()
            } else {
                sorted.iter().take(SUMMARY_TOP).cloned().collect()
            },
            hidden: high_fan_out,
        },
        fan_out,
        high_fan_out,
        dual_use,
        choices,
        already_present,
        selected_by_suggest,
        block_warning,
        allow_any_warning,
        all_destinations: sorted,
    }
}
