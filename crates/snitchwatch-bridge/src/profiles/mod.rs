//! Bridge-owned switchable firewall profiles ("At Home" / "Public Wi-Fi" /
//! "Office"), with automatic activation based on the connected network.
//!
//! ## Auto vs. manual activation semantics
//!
//! - [`ProfilesManager::activate`] (driven by `ClientMessage::ActivateProfile`,
//!   i.e. a user clicking "Activate" in the UI) is always **manual**: it sets
//!   an internal "pinned" flag and materializes the chosen profile
//!   immediately, regardless of what the network matcher would pick.
//! - The auto-switch loop (spawned by [`ProfilesManager::spawn_auto_switch`])
//!   only acts when the *network identity itself changes* (a new
//!   [`crate::profiles::network_watcher::NetworkWatcher`] observation
//!   different from the last one it saw). On every such change it clears the
//!   pin and re-evaluates matchers against the new network from scratch.
//! - In other words: **a manual pick sticks until you actually change
//!   networks**, at which point auto-switching resumes and evaluates the
//!   *new* network's matchers fresh — a manual choice never permanently
//!   disables auto-switching, and it is never silently overridden while you
//!   stay on the same network.
//! - If no profile's matchers match the new network, the loop leaves
//!   whatever's currently active as-is (it does not deactivate to "none").

pub mod matcher;
pub mod materializer;
pub mod network_watcher;
pub mod store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::profiles::materializer::{materialize_profile, MaterializedRule};
use crate::profiles::network_watcher::NetworkWatcher;
use crate::profiles::store::{Profile, ProfileRule, ProfileStore, StoreError};

/// Events emitted whenever profile state changes. The translator subscribes
/// and rebroadcasts as `SetProfiles` / `ProfileChanged` over the WS.
#[derive(Debug, Clone)]
pub enum ProfileEvent {
    ProfilesChanged,
    ActiveProfileChanged { profile_id: Option<String> },
}

/// Sink that receives materialized rules whenever a profile is activated
/// (`rules` non-empty) or deactivated (`rules` empty, to clear its band).
/// Mirrors [`crate::blocklists::RuleSink`]; kept as a separate trait since
/// profile activation additionally needs to *clear* the previously active
/// profile's band, and giving it its own name keeps that call site
/// unambiguous about which band is being replaced.
#[async_trait]
pub trait ProfileRuleSink: Send + Sync + 'static {
    async fn replace_profile_rules(
        &self,
        profile_id: &str,
        rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()>;
}

/// No-op sink used when no real sink has been wired in.
pub struct NoopProfileRuleSink;

#[async_trait]
impl ProfileRuleSink for NoopProfileRuleSink {
    async fn replace_profile_rules(
        &self,
        _profile_id: &str,
        _rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfilesError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
}

pub struct ProfilesManager {
    store: Arc<ProfileStore>,
    bus: broadcast::Sender<ProfileEvent>,
    rule_sink: Arc<dyn ProfileRuleSink>,
    /// `true` once a manual `activate()` call has pinned the active profile
    /// against auto-switching, until the network identity next changes. See
    /// module docs.
    manual_pin: AtomicBool,
    /// Last network identity the auto-switch loop observed, so it can detect
    /// "the network actually changed" vs. a repeated poll of the same value.
    last_seen_network: Mutex<Option<String>>,
}

impl ProfilesManager {
    pub fn new(store: Arc<ProfileStore>) -> Self {
        let (bus, _) = broadcast::channel(64);
        Self {
            store,
            bus,
            rule_sink: Arc::new(NoopProfileRuleSink),
            manual_pin: AtomicBool::new(false),
            last_seen_network: Mutex::new(None),
        }
    }

    pub fn with_rule_sink(mut self, sink: Arc<dyn ProfileRuleSink>) -> Self {
        self.rule_sink = sink;
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProfileEvent> {
        self.bus.subscribe()
    }

    pub fn store(&self) -> &Arc<ProfileStore> {
        &self.store
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
        let _ = self.bus.send(ProfileEvent::ProfilesChanged);
        Ok(())
    }

    pub async fn update_profile(
        &self,
        id: &str,
        name: &str,
        network_matchers: Vec<String>,
    ) -> Result<(), ProfilesError> {
        let mut existing = self
            .store
            .get_profile(id)?
            .ok_or_else(|| ProfilesError::UnknownProfile(id.to_string()))?;
        existing.name = name.to_string();
        existing.network_matchers = network_matchers;
        self.store.upsert_profile(&existing)?;
        let _ = self.bus.send(ProfileEvent::ProfilesChanged);
        Ok(())
    }

    pub async fn delete_profile(&self, id: &str) -> Result<(), ProfilesError> {
        let was_active = self
            .store
            .get_profile(id)?
            .map(|p| p.active)
            .unwrap_or(false);
        self.store.delete_profile(id)?;
        if was_active {
            if let Err(e) = self.rule_sink.replace_profile_rules(id, vec![]).await {
                warn!(%id, error = %e, "profiles: failed to clear rules for deleted active profile");
            }
            let _ = self
                .bus
                .send(ProfileEvent::ActiveProfileChanged { profile_id: None });
        }
        let _ = self.bus.send(ProfileEvent::ProfilesChanged);
        Ok(())
    }

    pub async fn add_rule(&self, profile_id: &str, rule: ProfileRule) -> Result<(), ProfilesError> {
        let mut profile = self
            .store
            .get_profile(profile_id)?
            .ok_or_else(|| ProfilesError::UnknownProfile(profile_id.to_string()))?;
        profile.rules.retain(|r| r.id != rule.id);
        profile.rules.push(rule);
        let is_active = profile.active;
        self.store.upsert_profile(&profile)?;
        if is_active {
            self.rematerialize(&profile).await;
        }
        let _ = self.bus.send(ProfileEvent::ProfilesChanged);
        Ok(())
    }

    pub async fn remove_rule(&self, profile_id: &str, rule_id: &str) -> Result<(), ProfilesError> {
        let mut profile = self
            .store
            .get_profile(profile_id)?
            .ok_or_else(|| ProfilesError::UnknownProfile(profile_id.to_string()))?;
        profile.rules.retain(|r| r.id != rule_id);
        let is_active = profile.active;
        self.store.upsert_profile(&profile)?;
        if is_active {
            self.rematerialize(&profile).await;
        }
        let _ = self.bus.send(ProfileEvent::ProfilesChanged);
        Ok(())
    }

    /// Manually activate `id`. Pins auto-switching off until the network
    /// identity next changes (see module docs).
    pub async fn activate(&self, id: &str) -> Result<(), ProfilesError> {
        self.manual_pin.store(true, Ordering::SeqCst);
        self.activate_inner(id).await
    }

    /// Deactivate whatever's active (clears its band, no profile becomes
    /// active). Also counts as a manual action for pinning purposes.
    pub async fn deactivate(&self) -> Result<(), ProfilesError> {
        self.manual_pin.store(true, Ordering::SeqCst);
        self.deactivate_inner().await
    }

    async fn activate_inner(&self, id: &str) -> Result<(), ProfilesError> {
        let profile = self
            .store
            .get_profile(id)?
            .ok_or_else(|| ProfilesError::UnknownProfile(id.to_string()))?;
        let previous = self.store.get_active()?;
        self.store.set_active(Some(id))?;

        if let Some(prev) = previous {
            if prev.id != id {
                if let Err(e) = self.rule_sink.replace_profile_rules(&prev.id, vec![]).await {
                    warn!(id = %prev.id, error = %e, "profiles: failed to clear previously active profile's rules");
                }
            }
        }

        let materialized = materialize_profile(&profile.id, &profile.rules);
        if let Err(e) = self
            .rule_sink
            .replace_profile_rules(&profile.id, materialized)
            .await
        {
            warn!(id = %profile.id, error = %e, "profiles: rule sink push failed; profile marked active but not enforced");
        }

        info!(id = %profile.id, "profile activated");
        let _ = self.bus.send(ProfileEvent::ActiveProfileChanged {
            profile_id: Some(profile.id),
        });
        Ok(())
    }

    async fn deactivate_inner(&self) -> Result<(), ProfilesError> {
        if let Some(prev) = self.store.get_active()? {
            self.store.set_active(None)?;
            if let Err(e) = self.rule_sink.replace_profile_rules(&prev.id, vec![]).await {
                warn!(id = %prev.id, error = %e, "profiles: failed to clear deactivated profile's rules");
            }
            let _ = self
                .bus
                .send(ProfileEvent::ActiveProfileChanged { profile_id: None });
        }
        Ok(())
    }

    async fn rematerialize(&self, profile: &Profile) {
        let materialized = materialize_profile(&profile.id, &profile.rules);
        if let Err(e) = self
            .rule_sink
            .replace_profile_rules(&profile.id, materialized)
            .await
        {
            warn!(id = %profile.id, error = %e, "profiles: rule sink push failed while rematerializing active profile");
        }
    }

    /// One tick of auto-switch evaluation: called by the watcher loop with
    /// the newly observed network identity. Only actually re-activates
    /// anything when `network` differs from the last-seen value (clearing
    /// the manual pin in that case) — see module docs for the full
    /// semantics. Exposed standalone (rather than only inside
    /// `spawn_auto_switch`) so it's directly unit-testable without a real
    /// `NetworkWatcher`.
    pub async fn on_network_observed(&self, network: Option<String>) -> Result<(), ProfilesError> {
        let mut last_seen = self.last_seen_network.lock().await;
        if *last_seen == network {
            return Ok(());
        }
        *last_seen = network.clone();
        drop(last_seen);

        // A genuinely new network resets the pin: auto-switching gets a
        // fresh chance to evaluate this network's matchers.
        self.manual_pin.store(false, Ordering::SeqCst);

        let profiles = self.store.list_profiles()?;
        let candidates: Vec<(&str, &[String])> = profiles
            .iter()
            .map(|p| (p.id.as_str(), p.network_matchers.as_slice()))
            .collect();
        let Some(matched_id) = matcher::find_matching_profile(candidates, network.as_deref())
        else {
            // No profile matches this network — leave the current active
            // profile (if any) untouched rather than deactivating.
            return Ok(());
        };

        let already_active = self
            .store
            .get_active()?
            .map(|p| p.id == matched_id)
            .unwrap_or(false);
        if already_active {
            return Ok(());
        }
        self.activate_inner(&matched_id).await
    }

    /// Spawn the background loop that drives [`Self::on_network_observed`]
    /// from a live [`NetworkWatcher`]. Returns immediately; the loop runs
    /// until every `NetworkWatcher` subscription is dropped or the manager
    /// itself is dropped.
    pub fn spawn_auto_switch(self: Arc<Self>, watcher: Arc<dyn NetworkWatcher>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut rx = watcher.subscribe();
            // Evaluate the watcher's current value once up front, in case it
            // already had a value before we subscribed (e.g. NetworkManager
            // was already connected to Wi-Fi when the bridge started).
            let initial = watcher.current_connection_id().await;
            if let Err(e) = self.on_network_observed(initial).await {
                warn!(error = %e, "profiles: initial auto-switch evaluation failed");
            }
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let network = rx.borrow().clone();
                if let Err(e) = self.on_network_observed(network).await {
                    warn!(error = %e, "profiles: auto-switch evaluation failed");
                }
            }
        })
    }
}

#[cfg(test)]
pub mod test_helpers {
    use super::*;
    use tokio::sync::watch;

    /// Test double implementing [`NetworkWatcher`] with a controllable
    /// `watch::Sender`, so `ProfilesManager` behavior can be exercised
    /// without any real D-Bus/NetworkManager dependency.
    pub struct FakeNetworkWatcher {
        tx: watch::Sender<Option<String>>,
    }

    impl FakeNetworkWatcher {
        pub fn new(initial: Option<&str>) -> (Arc<Self>, watch::Sender<Option<String>>) {
            let (tx, _rx) = watch::channel(initial.map(str::to_string));
            let watcher = Arc::new(Self { tx: tx.clone() });
            (watcher, tx)
        }
    }

    #[async_trait]
    impl NetworkWatcher for FakeNetworkWatcher {
        async fn current_connection_id(&self) -> Option<String> {
            self.tx.borrow().clone()
        }

        fn subscribe(&self) -> watch::Receiver<Option<String>> {
            self.tx.subscribe()
        }
    }

    #[derive(Default)]
    pub struct CapturingRuleSink {
        pub calls: std::sync::Mutex<Vec<(String, Vec<MaterializedRule>)>>,
    }

    #[async_trait]
    impl ProfileRuleSink for CapturingRuleSink {
        async fn replace_profile_rules(
            &self,
            profile_id: &str,
            rules: Vec<MaterializedRule>,
        ) -> anyhow::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push((profile_id.to_string(), rules));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{CapturingRuleSink, FakeNetworkWatcher};
    use super::*;

    fn manager() -> ProfilesManager {
        let store = Arc::new(ProfileStore::open_in_memory().unwrap());
        ProfilesManager::new(store)
    }

    #[tokio::test]
    async fn create_profile_emits_profiles_changed_event() {
        let mgr = manager();
        let mut rx = mgr.subscribe();
        mgr.create_profile("home", "At Home", vec!["Home*".into()])
            .await
            .unwrap();
        let evt = rx.recv().await.unwrap();
        assert!(matches!(evt, ProfileEvent::ProfilesChanged));
        assert_eq!(mgr.store().list_profiles().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn activate_materializes_rules_and_emits_active_changed() {
        let store = Arc::new(ProfileStore::open_in_memory().unwrap());
        let sink = Arc::new(CapturingRuleSink::default());
        let mgr = ProfilesManager::new(store).with_rule_sink(sink.clone());
        mgr.create_profile("home", "At Home", vec![]).await.unwrap();
        mgr.add_rule(
            "home",
            ProfileRule {
                id: "r1".into(),
                action: "allow".into(),
                operand: "dest.host".into(),
                data: "nas.local".into(),
            },
        )
        .await
        .unwrap();

        let mut rx = mgr.subscribe();
        mgr.activate("home").await.unwrap();
        let evt = rx.recv().await.unwrap();
        assert!(matches!(
            evt,
            ProfileEvent::ActiveProfileChanged {
                profile_id: Some(ref id)
            } if id == "home"
        ));

        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "home");
        assert_eq!(calls[0].1.len(), 1);
        assert!(mgr.store().get_active().unwrap().unwrap().active);
    }

    #[tokio::test]
    async fn activating_a_new_profile_clears_the_previous_ones_band() {
        let store = Arc::new(ProfileStore::open_in_memory().unwrap());
        let sink = Arc::new(CapturingRuleSink::default());
        let mgr = ProfilesManager::new(store).with_rule_sink(sink.clone());
        mgr.create_profile("home", "Home", vec![]).await.unwrap();
        mgr.create_profile("office", "Office", vec![])
            .await
            .unwrap();

        mgr.activate("home").await.unwrap();
        mgr.activate("office").await.unwrap();

        let calls = sink.calls.lock().unwrap();
        // home activated (empty rules), office activated (empty rules), and
        // home cleared (empty rules) when office took over — 3 calls total,
        // the middle assertions below check the *targets* seen.
        let targets: Vec<&str> = calls.iter().map(|(id, _)| id.as_str()).collect();
        assert!(targets.contains(&"home"));
        assert!(targets.contains(&"office"));
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "office");
        assert!(!mgr.store().get_profile("home").unwrap().unwrap().active);
    }

    #[tokio::test]
    async fn deactivate_clears_active_profile() {
        let mgr = manager();
        mgr.create_profile("home", "Home", vec![]).await.unwrap();
        mgr.activate("home").await.unwrap();
        mgr.deactivate().await.unwrap();
        assert!(mgr.store().get_active().unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_active_profile_clears_its_band_and_deactivates() {
        let store = Arc::new(ProfileStore::open_in_memory().unwrap());
        let sink = Arc::new(CapturingRuleSink::default());
        let mgr = ProfilesManager::new(store).with_rule_sink(sink.clone());
        mgr.create_profile("home", "Home", vec![]).await.unwrap();
        mgr.activate("home").await.unwrap();
        mgr.delete_profile("home").await.unwrap();
        assert!(mgr.store().get_profile("home").unwrap().is_none());
        let calls = sink.calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|(id, rules)| id == "home" && rules.is_empty()));
    }

    #[tokio::test]
    async fn auto_switch_activates_matching_profile_on_network_change() {
        let mgr = manager();
        mgr.create_profile("home", "Home", vec!["Home*".into()])
            .await
            .unwrap();
        mgr.create_profile("office", "Office", vec!["Office*".into()])
            .await
            .unwrap();

        mgr.on_network_observed(Some("Home-5G".into()))
            .await
            .unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "home");

        mgr.on_network_observed(Some("Office-Guest".into()))
            .await
            .unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "office");
    }

    #[tokio::test]
    async fn manual_activation_is_not_overridden_until_network_changes() {
        let mgr = manager();
        mgr.create_profile("home", "Home", vec!["Home*".into()])
            .await
            .unwrap();
        mgr.create_profile("office", "Office", vec!["Office*".into()])
            .await
            .unwrap();

        // Auto-activate "home" for the current network.
        mgr.on_network_observed(Some("Home-5G".into()))
            .await
            .unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "home");

        // User manually overrides to "office" while still on the Home network.
        mgr.activate("office").await.unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "office");

        // A repeated observation of the SAME network must not re-run
        // auto-switch logic and stomp the manual pick.
        mgr.on_network_observed(Some("Home-5G".into()))
            .await
            .unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "office");

        // Only once the network actually changes does auto-switch resume.
        mgr.on_network_observed(Some("Office-Guest".into()))
            .await
            .unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "office");
    }

    #[tokio::test]
    async fn network_with_no_matching_profile_leaves_active_profile_untouched() {
        let mgr = manager();
        mgr.create_profile("home", "Home", vec!["Home*".into()])
            .await
            .unwrap();
        mgr.on_network_observed(Some("Home-5G".into()))
            .await
            .unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "home");

        mgr.on_network_observed(Some("Coffee-Shop-WiFi".into()))
            .await
            .unwrap();
        // No profile matches "Coffee-Shop-WiFi" — "home" should remain active.
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "home");
    }

    #[tokio::test]
    async fn spawn_auto_switch_reacts_to_fake_watcher() {
        let mgr = Arc::new(manager());
        mgr.create_profile("home", "Home", vec!["Home*".into()])
            .await
            .unwrap();
        let (watcher, tx) = FakeNetworkWatcher::new(None);

        let handle = mgr.clone().spawn_auto_switch(watcher);
        // `spawn_auto_switch`'s task subscribes asynchronously; give it a
        // chance to do so before publishing, retrying the send rather than
        // asserting a fixed delay (a `watch::Sender::send` errors only when
        // zero receivers currently exist, which is true for a brief moment
        // right after `spawn` returns).
        let mut sent = false;
        for _ in 0..50 {
            if tx.send(Some("Home-Wifi".into())).is_ok() {
                sent = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(sent, "auto-switch task never subscribed to the watcher");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if mgr.store().get_active().unwrap().is_some() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                handle.abort();
                panic!("auto-switch never activated matching profile");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "home");
        handle.abort();
    }
}
