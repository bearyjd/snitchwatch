//! Account names for display (issue #102, PR #106 review M4). opensnitchd
//! reports a `user.name` condition with the uid it resolved when it loaded
//! the rule (#91), so a list would read `user.name = 958`. The bridge runs
//! on the host and sees the account database the daemon used; a sandboxed
//! GUI may not. So the bridge looks the uids up when a daemon snapshot
//! arrives, off the async runtime (`spawn_blocking`), and sends each rule's
//! names along with it (`userNames` on the wire rule). Display only: the
//! rule, and anything sent to the daemon, never change.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use snitchwatch_proto::protocol::{Operator, Rule};

use crate::translator::display::strip_display_hazards;

/// Looks up the account name of a uid; [`system_lookup`] in production,
/// a fake in tests (which must never read the host's accounts).
pub type AccountLookup = Arc<dyn Fn(u32) -> Option<String> + Send + Sync>;

/// Longest account name sent, in characters.
pub const MAX_ACCOUNT_NAME_CHARS: usize = 64;
/// Uids remembered at most, found or not.
pub const MAX_KNOWN_ACCOUNTS: usize = 256;
/// Uids looked up at most for one snapshot.
pub const MAX_LOOKUPS_PER_SNAPSHOT: usize = 64;

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

/// Look `uids` up with `lookup`, keeping only names fit to show
/// ([`display_name`]). Blocking: NSS may go over the network.
pub fn look_up_blocking(lookup: &AccountLookup, uids: Vec<u32>) -> Vec<(u32, Option<String>)> {
    uids.into_iter()
        .map(|uid| (uid, lookup(uid).as_deref().and_then(display_name)))
        .collect()
}

/// The uids looked up so far and what was found, at most
/// [`MAX_KNOWN_ACCOUNTS`].
#[derive(Debug, Clone, Default)]
pub struct KnownAccounts {
    names: BTreeMap<u32, Option<String>>,
}

impl KnownAccounts {
    /// Of `uids`, those not looked up yet, at most
    /// [`MAX_LOOKUPS_PER_SNAPSHOT`].
    pub fn not_looked_up(&self, uids: BTreeSet<u32>) -> Vec<u32> {
        uids.into_iter()
            .filter(|uid| !self.names.contains_key(uid))
            .take(MAX_LOOKUPS_PER_SNAPSHOT)
            .collect()
    }

    /// Of `uids`, those not looked up yet ([`Self::not_looked_up`]), now
    /// noted as being looked up (no name yet), so a second snapshot meanwhile
    /// doesn't look them up again.
    pub fn claim(&mut self, uids: BTreeSet<u32>) -> Vec<u32> {
        let wanted = self.not_looked_up(uids);
        self.learn(wanted.iter().map(|&uid| (uid, None)).collect());
        wanted
    }

    /// Remember lookups; when full, start again rather than grow.
    pub fn learn(&mut self, found: Vec<(u32, Option<String>)>) {
        for (uid, name) in found {
            if self.names.len() >= MAX_KNOWN_ACCOUNTS && !self.names.contains_key(&uid) {
                self.names.clear();
            }
            self.names.insert(uid, name);
        }
    }

    /// The names a rule's `user.name` uids have, keyed by the uid as the
    /// rule writes it.
    pub fn names_for(&self, rule: &Rule) -> BTreeMap<String, String> {
        user_name_uids(rule)
            .into_iter()
            .filter_map(|uid| Some((uid.to_string(), self.names.get(&uid)?.clone()?)))
            .collect()
    }
}

#[cfg(test)]
mod tests;
