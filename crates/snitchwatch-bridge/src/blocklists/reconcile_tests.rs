//! [`BlocklistsManager::reconcile`] and unsubscribe against a scripted sink
//! (issue #45 PR B).

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::Utc;

use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;

#[derive(Default)]
struct ScriptedSink {
    unknown: bool,
    current: Vec<&'static str>,
    unavailable: Option<&'static str>,
    pushes: StdMutex<Vec<(String, usize)>>,
    removed: StdMutex<Vec<String>>,
    orphan_passes: StdMutex<Vec<Vec<String>>>,
}

#[async_trait]
impl RuleSink for ScriptedSink {
    fn daemon_rules_known(&self) -> bool {
        !self.unknown
    }

    fn is_current(&self, list_id: &str) -> bool {
        self.current.contains(&list_id)
    }

    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        self.pushes
            .lock()
            .unwrap()
            .push((list_id.to_string(), hosts.len()));
        match self.unavailable {
            Some(reason) => Err(NotInstalled::daemon_unavailable(reason)),
            None => Ok(()),
        }
    }

    async fn remove_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.removed.lock().unwrap().push(list_id.to_string());
        Ok(())
    }

    async fn remove_orphans(&self, keep: &[String]) {
        self.orphan_passes.lock().unwrap().push(keep.to_vec());
    }
}

/// `downloaded` lists with two hosts each, plus one never downloaded.
fn manager(sink: Arc<ScriptedSink>, downloaded: &[&str]) -> BlocklistsManager {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for (id, fetched) in downloaded
        .iter()
        .map(|id| (*id, true))
        .chain([("never", false)])
    {
        store
            .upsert_subscription(&Subscription {
                id: id.into(),
                url: format!("https://example.invalid/{id}"),
                display_name: id.into(),
                format_hint: None,
                refresh_interval_secs: 86_400,
                last_fetched_at: fetched.then(Utc::now),
                last_attempt_at: None,
                last_fetch_status: if fetched {
                    FetchStatus::Ok
                } else {
                    FetchStatus::Pending
                },
                entry_count: if fetched { 2 } else { 0 },
            })
            .unwrap();
        if fetched {
            store
                .replace_entries(id, &["a.example", "b.example"])
                .unwrap();
        }
    }
    BlocklistsManager::new(store).with_rule_sink(sink)
}

fn pushed(sink: &ScriptedSink) -> Vec<(String, usize)> {
    sink.pushes.lock().unwrap().clone()
}

#[tokio::test]
async fn nothing_happens_while_the_daemons_rule_list_is_unknown() {
    let sink = Arc::new(ScriptedSink {
        unknown: true,
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.reconcile().await;
    assert!(pushed(&sink).is_empty());
    assert!(sink.orphan_passes.lock().unwrap().is_empty());
    assert_eq!(mgr.enforcement("ads"), Enforcement::Pending);
}

#[tokio::test]
async fn every_downloaded_list_not_installed_is_pushed_from_the_store() {
    let sink = Arc::new(ScriptedSink::default());
    let mgr = manager(sink.clone(), &["ads", "trackers"]);
    let mut events = mgr.subscribe();
    mgr.reconcile().await;
    assert_eq!(
        pushed(&sink),
        vec![("ads".to_string(), 2), ("trackers".to_string(), 2)],
        "the never-downloaded list is left to the refresh loop"
    );
    for id in ["ads", "trackers"] {
        assert!(matches!(
            mgr.enforcement(id),
            Enforcement::RuleInstalled { .. }
        ));
    }
    assert!(matches!(
        events.try_recv(),
        Ok(BlocklistEvent::StatusChanged { .. })
    ));
    assert_eq!(
        *sink.orphan_passes.lock().unwrap(),
        vec![vec![
            "ads".to_string(),
            "never".to_string(),
            "trackers".to_string()
        ]]
    );

    // A second pass finds both current and installed: nothing to resend.
    let current = Arc::new(ScriptedSink {
        current: vec!["ads", "trackers"],
        ..Default::default()
    });
    let mgr = mgr.with_rule_sink(current.clone());
    mgr.reconcile().await;
    assert!(pushed(&current).is_empty());
}

#[tokio::test]
async fn a_list_current_on_the_daemon_is_still_pushed_until_confirmed_in_this_run() {
    let sink = Arc::new(ScriptedSink {
        current: vec!["ads"],
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.reconcile().await;
    assert_eq!(pushed(&sink), vec![("ads".to_string(), 2)]);
}

#[tokio::test]
async fn an_unavailable_daemon_stops_the_pass_without_deleting_anything() {
    let sink = Arc::new(ScriptedSink {
        unavailable: Some("The firewall service didn't answer"),
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads", "trackers"]);
    mgr.reconcile().await;
    assert_eq!(pushed(&sink).len(), 1);
    assert!(sink.orphan_passes.lock().unwrap().is_empty());
    assert_eq!(
        mgr.enforcement("ads"),
        Enforcement::NotEnforced {
            reason: "The firewall service didn't answer".into()
        }
    );
}

#[tokio::test]
async fn a_sink_that_installs_nothing_is_never_reconciled_or_asked_to_remove() {
    let mgr = manager(Arc::new(ScriptedSink::default()), &["ads"]).with_rule_sink(Arc::new(
        NoopRuleSink::new("no state directory: in-process"),
    ));
    mgr.reconcile().await;
    assert_eq!(
        mgr.enforcement("ads"),
        Enforcement::NotEnforced {
            reason: "no state directory: in-process".into()
        }
    );
    mgr.remove_subscription("ads").await.unwrap();
}

#[tokio::test]
async fn unsubscribing_removes_the_lists_rules_through_the_sink() {
    let sink = Arc::new(ScriptedSink::default());
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.remove_subscription("ads").await.unwrap();
    assert_eq!(*sink.removed.lock().unwrap(), vec!["ads".to_string()]);
    assert!(mgr.subscription("ads").is_none());
}
