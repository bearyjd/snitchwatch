//! Issue #73: unsubscribing hands the list to the sink's `release`, which may
//! keep the list's files for a while, not to `remove`, which never does.

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;

use super::store::BlocklistStore;
use super::*;

#[derive(Default)]
struct Log(StdMutex<Vec<String>>);

#[async_trait]
impl RuleSink for Log {
    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        Ok(())
    }

    async fn remove_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.0.lock().unwrap().push(format!("remove:{list_id}"));
        Ok(())
    }

    async fn release_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.0.lock().unwrap().push(format!("release:{list_id}"));
        Ok(())
    }
}

#[tokio::test]
async fn unsubscribing_releases_the_list_rather_than_removing_it() {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let log = Arc::new(Log::default());
    let mgr = BlocklistsManager::new(store).with_rule_sink(log.clone());
    let id = mgr
        .add_subscription("https://example.invalid/ads.txt")
        .await
        .unwrap();
    mgr.remove_subscription(&id).await.unwrap();
    assert_eq!(*log.0.lock().unwrap(), vec![format!("release:{id}")]);
}
