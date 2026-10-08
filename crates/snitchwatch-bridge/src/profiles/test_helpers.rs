//! Test doubles for the profile manager: a controllable network watcher and
//! a sink that records what it was asked to install.

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::Utc;
use snitchwatch_proto::protocol::Rule;
use tokio::sync::watch;

use super::ProfileRuleSink;
use crate::blocklists::Enforcement;
use crate::profiles::network_watcher::NetworkWatcher;

/// [`NetworkWatcher`] with a controllable `watch::Sender`, so the manager
/// runs without D-Bus or NetworkManager.
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

/// Records each pass's wanted rule names and reports them installed.
#[derive(Default)]
pub struct CapturingRuleSink {
    pub calls: StdMutex<Vec<Vec<String>>>,
}

#[async_trait]
impl ProfileRuleSink for CapturingRuleSink {
    async fn apply(&self, wanted: &[Rule]) -> Vec<Enforcement> {
        let names = wanted.iter().map(|r| r.name.clone()).collect();
        self.calls.lock().unwrap().push(names);
        vec![Enforcement::RuleInstalled { at: Utc::now() }; wanted.len()]
    }
}

impl CapturingRuleSink {
    /// The wanted names of the last pass.
    pub fn last(&self) -> Option<Vec<String>> {
        self.calls.lock().unwrap().last().cloned()
    }
}
