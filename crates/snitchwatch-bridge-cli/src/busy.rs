//! Rule names a command is working on (P2.1): a rename's two names, an
//! add's name, an import's rule in flight. While a name is busy, no other
//! command or import may change it, so two changes to one name can't race.

use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

#[derive(Clone, Default)]
pub(crate) struct BusyNames(Arc<StdMutex<HashSet<String>>>);

/// Holds its names busy until dropped.
pub(crate) struct BusyGuard {
    busy: BusyNames,
    names: Vec<String>,
}

impl BusyNames {
    fn lock(&self) -> MutexGuard<'_, HashSet<String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.lock().contains(name)
    }

    /// Mark every name busy, or none of them if any already is.
    pub(crate) fn claim(&self, names: &[&str]) -> Option<BusyGuard> {
        let mut busy = self.lock();
        if names.iter().any(|name| busy.contains(*name)) {
            return None;
        }
        busy.extend(names.iter().map(|name| name.to_string()));
        Some(BusyGuard {
            busy: self.clone(),
            names: names.iter().map(|name| name.to_string()).collect(),
        })
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let mut busy = self.busy.lock();
        for name in &self.names {
            busy.remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_is_all_or_nothing_and_ends_with_its_guard() {
        let busy = BusyNames::default();
        let first = busy.claim(&["a", "b"]).expect("free");
        assert!(busy.claim(&["b", "c"]).is_none());
        assert!(!busy.contains("c"), "a failed claim takes nothing");
        drop(first);
        assert!(busy.claim(&["b", "c"]).is_some());
    }
}
