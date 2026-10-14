//! The DNS entry is offered only where its pinned user ID is the system
//! resolver's account (#120 item 38): a rule pinned to uid 193 would match
//! any other account an administrator gave that ID. Looked up off the
//! async threads, one lookup at a time (#155), and not at all by a bridge
//! that changes no recommended rules. Until it is confirmed the entry is
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

/// Why the DNS entry is unavailable where the system resolver's account is
/// definitely another user ID (#155): a copy already in the firewall no
/// longer names it.
pub const DNS_STALE_REASON: &str = "On this computer the system resolver's account \
     (systemd-resolve) isn't user ID 193, so Snitchwatch doesn't install this rule. A copy \
     already in the firewall is out of date: it is left as it is, and turning this off removes it.";

/// What the last lookup found, by account name: `None` when the account
/// wasn't found or the lookup failed. Empty until one returns.
pub(super) type Found = BTreeMap<&'static str, Option<u32>>;

/// The account lookup and its last result (#155): one lookup in flight at
/// a time, and a result that changed bumps `epoch`, part of the pass key,
/// so an answer that comes after its pass gave up is planned for.
#[derive(Default)]
pub(super) struct Accounts {
    /// `None` until set: every account is unknown and nothing is looked up
    /// (#157). Only the system bridge sets the host's database.
    lookup: Option<crate::accounts::UidLookup>,
    found: Found,
    pub(super) epoch: u64,
    /// The `epoch` of the result the last pass read.
    pub(super) used: u64,
    in_flight: bool,
    /// Bumped by every new lookup: an older one's answer is dropped.
    serial: u64,
}

/// Clears `in_flight` when the blocking lookup ends, even by a panic.
struct Landing {
    inner: Arc<Inner>,
    serial: u64,
}

impl Drop for Landing {
    fn drop(&mut self) {
        let mut accounts = lock_accounts(&self.inner);
        if accounts.serial == self.serial {
            accounts.in_flight = false;
        }
    }
}

pub(super) fn lock_accounts(inner: &Inner) -> MutexGuard<'_, Accounts> {
    inner.accounts.lock().unwrap_or_else(|e| e.into_inner())
}

/// The entries withheld this pass, with why: the DNS entry unless its
/// account is confirmed at the user ID it pins. A definite other ID reads
/// stale (#155); no answer reads unconfirmed.
pub(super) fn withheld(found: &Found) -> BTreeMap<String, &'static str> {
    let reason = match found.get(super::super::DNS_USER_NAME).copied().flatten() {
        Some(uid) if uid.to_string() == super::super::DNS_USER_ID => return BTreeMap::new(),
        Some(_) => DNS_STALE_REASON,
        None => DNS_ACCOUNT_REASON,
    };
    entries()
        .iter()
        .filter(|entry| entry.any_address)
        .map(|entry| (entry.id.clone(), reason))
        .collect()
}

/// Hold back what would install or re-enable an entry `withheld` this
/// pass, and show it unavailable, saying why.
pub(super) fn withhold(
    state: &mut State,
    actions: Vec<CuratedAction>,
    withheld: &BTreeMap<String, &'static str>,
) -> Vec<CuratedAction> {
    for (id, reason) in withheld {
        state.statuses.insert(id.clone(), EntryStatus::Unavailable);
        state.problems.insert(id.clone(), reason);
        state.reenables.remove(id);
    }
    state.withheld = withheld.keys().cloned().collect();
    actions
        .into_iter()
        .filter(|action| !matches!(action, CuratedAction::Install(id) if withheld.contains_key(id)))
        .collect()
}

impl CuratedDefaults {
    /// Look the services' accounts up with `lookup` from now on; the next
    /// pass plans again.
    pub fn set_uid_lookup(&self, lookup: crate::accounts::UidLookup) {
        {
            let mut accounts = lock_accounts(&self.inner);
            accounts.lookup = Some(lookup);
            accounts.found = Found::new();
            accounts.in_flight = false;
            accounts.serial += 1;
            accounts.epoch += 1;
        }
        self.inner.wake.notify_one();
    }

    /// The accounts as of this pass, off the async threads. A pass waits up
    /// to [`RESOLVER_LOOKUP_TIMEOUT`] for a lookup it starts; while one is
    /// still running (a hung NSS), it reuses the last result and starts
    /// none.
    pub(super) async fn found(&self) -> Found {
        let started = {
            let mut accounts = lock_accounts(&self.inner);
            match accounts.lookup.clone() {
                Some(lookup) if !accounts.in_flight => {
                    accounts.in_flight = true;
                    Some((lookup, accounts.serial))
                }
                _ => None,
            }
        };
        if let Some((lookup, serial)) = started {
            let inner = self.inner.clone();
            let task = tokio::task::spawn_blocking(move || {
                let landing = Landing { inner, serial };
                let found: Found = super::super::account_names()
                    .into_iter()
                    .map(|name| (name, lookup(name)))
                    .collect();
                let mut accounts = lock_accounts(&landing.inner);
                if accounts.serial == serial && accounts.found != found {
                    accounts.found = found;
                    accounts.epoch += 1;
                    drop(accounts);
                    landing.inner.wake.notify_one();
                }
            });
            if tokio::time::timeout(RESOLVER_LOOKUP_TIMEOUT, task)
                .await
                .is_err()
            {
                warn!("looking up the services' accounts timed out");
            }
        }
        let mut accounts = lock_accounts(&self.inner);
        accounts.used = accounts.epoch;
        accounts.found.clone()
    }
}
