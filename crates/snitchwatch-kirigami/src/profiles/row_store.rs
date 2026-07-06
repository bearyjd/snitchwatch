//! Pure, Qt-free store for the Profiles tab's flat profile list.
//!
//! Mirrors `blocklists::row_store::SubscriptionsStore`: profile changes are
//! low-frequency whole-list replaces/upserts, not a hot per-row stream, so
//! this store reports a single "did anything change" boolean and the model
//! wrapper brackets each apply with `beginResetModel`/`endResetModel`.

use snitchwatch_bridge::ws_messages::{ProfileSummary, ServerMessage};

/// One profile row as rendered by the Profiles tab. `network_matchers` is
/// kept as the `Vec<String>` the wire type carries; the model wrapper joins
/// it into a single comma-separated `QString` role for the simple
/// string-list editor the page uses (see `ProfilesModel::role_names`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    pub network_matchers: Vec<String>,
    pub active: bool,
}

impl From<ProfileSummary> for ProfileRow {
    fn from(p: ProfileSummary) -> Self {
        Self {
            id: p.id,
            name: p.name,
            network_matchers: p.network_matchers,
            active: p.active,
        }
    }
}

#[derive(Debug, Default)]
pub struct ProfilesStore {
    profiles: Vec<ProfileRow>,
}

impl ProfilesStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn row(&self, index: usize) -> Option<&ProfileRow> {
        self.profiles.get(index)
    }

    pub fn find_by_id(&self, id: &str) -> Option<&ProfileRow> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.id.clone()).collect()
    }

    /// Apply one bridge message. Returns `true` if the profile list changed
    /// (the model wrapper resets on `true`).
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetProfiles { profiles } => {
                self.profiles = profiles.iter().cloned().map(ProfileRow::from).collect();
                true
            }
            ServerMessage::ProfileChanged { active_profile_id } => {
                let mut changed = false;
                for p in &mut self.profiles {
                    let should_be_active = active_profile_id.as_deref() == Some(p.id.as_str());
                    if p.active != should_be_active {
                        p.active = should_be_active;
                        changed = true;
                    }
                }
                changed
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::ProfileRuleWire;

    fn summary(id: &str, name: &str, matchers: &[&str], active: bool) -> ProfileSummary {
        ProfileSummary {
            id: id.to_string(),
            name: name.to_string(),
            network_matchers: matchers.iter().map(|s| s.to_string()).collect(),
            rules: vec![ProfileRuleWire {
                id: "r1".into(),
                action: "allow".into(),
                operand: "dest.host".into(),
                data: "nas.local".into(),
            }],
            active,
        }
    }

    #[test]
    fn set_profiles_replaces_the_list() {
        let mut s = ProfilesStore::new();
        assert!(s.apply(&ServerMessage::SetProfiles {
            profiles: vec![summary("home", "Home", &["Home*"], false)]
        }));
        assert_eq!(s.len(), 1);
        assert_eq!(s.row(0).unwrap().id, "home");

        assert!(s.apply(&ServerMessage::SetProfiles {
            profiles: vec![summary("office", "Office", &[], true)]
        }));
        assert_eq!(s.len(), 1);
        assert_eq!(s.row(0).unwrap().id, "office");
    }

    #[test]
    fn profile_changed_updates_active_flag_only() {
        let mut s = ProfilesStore::new();
        s.apply(&ServerMessage::SetProfiles {
            profiles: vec![
                summary("home", "Home", &["Home*"], true),
                summary("office", "Office", &["Office*"], false),
            ],
        });
        assert!(s.apply(&ServerMessage::ProfileChanged {
            active_profile_id: Some("office".into())
        }));
        assert!(!s.find_by_id("home").unwrap().active);
        assert!(s.find_by_id("office").unwrap().active);
    }

    #[test]
    fn profile_changed_none_clears_every_active_flag() {
        let mut s = ProfilesStore::new();
        s.apply(&ServerMessage::SetProfiles {
            profiles: vec![summary("home", "Home", &[], true)],
        });
        assert!(s.apply(&ServerMessage::ProfileChanged {
            active_profile_id: None
        }));
        assert!(!s.find_by_id("home").unwrap().active);
    }

    #[test]
    fn profile_changed_is_noop_when_nothing_changes() {
        let mut s = ProfilesStore::new();
        s.apply(&ServerMessage::SetProfiles {
            profiles: vec![summary("home", "Home", &[], true)],
        });
        assert!(!s.apply(&ServerMessage::ProfileChanged {
            active_profile_id: Some("home".into())
        }));
    }

    #[test]
    fn unrelated_message_does_not_change_profiles() {
        let mut s = ProfilesStore::new();
        assert!(!s.apply(&ServerMessage::ClearConnectionRows));
    }

    #[test]
    fn find_by_id_and_ids_reflect_current_list() {
        let mut s = ProfilesStore::new();
        s.apply(&ServerMessage::SetProfiles {
            profiles: vec![
                summary("home", "Home", &[], false),
                summary("office", "Office", &[], false),
            ],
        });
        assert_eq!(s.ids(), vec!["home".to_string(), "office".to_string()]);
        assert!(s.find_by_id("office").is_some());
        assert!(s.find_by_id("nope").is_none());
    }
}
