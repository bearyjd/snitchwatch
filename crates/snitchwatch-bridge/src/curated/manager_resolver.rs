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

/// How long a pass waits for the lookup: an NSS backend (LDAP, SSSD) may
/// hang, and a pass carries the user's own deletes too. A lookup that takes
/// longer counts as unconfirmed.
pub const RESOLVER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

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
        let lookup =
            tokio::task::spawn_blocking(move || super::super::resolver_account_confirmed(&lookup));
        let confirmed = match tokio::time::timeout(RESOLVER_LOOKUP_TIMEOUT, lookup).await {
            Ok(confirmed) => confirmed.unwrap_or(false),
            Err(_) => {
                warn!("looking up the system resolver's account timed out");
                false
            }
        };
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
