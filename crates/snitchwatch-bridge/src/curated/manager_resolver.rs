//! Entries pinned to a service account (the DNS entry's `systemd-resolve`,
//! `chrony`, `avahi`; #120 item 38, items 36/41) are offered only where
//! that account is found, and installed at the uid it is found at: those
//! uids are allocated when the image is built. Looked up off the async
//! threads, one lookup at a time (#155), and not at all by a bridge that
//! changes no recommended rules. Until an account is confirmed its entry
//! is withheld: nothing is installed, replaced or re-enabled for it, its
//! choices and grant are kept, and a copy already in the firewall is left
//! as it is, so a lookup that fails for a moment never removes a rule the
//! user opted into. Turning it off still removes it, as for any entry. A
//! confirmed account at another uid than a copy names is `manager_moved.rs`.

use super::state::State;
use super::*;

/// How long a pass waits for the lookup: an NSS backend (LDAP, SSSD) may
/// hang, and a pass carries the user's own deletes too. A lookup that takes
/// longer counts as unconfirmed.
/// The highest uid a service account may have: Fedora's `SYS_UID_MAX`
/// (`login.defs`). A service account's uid is in `1..=SYS_UID_MAX`, which
/// leaves out root, `nobody` (65534) and every person's account (security
/// review M1: a directory that defines a service account at a person's uid
/// must not give that person the service's rule).
pub const SYS_UID_MAX: u32 = 999;

pub const RESOLVER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the DNS entry is unavailable on this host: its account wasn't
/// found.
pub const DNS_ACCOUNT_REASON: &str = "Snitchwatch couldn't look up the system resolver's \
     account (systemd-resolve) on this computer, so it doesn't install or update this rule. A \
     copy already in the firewall is left as it is.";

/// Why an entry pinned to a service account is unavailable: its account
/// wasn't found (#120 36/41).
pub const ACCOUNT_REASON: &str = "Snitchwatch couldn't look up the account this service runs \
     as on this computer, so it doesn't install or update this rule. A copy already in the \
     firewall is left as it is.";

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
    lookup: Option<(crate::accounts::UidLookup, crate::accounts::AccountLookup)>,
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

/// The uid `entry`'s service account was found at: only a system uid
/// ([`SYS_UID_MAX`]) whose own name is the account's.
pub(super) fn account_uid(entry: &super::super::CuratedEntry, found: &Found) -> Option<u32> {
    entry
        .account()
        .and_then(|name| found.get(name).copied().flatten())
        .filter(|uid| (1..=SYS_UID_MAX).contains(uid))
}

/// The entries withheld this pass, with why: each pinned to a service
/// account that wasn't found.
pub(super) fn withheld(found: &Found) -> BTreeMap<String, &'static str> {
    entries()
        .iter()
        .filter(|entry| entry.account().is_some() && account_uid(entry, found).is_none())
        .map(|entry| {
            let reason = if entry.any_address {
                DNS_ACCOUNT_REASON
            } else {
                ACCOUNT_REASON
            };
            (entry.id.clone(), reason)
        })
        .collect()
}

/// Hold back what would install, replace or re-enable an entry `withheld`
/// this pass, and show it unavailable, saying why (an undecided copy in the
/// firewall keeps its status: it still applies).
pub(super) fn withhold(
    state: &mut State,
    actions: Vec<CuratedAction>,
    withheld: &BTreeMap<String, &'static str>,
    found: &Found,
) -> Vec<CuratedAction> {
    for (id, reason) in withheld {
        // An undecided copy still applies, and still reads as on.
        let undecided = matches!(
            state.statuses.get(id),
            Some(EntryStatus::InFirewall | EntryStatus::InFirewallButOff)
        );
        if !undecided {
            state.statuses.insert(id.clone(), EntryStatus::Unavailable);
        }
        state.problems.insert(id.clone(), reason);
        state.reenables.remove(id);
    }
    state.withheld = withheld.keys().cloned().collect();
    state.found = found.clone();
    actions
        .into_iter()
        .filter(|action| {
            !matches!(action, CuratedAction::Install(id) | CuratedAction::Upgrade(id)
                if withheld.contains_key(id))
        })
        .collect()
}

impl CuratedDefaults {
    /// [`Self::set_account_lookups`] for a test's fake: each uid's name is
    /// the account `lookup` gives it.
    pub fn set_uid_lookup(&self, lookup: crate::accounts::UidLookup) {
        let forward = lookup.clone();
        let names: crate::accounts::AccountLookup = Arc::new(move |uid| {
            super::super::account_names()
                .into_iter()
                .find(|name| forward(name) == Some(uid))
                .map(str::to_string)
        });
        self.set_account_lookups(lookup, names);
    }

    /// Look the services' accounts up with `lookup`, and each uid found
    /// back with `names` (`getpwuid_r` in production): a uid whose own name
    /// isn't the account's is unconfirmed (security review M1). The next
    /// pass plans again.
    pub fn set_account_lookups(
        &self,
        lookup: crate::accounts::UidLookup,
        names: crate::accounts::AccountLookup,
    ) {
        {
            let mut accounts = lock_accounts(&self.inner);
            accounts.lookup = Some((lookup, names));
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
                let (lookup, names) = lookup;
                let found: Found = super::super::account_names()
                    .into_iter()
                    .map(|name| {
                        let uid = lookup(name)
                            .filter(|uid| (1..=SYS_UID_MAX).contains(uid))
                            .filter(|uid| names(*uid).as_deref() == Some(name));
                        (name, uid)
                    })
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
