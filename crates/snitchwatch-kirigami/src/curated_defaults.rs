//! The recommended background-service rules page's state (prompt-slot D),
//! folded from the bridge's `SetCuratedDefaults`. Qt-free; the cxx-qt
//! wrapper is `curated_defaults_model`.
//!
//! Every text the page shows is either the bridge's (each entry's program,
//! what it allows and why, from the reviewed data file; the reasons) or a
//! fixed sentence from here. Nothing reads as on unless the bridge says so:
//! the user turned it on, or its rule was already in the firewall.

use snitchwatch_bridge::curated::reconcile::EntryStatus;
use snitchwatch_bridge::curated::wire::CuratedDefaultSummary;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

/// What the page knows, for one bridge session.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CuratedStore {
    /// The session the state came from; a new session starts empty, so a
    /// bridge without the `curatedDefaults` capability never shows an
    /// earlier bridge's list.
    session: u64,
    /// A `SetCuratedDefaults` has arrived: the bridge offers them.
    received: bool,
    entries: Vec<CuratedDefaultSummary>,
    unavailable: Option<String>,
}

impl CuratedStore {
    /// Take `msg` from session `connection_id`; true if the state changed.
    pub fn apply(&mut self, connection_id: u64, msg: &ServerMessage) -> bool {
        let mut changed = false;
        if connection_id != self.session {
            *self = Self {
                session: connection_id,
                ..Self::default()
            };
            changed = true;
        }
        if let ServerMessage::SetCuratedDefaults {
            entries,
            unavailable,
            ..
        } = msg
        {
            self.received = true;
            self.entries = entries.clone();
            self.unavailable = unavailable.clone();
            changed = true;
        }
        changed
    }

    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn received(&self) -> bool {
        self.received
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn row(&self, index: usize) -> Option<&CuratedDefaultSummary> {
        self.entries.get(index)
    }

    /// Why this bridge changes no recommended rules; empty when it does.
    pub fn unavailable_reason(&self) -> &str {
        self.unavailable.as_deref().unwrap_or_default()
    }

    fn usable(&self) -> bool {
        self.received && self.unavailable.is_none()
    }

    /// Whether `entry` offers Remove: a rule edited outside Snitchwatch.
    pub fn can_remove(&self, entry: &CuratedDefaultSummary) -> bool {
        self.usable() && entry.status == EntryStatus::EditedByYou
    }

    /// Whether `entry` offers Keep: a rule already in the firewall that the
    /// user hasn't chosen yet (the switch alone can only remove it).
    pub fn can_keep(&self, entry: &CuratedDefaultSummary) -> bool {
        self.usable()
            && matches!(
                entry.status,
                EntryStatus::InFirewall | EntryStatus::InFirewallButOff
            )
    }

    /// The request keeping `id`'s rule: it is turned on, so Snitchwatch
    /// adopts the copy already there and sends nothing.
    pub fn keeping(&self, id: &str) -> Option<ClientMessage> {
        self.entries
            .iter()
            .any(|e| e.id == id && self.can_keep(e))
            .then(|| ClientMessage::SetCuratedDefaults {
                ids: vec![id.to_string()],
                on: true,
            })
    }

    /// The request turning `id` on or off; `None` while the bridge hasn't
    /// offered them or changes none, or for an id it didn't list.
    pub fn request(&self, id: &str, on: bool) -> Option<ClientMessage> {
        let known = self.entries.iter().any(|e| e.id == id);
        (self.usable() && known).then(|| ClientMessage::SetCuratedDefaults {
            ids: vec![id.to_string()],
            on,
        })
    }

    /// "Turn all on/off": only the entries not already that way, so an
    /// entry already on isn't asked again. "Turn all on" also keeps a rule
    /// already in the firewall that the user hadn't chosen yet, but never
    /// touches a broad entry (any address, S6): that one is turned on, or
    /// kept, by itself. "Turn all off" includes it, the safe direction.
    pub fn request_all(&self, on: bool) -> Option<ClientMessage> {
        let ids: Vec<String> = self
            .entries
            .iter()
            .filter(|e| asks_for(e, on))
            .map(|e| e.id.clone())
            .collect();
        (self.usable() && !ids.is_empty()).then_some(ClientMessage::SetCuratedDefaults { ids, on })
    }

    /// The request removing `id`'s edited rule, once the user confirmed.
    pub fn removal(&self, id: &str) -> Option<ClientMessage> {
        self.entries
            .iter()
            .any(|e| e.id == id && self.can_remove(e))
            .then(|| ClientMessage::RemoveCuratedDefault { id: id.to_string() })
    }
}

/// Whether "Turn all on/off" asks for `entry`.
fn asks_for(entry: &CuratedDefaultSummary, on: bool) -> bool {
    if on && entry.broad {
        return false;
    }
    entry.on != on || (on && entry.status == EntryStatus::InFirewall)
}

/// Where an entry stands, in one fixed sentence, as of the firewall
/// service's last rule list.
pub fn status_text(status: EntryStatus) -> &'static str {
    match status {
        EntryStatus::Waiting => "Waiting for the firewall service's rule list.",
        EntryStatus::Unavailable => "Not added by this Snitchwatch service.",
        EntryStatus::InFirewall => "In the firewall (added earlier).",
        EntryStatus::InFirewallButOff => {
            "In the firewall (added earlier), turned off on the Rules page."
        }
        EntryStatus::Off => "Off.",
        EntryStatus::Installing => "Adding the rule…",
        EntryStatus::Installed => "Rule installed.",
        EntryStatus::InstalledButOff => "Rule installed, but turned off on the Rules page.",
        EntryStatus::Removing => "Removing the rule…",
        EntryStatus::EditedByYou => {
            "The firewall's rule under this name differs from this description and still \
             applies; see the Rules page."
        }
        EntryStatus::DeletedOutside => {
            "This rule was removed, so Snitchwatch won't add it again until you turn this off \
             and on again."
        }
        EntryStatus::NotInstalled => "Not installed.",
        EntryStatus::NotRemoved => "Not removed.",
        EntryStatus::OffFileLeft => {
            "Turned off, but the firewall service couldn't remove the rule's saved file, so it may \
             come back when the service restarts. If it comes back unchanged, Snitchwatch removes \
             it then."
        }
        EntryStatus::Unknown => "Status unknown to this version of Snitchwatch.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::StorageStatus;

    fn entry(id: &str, on: bool, status: EntryStatus) -> CuratedDefaultSummary {
        CuratedDefaultSummary {
            id: id.into(),
            program: "/usr/bin/flatpak".into(),
            allows: "/usr/bin/flatpak may connect to dl.flathub.org on TCP port 443.".into(),
            why: "Flatpak downloads app updates.".into(),
            on,
            broad: false,
            status,
            problem: None,
        }
    }

    fn broad_entry(id: &str, on: bool, status: EntryStatus) -> CuratedDefaultSummary {
        CuratedDefaultSummary {
            broad: true,
            ..entry(id, on, status)
        }
    }

    fn message_with_broad() -> ServerMessage {
        ServerMessage::SetCuratedDefaults {
            entries: vec![
                entry("flatpak-flathub", false, EntryStatus::Off),
                broad_entry("dns-off", false, EntryStatus::Off),
                broad_entry("dns-adopt", true, EntryStatus::InFirewall),
                broad_entry("dns-on", true, EntryStatus::Installed),
                entry("chronyc-local", true, EntryStatus::Installed),
            ],
            storage: StorageStatus {
                persistent: true,
                reason: None,
                unreadable: false,
            },
            unavailable: None,
        }
    }

    fn message(unavailable: Option<&str>) -> ServerMessage {
        ServerMessage::SetCuratedDefaults {
            entries: vec![
                entry("flatpak-flathub", false, EntryStatus::Off),
                entry("chronyc-local", true, EntryStatus::Installed),
                entry("networkmanager", true, EntryStatus::EditedByYou),
                entry("undecided", true, EntryStatus::InFirewall),
            ],
            storage: StorageStatus {
                persistent: true,
                reason: None,
                unreadable: false,
            },
            unavailable: unavailable.map(str::to_string),
        }
    }

    #[test]
    fn nothing_is_offered_until_the_bridge_says_so() {
        let mut store = CuratedStore::default();
        assert!(!store.received());
        assert!(store.request("flatpak-flathub", true).is_none());
        assert!(store.apply(1, &message(None)));
        assert!(store.received());
        assert_eq!(store.len(), 4);
        assert!(!store.row(0).unwrap().on, "off unless the bridge says on");
        assert_eq!(
            store.request("flatpak-flathub", true),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["flatpak-flathub".into()],
                on: true
            })
        );
        assert!(store.request("unknown", true).is_none());
    }

    /// Code review M1: "Turn all on" asks only for entries that are off.
    #[test]
    fn turn_all_asks_only_for_entries_that_differ() {
        let mut store = CuratedStore::default();
        store.apply(1, &message(None));
        assert_eq!(
            store.request_all(true),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["flatpak-flathub".into(), "undecided".into()],
                on: true
            })
        );
        assert_eq!(
            store.request_all(false),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec![
                    "chronyc-local".into(),
                    "networkmanager".into(),
                    "undecided".into()
                ],
                on: false
            })
        );
    }

    /// S6: "Turn all on" never turns on or adopts a broad (any address)
    /// entry, in either branch, and "Turn all off" does include it.
    #[test]
    fn turn_all_on_never_asks_for_a_broad_entry() {
        let mut store = CuratedStore::default();
        store.apply(1, &message_with_broad());
        // A broad entry that is off, and one already in the firewall that
        // would be adopted: neither is asked for.
        assert_eq!(
            store.request_all(true),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["flatpak-flathub".into()],
                on: true
            })
        );
        // Turn all off: the safe direction, broad entries included.
        assert_eq!(
            store.request_all(false),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["dns-adopt".into(), "dns-on".into(), "chronyc-local".into()],
                on: false
            })
        );
        // Only broad entries left to turn on: nothing to ask.
        let mut only_broad = CuratedStore::default();
        only_broad.apply(
            1,
            &ServerMessage::SetCuratedDefaults {
                entries: vec![
                    broad_entry("dns-off", false, EntryStatus::Off),
                    broad_entry("dns-adopt", true, EntryStatus::InFirewall),
                ],
                storage: StorageStatus {
                    persistent: true,
                    reason: None,
                    unreadable: false,
                },
                unavailable: None,
            },
        );
        assert!(only_broad.request_all(true).is_none());
        // By itself, it still can be turned on and kept.
        assert_eq!(
            only_broad.request("dns-off", true),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["dns-off".into()],
                on: true
            })
        );
        assert_eq!(
            only_broad.keeping("dns-adopt"),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["dns-adopt".into()],
                on: true
            })
        );
    }

    /// Re-review 2, LOW 5: Keep is offered for an undecided rule already in
    /// the firewall, and keeping it is turning it on.
    #[test]
    fn only_an_undecided_rule_can_be_kept() {
        let mut store = CuratedStore::default();
        store.apply(1, &message(None));
        assert_eq!(
            store.keeping("undecided"),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["undecided".into()],
                on: true
            })
        );
        assert!(store.keeping("chronyc-local").is_none());
        store.apply(1, &message(Some("per-user")));
        assert!(store.keeping("undecided").is_none());
    }

    /// Code review M2: Remove is offered for an edited rule only.
    #[test]
    fn only_an_edited_rule_can_be_removed() {
        let mut store = CuratedStore::default();
        store.apply(1, &message(None));
        assert_eq!(
            store.removal("networkmanager"),
            Some(ClientMessage::RemoveCuratedDefault {
                id: "networkmanager".into()
            })
        );
        assert!(store.removal("chronyc-local").is_none());
        assert!(store.removal("unknown").is_none());
        store.apply(1, &message(Some("per-user")));
        assert!(store.removal("networkmanager").is_none());
    }

    #[test]
    fn a_bridge_that_changes_nothing_takes_no_request_and_says_why() {
        let mut store = CuratedStore::default();
        store.apply(1, &message(Some("Needs the system service.")));
        assert_eq!(store.unavailable_reason(), "Needs the system service.");
        assert!(store.request("flatpak-flathub", true).is_none());
        assert!(store.request_all(true).is_none());
    }

    /// A new session starts empty: a bridge without the capability never
    /// shows an earlier bridge's list.
    #[test]
    fn a_new_session_forgets_the_last_ones_list() {
        let mut store = CuratedStore::default();
        store.apply(1, &message(None));
        assert!(store.apply(2, &ServerMessage::ClearConnectionRows));
        assert!(!store.received());
        assert!(store.is_empty());
        assert!(!store.apply(2, &ServerMessage::ClearConnectionRows));
    }

    #[test]
    fn every_status_has_its_own_fixed_sentence() {
        let all = [
            EntryStatus::Waiting,
            EntryStatus::Unavailable,
            EntryStatus::InFirewall,
            EntryStatus::InFirewallButOff,
            EntryStatus::Off,
            EntryStatus::Installing,
            EntryStatus::Installed,
            EntryStatus::InstalledButOff,
            EntryStatus::Removing,
            EntryStatus::EditedByYou,
            EntryStatus::DeletedOutside,
            EntryStatus::NotInstalled,
            EntryStatus::NotRemoved,
            EntryStatus::OffFileLeft,
            EntryStatus::Unknown,
        ];
        let texts: std::collections::BTreeSet<&str> = all.iter().map(|s| status_text(*s)).collect();
        assert_eq!(texts.len(), all.len());
        assert!(status_text(EntryStatus::EditedByYou).contains("still applies"));
        assert!(!status_text(EntryStatus::DeletedOutside).contains("You deleted"));
        let left = status_text(EntryStatus::OffFileLeft);
        assert!(left.starts_with("Turned off") && left.contains("may come back"));
    }
}
