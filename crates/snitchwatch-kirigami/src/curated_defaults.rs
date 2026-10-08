//! The recommended background-service rules page's state (prompt-slot D),
//! folded from the bridge's `SetCuratedDefaults`. Qt-free; the cxx-qt
//! wrapper is `curated_defaults_model`.
//!
//! Every text the page shows is either the bridge's (each entry's program,
//! what it allows and why, from the reviewed data file; the reasons) or a
//! fixed sentence from here. Nothing is on unless the bridge says the user
//! turned it on.

use snitchwatch_bridge::curated::reconcile::EntryStatus;
use snitchwatch_bridge::curated::wire::CuratedDefaultSummary;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

/// What the page knows.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CuratedStore {
    /// A `SetCuratedDefaults` has arrived: the bridge offers them.
    received: bool,
    entries: Vec<CuratedDefaultSummary>,
    unavailable: Option<String>,
    storage_persistent: bool,
    storage_reason: String,
}

impl CuratedStore {
    /// Take `msg` if it is a `SetCuratedDefaults`; true if it was.
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        let ServerMessage::SetCuratedDefaults {
            entries,
            storage,
            unavailable,
        } = msg
        else {
            return false;
        };
        *self = Self {
            received: true,
            entries: entries.clone(),
            unavailable: unavailable.clone(),
            storage_persistent: storage.persistent,
            storage_reason: storage.reason.clone().unwrap_or_default(),
        };
        true
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

    /// Why this bridge never adds them; empty when it does.
    pub fn unavailable_reason(&self) -> &str {
        self.unavailable.as_deref().unwrap_or_default()
    }

    /// The user's choices are lost on restart (and the bridge still adds
    /// rules): the save failed after it started.
    pub fn choices_not_saved(&self) -> bool {
        self.received && self.unavailable.is_none() && !self.storage_persistent
    }

    pub fn storage_reason(&self) -> &str {
        &self.storage_reason
    }

    /// The request turning `ids` on or off; `None` while the bridge hasn't
    /// offered them or never adds them, or for an id it didn't list.
    pub fn request(&self, ids: &[&str], on: bool) -> Option<ClientMessage> {
        let usable = self.received && self.unavailable.is_none() && !ids.is_empty();
        let known = ids
            .iter()
            .all(|id| self.entries.iter().any(|e| e.id == *id));
        (usable && known).then(|| ClientMessage::SetCuratedDefaults {
            ids: ids.iter().map(|id| id.to_string()).collect(),
            on,
        })
    }

    /// Every listed id, for "Turn all on/off".
    pub fn ids(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.id.as_str()).collect()
    }
}

/// Where an entry stands, in one fixed sentence.
pub fn status_text(status: EntryStatus) -> &'static str {
    match status {
        EntryStatus::Waiting => "Waiting for the firewall service's rule list.",
        EntryStatus::Unavailable => "Not added by this Snitchwatch service.",
        EntryStatus::Off => "Off.",
        EntryStatus::Installing => "Adding the rule…",
        EntryStatus::Installed => "Rule installed.",
        EntryStatus::InstalledButOff => "Rule installed, but turned off on the Rules page.",
        EntryStatus::Removing => "Removing the rule…",
        EntryStatus::EditedByYou => "Edited by you; Snitchwatch won't change it.",
        EntryStatus::DeletedOutside => {
            "You deleted this rule, so Snitchwatch won't add it again. Turn this off and on \
             again to add it back."
        }
        EntryStatus::NotInstalled => "Not installed.",
        EntryStatus::NotRemoved => "Not removed.",
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
            status,
            problem: None,
        }
    }

    fn message(unavailable: Option<&str>, persistent: bool) -> ServerMessage {
        ServerMessage::SetCuratedDefaults {
            entries: vec![
                entry("flatpak-flathub", false, EntryStatus::Off),
                entry("chronyc-local", true, EntryStatus::Installed),
            ],
            storage: StorageStatus {
                persistent,
                reason: (!persistent).then(|| "disk full".to_string()),
                unreadable: false,
            },
            unavailable: unavailable.map(str::to_string),
        }
    }

    #[test]
    fn nothing_is_offered_until_the_bridge_says_so() {
        let mut store = CuratedStore::default();
        assert!(!store.received());
        assert!(store.request(&["flatpak-flathub"], true).is_none());
        assert!(!store.apply(&ServerMessage::ClearConnectionRows));
        assert!(store.apply(&message(None, true)));
        assert!(store.received());
        assert_eq!(store.len(), 2);
        assert!(!store.row(0).unwrap().on, "off unless the bridge says on");
        assert_eq!(
            store.request(&["flatpak-flathub"], true),
            Some(ClientMessage::SetCuratedDefaults {
                ids: vec!["flatpak-flathub".into()],
                on: true
            })
        );
        assert_eq!(store.ids(), ["flatpak-flathub", "chronyc-local"]);
        assert!(store.request(&["unknown"], true).is_none());
        assert!(store.request(&[], true).is_none());
        assert!(!store.choices_not_saved());
    }

    #[test]
    fn a_bridge_that_never_adds_them_takes_no_request_and_says_why() {
        let mut store = CuratedStore::default();
        store.apply(&message(Some("Needs the system service."), false));
        assert_eq!(store.unavailable_reason(), "Needs the system service.");
        assert!(store.request(&["flatpak-flathub"], true).is_none());
        assert!(!store.choices_not_saved(), "the reason already says why");
        store.apply(&message(None, false));
        assert!(store.choices_not_saved());
        assert_eq!(store.storage_reason(), "disk full");
    }

    #[test]
    fn every_status_has_its_own_fixed_sentence() {
        let all = [
            EntryStatus::Waiting,
            EntryStatus::Unavailable,
            EntryStatus::Off,
            EntryStatus::Installing,
            EntryStatus::Installed,
            EntryStatus::InstalledButOff,
            EntryStatus::Removing,
            EntryStatus::EditedByYou,
            EntryStatus::DeletedOutside,
            EntryStatus::NotInstalled,
            EntryStatus::NotRemoved,
            EntryStatus::Unknown,
        ];
        let texts: std::collections::BTreeSet<&str> = all.iter().map(|s| status_text(*s)).collect();
        assert_eq!(texts.len(), all.len());
        assert_eq!(status_text(EntryStatus::Installed), "Rule installed.");
        assert!(status_text(EntryStatus::EditedByYou).starts_with("Edited by you"));
    }
}
