//! Bridge-owned switchable firewall profiles ("At Home" / "Public Wi-Fi" /
//! "Office"), with automatic activation based on the connected network, and
//! enforcement of the active profile's rules (issue #46 Part 2).
//!
//! ## Auto vs. manual activation (issue #82)
//!
//! - [`ProfilesManager::activate`] / [`ProfilesManager::deactivate`] (the
//!   Profiles page) are **manual**: the choice is saved together with the
//!   newest network the bridge has seen ([`store::ManualChoice`]).
//! - Auto-switching ([`ProfilesManager::spawn_auto_switch`]) acts on a
//!   network only once it has stayed the same for a few seconds
//!   ([`tasks::NETWORK_SETTLE`]), and only when it differs from the
//!   last network it acted on.
//! - **A manual choice holds while the observed network is the one it was
//!   made on**, including the first reading after a restart. A different
//!   network clears it, and auto-switching evaluates that network's
//!   matchers from scratch.
//! - If no profile's matchers match a network, the active profile is left
//!   as it is (it does not deactivate to "none").
//!
//! ## Enforcement
//!
//! Profile actions only change the store and ask for an enforcement pass;
//! the pass itself runs on one task ([`ProfilesManager::spawn_enforcer`]),
//! never on the bridge's message pump, and also after every committed
//! daemon rules snapshot. See [`enforcer`] for what a pass does. Each rule
//! of the active profile has a status: pending, "Rule installed" (only
//! after a correlated `OK`, or found in place), or not enforced with the
//! reason. A rule the `ProfileRule` policy refuses is never installed.

pub mod enforcer;
pub mod matcher;
pub mod materializer;
pub mod network_watcher;
pub mod store;
pub mod tasks;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use tokio::sync::{broadcast, Mutex, Notify};
use tracing::{error, info};

pub use crate::profiles::enforcer::{DaemonProfileSink, NoopProfileRuleSink, ProfileRuleSink};

use crate::blocklists::Enforcement;
use crate::profiles::materializer::{materialize_profile, materialize_rule, valid_rule_id};
use crate::profiles::store::{ManualChoice, Profile, ProfileRule, ProfileStore, StoreError};
use crate::rule_policy::RuleProblem;
use crate::ws_messages::StorageStatus;

/// Most rules one profile holds.
pub const MAX_RULES_PER_PROFILE: usize = 64;

/// Why nothing is installed when no sink was wired in (in-process bridges
/// and tests).
pub const NO_SINK_REASON: &str = "Profiles aren't applied to the firewall by this Snitchwatch";

const RULE_ID_REFUSED: &str = "a profile rule's id must be 1 to 64 letters, digits, - or _";
const TOO_MANY_RULES: &str = "a profile holds at most 64 rules";

/// Events emitted whenever profile state changes. The translator subscribes
/// and rebroadcasts as `SetProfiles` / `ProfileChanged` over the WS.
#[derive(Debug, Clone)]
pub enum ProfileEvent {
    ProfilesChanged,
    ActiveProfileChanged { profile_id: Option<String> },
}

#[derive(Debug, thiserror::Error)]
pub enum ProfilesError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    /// A profile rule the bridge won't keep; each reason is plain text.
    #[error("profile rule refused")]
    Refused(Vec<RuleProblem>),
}

/// The active profile's rule statuses, by rule id.
#[derive(Debug, Default, Clone, PartialEq)]
struct Statuses {
    profile_id: Option<String>,
    rules: HashMap<String, Enforcement>,
}

impl Statuses {
    /// `active`'s rules, all pending.
    fn pending(active: Option<&Profile>) -> Self {
        Self {
            profile_id: active.map(|p| p.id.clone()),
            rules: active
                .map(|p| {
                    p.rules
                        .iter()
                        .map(|r| (r.id.clone(), Enforcement::Pending))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

pub struct ProfilesManager {
    store: Arc<ProfileStore>,
    bus: broadcast::Sender<ProfileEvent>,
    rule_sink: Arc<dyn ProfileRuleSink>,
    storage: StorageStatus,
    /// Serializes manual choices and network observations, so an
    /// auto-switch can't land between a click and its saved choice.
    switch_lock: Mutex<()>,
    /// The last settled network auto-switching acted on; `None` until the
    /// first one since the bridge started.
    last_settled: StdMutex<Option<Option<String>>>,
    /// The newest network seen, settled or not: what a manual choice is
    /// saved with.
    latest_network: StdMutex<Option<String>>,
    statuses: StdMutex<Statuses>,
    enforce_requested: Notify,
    /// One enforcement pass at a time.
    pass_lock: Mutex<()>,
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ProfilesManager {
    pub fn new(store: Arc<ProfileStore>) -> Self {
        let (bus, _) = broadcast::channel(64);
        // A profile active before a restart: its rules start pending, so the
        // first pass records what the firewall holds.
        let statuses = match store.get_active() {
            Ok(active) => Statuses::pending(active.as_ref()),
            Err(e) => {
                error!(error = %e, "profiles: couldn't read the active profile at start");
                Statuses::default()
            }
        };
        Self {
            store,
            bus,
            rule_sink: Arc::new(NoopProfileRuleSink::new(NO_SINK_REASON)),
            storage: StorageStatus {
                unreadable: false,
                persistent: false,
                reason: None,
            },
            switch_lock: Mutex::new(()),
            last_settled: StdMutex::new(None),
            latest_network: StdMutex::new(None),
            statuses: StdMutex::new(statuses),
            enforce_requested: Notify::new(),
            pass_lock: Mutex::new(()),
        }
    }

    pub fn with_rule_sink(mut self, sink: Arc<dyn ProfileRuleSink>) -> Self {
        self.rule_sink = sink;
        self
    }

    /// Record whether [`store`](Self::store) outlives the bridge process.
    /// Sent to GUIs with every `SetProfiles` (issue #46 Part 1).
    pub fn with_storage_status(mut self, storage: StorageStatus) -> Self {
        self.storage = storage;
        self
    }

    pub fn storage_status(&self) -> &StorageStatus {
        &self.storage
    }

    /// Why profiles install nothing at all, if they don't.
    pub fn not_applied_reason(&self) -> Option<String> {
        self.rule_sink.not_applied_reason().map(str::to_string)
    }

    /// The status of `profile_id`'s rule `rule_id`; `None` unless that
    /// profile is the active one.
    pub fn rule_status(&self, profile_id: &str, rule_id: &str) -> Option<Enforcement> {
        let statuses = lock(&self.statuses);
        if statuses.profile_id.as_deref() != Some(profile_id) {
            return None;
        }
        Some(
            statuses
                .rules
                .get(rule_id)
                .cloned()
                .unwrap_or(Enforcement::Pending),
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProfileEvent> {
        self.bus.subscribe()
    }

    pub fn store(&self) -> &Arc<ProfileStore> {
        &self.store
    }

    fn changed(&self) {
        let _ = self.bus.send(ProfileEvent::ProfilesChanged);
    }

    /// Ask the enforcer for a pass; requests made before it runs coalesce.
    pub fn request_enforcement(&self) {
        self.enforce_requested.notify_one();
    }

    pub async fn create_profile(
        &self,
        id: &str,
        name: &str,
        network_matchers: Vec<String>,
    ) -> Result<(), ProfilesError> {
        self.store.upsert_profile(&Profile {
            id: id.to_string(),
            name: name.to_string(),
            network_matchers,
            rules: vec![],
            active: false,
        })?;
        self.changed();
        Ok(())
    }

    pub async fn update_profile(
        &self,
        id: &str,
        name: &str,
        network_matchers: Vec<String>,
    ) -> Result<(), ProfilesError> {
        let mut existing = self.profile(id)?;
        existing.name = name.to_string();
        existing.network_matchers = network_matchers;
        self.store.upsert_profile(&existing)?;
        self.changed();
        Ok(())
    }

    fn profile(&self, id: &str) -> Result<Profile, ProfilesError> {
        self.store
            .get_profile(id)?
            .ok_or_else(|| ProfilesError::UnknownProfile(id.to_string()))
    }

    pub async fn delete_profile(&self, id: &str) -> Result<(), ProfilesError> {
        let _switch = self.switch_lock.lock().await;
        let was_active = self.store.get_profile(id)?.is_some_and(|p| p.active);
        self.store.delete_profile(id)?;
        let chosen = self.store.manual_choice()?;
        if chosen.is_some_and(|c| c.profile_id.as_deref() == Some(id)) {
            self.store.clear_manual_choice()?;
        }
        if was_active {
            self.reset_statuses(None);
            self.request_enforcement();
            let _ = self
                .bus
                .send(ProfileEvent::ActiveProfileChanged { profile_id: None });
        }
        self.changed();
        Ok(())
    }

    /// Add or replace a rule of `profile_id`. Refused, and nothing stored,
    /// when its id isn't plain, the profile is full, or the bridge wouldn't
    /// install it (the `ProfileRule` policy, through
    /// [`materializer::materialize_rule`]).
    pub async fn add_rule(&self, profile_id: &str, rule: ProfileRule) -> Result<(), ProfilesError> {
        let mut profile = self.profile(profile_id)?;
        if !valid_rule_id(&rule.id) {
            return Err(refused("id", RULE_ID_REFUSED));
        }
        profile.rules.retain(|r| r.id != rule.id);
        if profile.rules.len() >= MAX_RULES_PER_PROFILE {
            return Err(refused("rule", TOO_MANY_RULES));
        }
        let seq = profile.rules.len();
        materialize_rule(profile_id, &rule, seq).map_err(ProfilesError::Refused)?;
        let rule_id = rule.id.clone();
        profile.rules.push(rule);
        self.store.upsert_profile(&profile)?;
        if profile.active {
            lock(&self.statuses)
                .rules
                .insert(rule_id, Enforcement::Pending);
            self.request_enforcement();
        }
        self.changed();
        Ok(())
    }

    pub async fn remove_rule(&self, profile_id: &str, rule_id: &str) -> Result<(), ProfilesError> {
        let mut profile = self.profile(profile_id)?;
        profile.rules.retain(|r| r.id != rule_id);
        self.store.upsert_profile(&profile)?;
        if profile.active {
            lock(&self.statuses).rules.remove(rule_id);
            self.request_enforcement();
        }
        self.changed();
        Ok(())
    }

    /// Manually activate `id`; the choice is saved with the newest network
    /// seen (issue #82, see module docs).
    pub async fn activate(&self, id: &str) -> Result<(), ProfilesError> {
        let _switch = self.switch_lock.lock().await;
        self.activate_inner(id)?;
        self.save_manual_choice(Some(id))
    }

    /// Deactivate whatever's active, as a manual choice of no profile.
    pub async fn deactivate(&self) -> Result<(), ProfilesError> {
        let _switch = self.switch_lock.lock().await;
        self.deactivate_inner()?;
        self.save_manual_choice(None)
    }

    fn save_manual_choice(&self, profile_id: Option<&str>) -> Result<(), ProfilesError> {
        let network = lock(&self.latest_network).clone();
        self.store.set_manual_choice(&ManualChoice {
            profile_id: profile_id.map(str::to_string),
            network,
        })?;
        Ok(())
    }

    fn activate_inner(&self, id: &str) -> Result<(), ProfilesError> {
        let profile = self.profile(id)?;
        self.store.set_active(Some(id))?;
        self.reset_statuses(Some(&profile));
        self.request_enforcement();
        info!(id = %profile.id, "profile activated");
        let _ = self.bus.send(ProfileEvent::ActiveProfileChanged {
            profile_id: Some(profile.id),
        });
        self.changed();
        Ok(())
    }

    fn deactivate_inner(&self) -> Result<(), ProfilesError> {
        if self.store.get_active()?.is_some() {
            self.store.set_active(None)?;
            self.reset_statuses(None);
            self.request_enforcement();
            let _ = self
                .bus
                .send(ProfileEvent::ActiveProfileChanged { profile_id: None });
            self.changed();
        }
        Ok(())
    }

    /// Every rule of a newly active profile starts pending, so the page
    /// never shows a status from an earlier activation.
    fn reset_statuses(&self, active: Option<&Profile>) {
        *lock(&self.statuses) = Statuses::pending(active);
    }

    /// One enforcement pass: install the active profile's rules that pass
    /// the policy, delete the bridge's others, and record each rule's
    /// status. Announces a change of status.
    pub async fn enforce(&self) {
        let _pass = self.pass_lock.lock().await;
        let active = match self.store.get_active() {
            Ok(active) => active,
            Err(e) => {
                error!(error = %e, "profiles: couldn't read the active profile; nothing applied");
                return;
            }
        };
        let materialized = active
            .as_ref()
            .map(|p| materialize_profile(&p.id, &p.rules))
            .unwrap_or_default();
        let wanted: Vec<_> = materialized
            .iter()
            .filter_map(|(_, rule)| rule.as_ref().ok().cloned())
            .collect();
        let mut outcomes = self.rule_sink.apply(&wanted).await.into_iter();
        let rules = materialized
            .into_iter()
            .map(|(id, rule)| {
                let status = match rule {
                    Ok(_) => outcomes.next().unwrap_or(Enforcement::Pending),
                    Err(problems) => Enforcement::NotEnforced {
                        reason: refusal_text(&problems),
                    },
                };
                (id, status)
            })
            .collect();
        self.record_statuses(active.map(|p| p.id), rules);
    }

    /// Store a pass's statuses, keeping an unchanged "Rule installed" time.
    fn record_statuses(&self, profile_id: Option<String>, rules: HashMap<String, Enforcement>) {
        let mut statuses = lock(&self.statuses);
        if statuses.profile_id != profile_id {
            // Another profile became active during the pass; its own pass
            // follows.
            return;
        }
        let mut changed = false;
        for (id, status) in rules {
            let kept = matches!(
                (statuses.rules.get(&id), &status),
                (
                    Some(Enforcement::RuleInstalled { .. }),
                    Enforcement::RuleInstalled { .. }
                )
            );
            if !kept && statuses.rules.get(&id) != Some(&status) {
                statuses.rules.insert(id, status);
                changed = true;
            }
        }
        drop(statuses);
        if changed {
            self.changed();
        }
    }
}

impl ProfilesManager {
    /// The newest network seen, settled or not (what a manual choice is
    /// saved with).
    pub fn note_network(&self, network: Option<String>) {
        *lock(&self.latest_network) = network;
    }

    /// Act on a network that stayed the same for the settle time (the
    /// auto-switch task calls this; tests call it directly). Nothing happens
    /// for the network last acted on, nor while a manual choice made on
    /// this network holds (issue #82, also right after a restart). Another
    /// network clears the choice, and the first profile whose matchers
    /// match it is activated; none matching leaves the active one.
    pub async fn on_network_observed(&self, network: Option<String>) -> Result<(), ProfilesError> {
        let _switch = self.switch_lock.lock().await;
        {
            let mut last = lock(&self.last_settled);
            if last.as_ref() == Some(&network) {
                return Ok(());
            }
            *last = Some(network.clone());
        }
        if let Some(choice) = self.store.manual_choice()? {
            if choice.network == network {
                return Ok(());
            }
            self.store.clear_manual_choice()?;
        }
        let profiles = self.store.list_profiles()?;
        let candidates: Vec<(&str, &[String])> = profiles
            .iter()
            .map(|p| (p.id.as_str(), p.network_matchers.as_slice()))
            .collect();
        let Some(matched) = matcher::find_matching_profile(candidates, network.as_deref()) else {
            return Ok(());
        };
        let already = self.store.get_active()?.is_some_and(|p| p.id == matched);
        if already {
            return Ok(());
        }
        self.activate_inner(&matched)
    }
}

fn refused(path: &str, reason: &str) -> ProfilesError {
    ProfilesError::Refused(vec![RuleProblem {
        path: path.into(),
        reason: reason.into(),
    }])
}

/// The policy's reasons as one plain sentence list.
fn refusal_text(problems: &[RuleProblem]) -> String {
    let reasons: Vec<&str> = problems.iter().map(|p| p.reason.as_str()).collect();
    format!("Not installed: {}", reasons.join("; "))
}

#[cfg(test)]
pub mod test_helpers;

#[cfg(test)]
mod tests;
