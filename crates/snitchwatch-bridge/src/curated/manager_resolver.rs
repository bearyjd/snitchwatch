//! The DNS entry is offered only where its pinned user ID is the system
//! resolver's account (#120 item 38): a rule pinned to uid 193 would match
//! any other account an administrator gave that ID. Looked up once per
//! pass, off the async threads. Until it is confirmed the entry is
//! withheld: nothing is installed or re-enabled for it, its choices and
//! grant are kept, and a copy already in the firewall is left as it is, so
//! a lookup that fails for a moment never removes a rule the user opted
//! into. Turning it off still removes it, as for any entry.

use super::state::State;
use super::*;

/// Why the DNS entry is unavailable on this host.
pub const DNS_ACCOUNT_REASON: &str = "Snitchwatch couldn't confirm that the system resolver's \
     account (systemd-resolve) is user ID 193 on this computer, so it doesn't install this rule. \
     A copy already in the firewall is left as it is.";

/// Hold back what would install or re-enable an entry `withheld` this
/// pass, and show it unavailable, saying why.
pub(super) fn withhold(
    state: &mut State,
    actions: Vec<CuratedAction>,
    withheld: &BTreeSet<String>,
) -> Vec<CuratedAction> {
    for id in withheld {
        state.statuses.insert(id.clone(), EntryStatus::Unavailable);
        state.problems.insert(id.clone(), DNS_ACCOUNT_REASON);
        state.reenables.remove(id);
    }
    state.withheld = withheld.clone();
    actions
        .into_iter()
        .filter(|action| !matches!(action, CuratedAction::Install(id) if withheld.contains(id)))
        .collect()
}

impl CuratedDefaults {
    /// Look the system resolver's account up with `lookup` from now on.
    pub fn set_uid_lookup(&self, lookup: crate::accounts::UidLookup) {
        *self
            .inner
            .uid_lookup
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = lookup;
    }

    /// The entries withheld this pass: those that need the resolver's
    /// account, unless it is confirmed.
    pub(super) async fn withheld(&self) -> BTreeSet<String> {
        let lookup = self
            .inner
            .uid_lookup
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let confirmed =
            tokio::task::spawn_blocking(move || super::super::resolver_account_confirmed(&lookup))
                .await
                .unwrap_or(false);
        if confirmed {
            return BTreeSet::new();
        }
        entries()
            .iter()
            .filter(|entry| entry.any_address)
            .map(|entry| entry.id.clone())
            .collect()
    }
}
