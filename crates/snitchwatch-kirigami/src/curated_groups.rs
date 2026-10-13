//! The grouped view of the recommended rules (`curatedGroupsV1`), Qt-free.
//!
//! The bridge sends its groups (id, title, description, in display order)
//! and tags each entry with its group. Every text here is the bridge's own;
//! the only fixed words are for entries the bridge left in no listed group.
//! A group is on only when every entry in it is on. The broad DNS entry is
//! alone in its group (bridge guarantee) and is never turned on by a group
//! request: the model routes it through the reviewed consent flow instead.

use serde_json::{json, Value};
use snitchwatch_bridge::curated::reconcile::EntryStatus;
use snitchwatch_bridge::curated::wire::CuratedDefaultSummary;
use snitchwatch_bridge::curated::CuratedGroup;

use crate::curated_broad_consent::DNS_ID;

/// Title of the catch-all for entries whose group the bridge didn't list.
pub const OTHER_TITLE: &str = "Other recommended rules";
/// Fixed description of that catch-all.
pub const OTHER_DESCRIPTION: &str = "Offered by your Snitchwatch service without a group.";

/// Whether `entry` allows more than a named place (or is the DNS rule).
pub fn is_broad(entry: &CuratedDefaultSummary) -> bool {
    entry.broad || entry.id == DNS_ID
}

/// The listed group `entry` belongs to; empty when it has none listed.
pub fn group_key<'a>(groups: &[CuratedGroup], entry: &'a CuratedDefaultSummary) -> &'a str {
    if groups.iter().any(|g| g.id == entry.group) {
        &entry.group
    } else {
        ""
    }
}

/// Entries in the groups' display order (unlisted ones last), otherwise as
/// the bridge sent them.
pub fn in_group_order(
    groups: &[CuratedGroup],
    mut entries: Vec<CuratedDefaultSummary>,
) -> Vec<CuratedDefaultSummary> {
    let rank = |e: &CuratedDefaultSummary| {
        groups
            .iter()
            .position(|g| g.id == e.group)
            .unwrap_or(usize::MAX)
    };
    entries.sort_by_key(rank);
    entries
}

/// The title of the group `entry` is shown under.
pub fn title_of<'a>(groups: &'a [CuratedGroup], entry: &CuratedDefaultSummary) -> &'a str {
    groups
        .iter()
        .find(|g| g.id == entry.group)
        .map_or(OTHER_TITLE, |g| g.title.as_str())
}

/// The entries of group `gid` (`""` = the unlisted ones).
pub fn entries_of<'a>(
    groups: &[CuratedGroup],
    entries: &'a [CuratedDefaultSummary],
    gid: &str,
) -> Vec<&'a CuratedDefaultSummary> {
    entries
        .iter()
        .filter(|e| group_key(groups, e) == gid)
        .collect()
}

/// The groups as JSON for the page: an array of `{id, title, description,
/// isOn, isBroad, entries: [{entryId, program, allows, why, isOn, statusText,
/// problem}]}`. Groups with no entries are left out.
pub fn view_json(
    groups: &[CuratedGroup],
    entries: &[CuratedDefaultSummary],
    status_text: fn(EntryStatus) -> &'static str,
) -> String {
    let unlisted = (OTHER_TITLE, OTHER_DESCRIPTION, "");
    let listed = groups
        .iter()
        .map(|g| (g.title.as_str(), g.description.as_str(), g.id.as_str()));
    let views: Vec<Value> = listed
        .chain(std::iter::once(unlisted))
        .filter_map(|(title, description, gid)| {
            let members = entries_of(groups, entries, gid);
            (!members.is_empty()).then(|| {
                json!({
                    "id": gid,
                    "title": title,
                    "description": description,
                    "isOn": members.iter().all(|e| e.on),
                    "isBroad": members.iter().any(|e| is_broad(e)),
                    "entries": members.iter().map(|e| json!({
                        "entryId": e.id,
                        "program": e.program,
                        "allows": e.allows,
                        "why": e.why,
                        "isOn": e.on,
                        "statusText": status_text(e.status),
                        "problem": e.problem.as_deref().unwrap_or_default(),
                    })).collect::<Vec<_>>(),
                })
            })
        })
        .collect();
    Value::Array(views).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curated_defaults::status_text;

    fn group(id: &str) -> CuratedGroup {
        CuratedGroup {
            id: id.into(),
            title: format!("Title {id}"),
            description: format!("About {id}."),
        }
    }

    fn entry(id: &str, group: &str, on: bool) -> CuratedDefaultSummary {
        CuratedDefaultSummary {
            id: id.into(),
            group: group.into(),
            program: format!("/usr/bin/{id}"),
            allows: format!("{id} may connect somewhere."),
            why: format!("{id} needs it."),
            on,
            broad: false,
            status: EntryStatus::Off,
            problem: None,
        }
    }

    #[test]
    fn entries_follow_the_groups_display_order_and_unlisted_ones_come_last() {
        let groups = [group("a"), group("b")];
        let sent = vec![
            entry("x", "b", false),
            entry("lost", "gone", false),
            entry("y", "a", false),
            entry("z", "b", false),
        ];
        let ids: Vec<String> = in_group_order(&groups, sent)
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, ["y", "x", "z", "lost"]);
    }

    #[test]
    fn the_view_uses_the_bridges_words_and_a_group_is_on_only_when_all_are() {
        let groups = [group("a"), group("b"), group("empty")];
        let entries = [
            entry("one", "a", true),
            entry("two", "a", false),
            entry("three", "b", true),
            entry("lost", "gone", false),
        ];
        let v: Value = serde_json::from_str(&view_json(&groups, &entries, status_text)).unwrap();
        let v = v.as_array().unwrap();
        assert_eq!(v.len(), 3, "empty group left out, unlisted one shown");
        assert_eq!(v[0]["title"], "Title a");
        assert_eq!(v[0]["description"], "About a.");
        assert_eq!(v[0]["isOn"], false);
        assert_eq!(v[0]["entries"].as_array().unwrap().len(), 2);
        assert_eq!(v[0]["entries"][0]["program"], "/usr/bin/one");
        assert_eq!(v[0]["entries"][0]["allows"], "one may connect somewhere.");
        assert_eq!(v[1]["isOn"], true);
        assert_eq!(v[2]["title"], OTHER_TITLE);
        assert_eq!(v[2]["id"], "");
    }

    #[test]
    fn the_dns_entry_marks_its_group_broad() {
        let groups = [group("name-resolution")];
        let entries = [entry(DNS_ID, "name-resolution", false)];
        let v: Value = serde_json::from_str(&view_json(&groups, &entries, status_text)).unwrap();
        assert_eq!(v[0]["isBroad"], true);
    }
}
