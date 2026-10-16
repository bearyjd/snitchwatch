//! Account names for display (issue #102, PR #106 review M4). opensnitchd
//! reports a `user.name` condition with the uid it resolved when it loaded
//! the rule (#91), so a list would read `user.name = 958`. The bridge runs
//! on the host and sees the account database the daemon used; a sandboxed
//! GUI may not. So the bridge looks the uids up when a daemon snapshot
//! arrives, off the async runtime (`spawn_blocking`, one batch at a time,
//! #115), and sends each rule's names along with it (`userNames` on the wire
//! rule). Display only: the rule, and anything sent to the daemon, never
//! change.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use snitchwatch_proto::protocol::{Operator, Rule};

use crate::translator::display::strip_display_hazards;

/// Looks up the account name of a uid; [`system_lookup`] in production,
/// a fake in tests (which must never read the host's accounts).
pub type AccountLookup = Arc<dyn Fn(u32) -> Option<String> + Send + Sync>;

/// Longest account name sent, in characters.
pub const MAX_ACCOUNT_NAME_CHARS: usize = 64;
/// Uids remembered at most, found or not.
pub const MAX_KNOWN_ACCOUNTS: usize = 256;
/// Uids looked up at most for one snapshot, and queued at most while a
/// batch runs.
pub const MAX_LOOKUPS_PER_SNAPSHOT: usize = 64;
/// How long a uid with no name (no account, or a failed lookup) waits before
/// it is looked up again.
pub const MISSING_RETRY: Duration = Duration::from_secs(60);

/// Longest buffer `getpwuid_r` may ask for.
const MAX_PASSWD_BUFFER: usize = 1 << 16;

/// The uid a `user.name` value names: only canonical decimal (`958`, not
/// `0958` or `+958`).
pub fn canonical_uid(data: &str) -> Option<u32> {
    let uid: u32 = data.parse().ok()?;
    (uid.to_string() == data).then_some(uid)
}

/// Every uid `rule` names in a `user.name` condition, list members included.
pub fn user_name_uids(rule: &Rule) -> BTreeSet<u32> {
    fn walk(op: &Operator, uids: &mut BTreeSet<u32>) {
        if op.operand == "user.name" {
            uids.extend(canonical_uid(&op.data));
        }
        for member in &op.list {
            walk(member, uids);
        }
    }
    let mut uids = BTreeSet::new();
    if let Some(op) = &rule.operator {
        walk(op, &mut uids);
    }
    uids
}

/// An account name as a GUI may show it: display hazards stripped, at most
/// [`MAX_ACCOUNT_NAME_CHARS`]; `None` when nothing is left.
pub fn display_name(raw: &str) -> Option<String> {
    let plain = strip_display_hazards(raw);
    let name: String = plain.trim().chars().take(MAX_ACCOUNT_NAME_CHARS).collect();
    (!name.is_empty()).then_some(name)
}

/// The host's account database, through `getpwuid_r`.
pub fn system_lookup() -> AccountLookup {
    Arc::new(account_name)
}

fn account_name(uid: u32) -> Option<String> {
    let mut buffer = vec![0u8; 1024];
    loop {
        // SAFETY: an all-zero `passwd` is a valid out-parameter; it is only
        // read after getpwuid_r reports success.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call, and `buffer`'s length
        // is passed with it; getpwuid_r writes only within them.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buffer.len() < MAX_PASSWD_BUFFER {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || entry.pw_name.is_null() {
            return None;
        }
        // SAFETY: on success `pw_name` points at a NUL-terminated string
        // inside `buffer`, which is still alive.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
        return name.to_str().ok().map(str::to_string);
    }
}

/// Looks up the uid of an account name; [`system_uid_lookup`] in
/// production, a fake in tests (which must never read the host's accounts).
/// `None` when there is no such account or the lookup failed.
pub type UidLookup = Arc<dyn Fn(&str) -> Option<u32> + Send + Sync>;

/// The host's account database, through `getpwnam_r`. Blocking: NSS may go
/// over the network.
pub fn system_uid_lookup() -> UidLookup {
    Arc::new(account_uid)
}

fn account_uid(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut buffer = vec![0u8; 1024];
    loop {
        // SAFETY: an all-zero `passwd` is a valid out-parameter; it is only
        // read after getpwnam_r reports success.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated, every pointer is valid for the
        // call, and `buffer`'s length is passed with it; getpwnam_r writes
        // only within them.
        let rc = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buffer.len() < MAX_PASSWD_BUFFER {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        return (rc == 0 && !found.is_null()).then_some(entry.pw_uid);
    }
}

/// Look `uids` up with `lookup`, keeping only names fit to show
/// ([`display_name`]). Blocking: NSS may go over the network. No lookup
/// starts once `budget` has passed (the first always does); the uids left
/// are not in the result.
pub fn look_up_blocking(
    lookup: &AccountLookup,
    uids: Vec<u32>,
    budget: Duration,
) -> Vec<(u32, Option<String>)> {
    let started = Instant::now();
    let mut found = Vec::with_capacity(uids.len());
    for uid in uids {
        if !found.is_empty() && started.elapsed() >= budget {
            break;
        }
        found.push((uid, lookup(uid).as_deref().and_then(display_name)));
    }
    found
}

/// What a finished lookup found.
#[derive(Debug, Clone)]
enum Known {
    Named(String),
    /// No account, or the lookup failed: looked up again from this time.
    Missing {
        retry_at: Instant,
    },
}

/// The uids looked up so far and what was found (at most
/// [`MAX_KNOWN_ACCOUNTS`]), and the one lookup batch that may run at a time
/// (#115): uids wanted while it runs queue for it.
#[derive(Debug, Clone, Default)]
pub struct KnownAccounts {
    names: BTreeMap<u32, Known>,
    /// Wanted and not yet found: in the running batch or queued for it.
    claimed: BTreeSet<u32>,
    /// Of `claimed`, those the running batch hasn't taken yet.
    queued: BTreeSet<u32>,
    in_flight: bool,
    /// Bumped by every new batch, so an older batch's guard
    /// ([`Self::abandon`]) leaves a newer one alone.
    serial: u64,
}

impl KnownAccounts {
    /// Claim the uids of `uids` that aren't known (a miss counts once its
    /// cool-down is over) or claimed yet, at most [`MAX_LOOKUPS_PER_SNAPSHOT`]
    /// queued at once. With no batch running, they start one: its uids.
    /// Otherwise they wait for the running batch's [`Self::next_batch`].
    pub fn want(&mut self, uids: BTreeSet<u32>, now: Instant) -> Option<Vec<u32>> {
        for uid in uids {
            if self.queued.len() >= MAX_LOOKUPS_PER_SNAPSHOT {
                break;
            }
            let known = match self.names.get(&uid) {
                Some(Known::Named(_)) => true,
                Some(Known::Missing { retry_at }) => now < *retry_at,
                None => false,
            };
            if !known && self.claimed.insert(uid) {
                self.queued.insert(uid);
            }
        }
        if self.in_flight || self.queued.is_empty() {
            return None;
        }
        self.in_flight = true;
        self.serial += 1;
        Some(std::mem::take(&mut self.queued).into_iter().collect())
    }

    /// For the running batch, once it has learnt its uids: what queued
    /// meanwhile, or `None`, and the batch is over.
    pub fn next_batch(&mut self) -> Option<Vec<u32>> {
        if self.queued.is_empty() {
            self.in_flight = false;
            return None;
        }
        Some(std::mem::take(&mut self.queued).into_iter().collect())
    }

    /// Unclaim the uids of `uids` the batch didn't learn (it ran out of
    /// time): a later snapshot wants them again.
    pub fn release(&mut self, uids: &[u32]) {
        for uid in uids {
            self.claimed.remove(uid);
        }
    }

    /// The running batch's serial, for its guard.
    pub fn batch_serial(&self) -> u64 {
        self.serial
    }

    /// The batch `serial` ended without [`Self::next_batch`] saying so (its
    /// lookup panicked): free the slot and the claims it took, keeping the
    /// queued ones for the next batch.
    pub fn abandon(&mut self, serial: u64) {
        if self.in_flight && self.serial == serial {
            self.in_flight = false;
            let queued = &self.queued;
            self.claimed.retain(|uid| queued.contains(uid));
        }
    }

    /// Remember lookups; a uid not found is looked up again after
    /// [`MISSING_RETRY`]. When full, one entry makes room (a miss first);
    /// a batch's claims are kept apart and never forgotten.
    pub fn learn(&mut self, found: Vec<(u32, Option<String>)>, now: Instant) {
        for (uid, name) in found {
            self.claimed.remove(&uid);
            if self.names.len() >= MAX_KNOWN_ACCOUNTS && !self.names.contains_key(&uid) {
                let evict = self
                    .names
                    .iter()
                    .find(|(_, known)| matches!(known, Known::Missing { .. }))
                    .or_else(|| self.names.first_key_value())
                    .map(|(&evict, _)| evict);
                if let Some(evict) = evict {
                    self.names.remove(&evict);
                }
            }
            let known = match name {
                Some(name) => Known::Named(name),
                None => Known::Missing {
                    retry_at: now + MISSING_RETRY,
                },
            };
            self.names.insert(uid, known);
        }
    }

    /// The names a rule's `user.name` uids have, keyed by the uid as the
    /// rule writes it.
    pub fn names_for(&self, rule: &Rule) -> BTreeMap<String, String> {
        user_name_uids(rule)
            .into_iter()
            .filter_map(|uid| match self.names.get(&uid)? {
                Known::Named(name) => Some((uid.to_string(), name.clone())),
                Known::Missing { .. } => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
