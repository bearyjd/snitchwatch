//! Account names for display (issue #102): opensnitchd reports a
//! `user.name` condition with the uid it resolved when it loaded the rule
//! (#91), so the Rules page shows `user.name = 958`. This looks the uid up
//! locally (`getpwuid_r`) to show the account's name next to it, for
//! display only: the rule itself, and what is sent, never change. Inside a
//! sandbox that doesn't list the account, the number is shown as before.

/// Longest buffer `getpwuid_r` may ask for.
const MAX_BUFFER: usize = 1 << 16;

/// The account name for `uid`, if this system knows it.
pub fn account_name(uid: u32) -> Option<String> {
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
        if rc == libc::ERANGE && buffer.len() < MAX_BUFFER {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || entry.pw_name.is_null() {
            return None;
        }
        // SAFETY: on success `pw_name` points at a NUL-terminated string
        // inside `buffer`, which is still alive.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
        return name
            .to_str()
            .ok()
            .filter(|n| !n.is_empty() && !n.chars().any(char::is_control))
            .map(str::to_string);
    }
}

/// A `user.name` condition's value as shown: `snitchwatch (958)` for a uid
/// this system knows, otherwise the value unchanged.
pub fn display_user_name(data: &str) -> String {
    let uid = data
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| data.parse::<u32>().ok())
        .flatten();
    match uid.and_then(account_name) {
        Some(name) => format!("{name} ({data})"),
        None => data.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_uid_shows_its_account_name() {
        assert_eq!(account_name(0).as_deref(), Some("root"));
        assert_eq!(display_user_name("0"), "root (0)");
    }

    #[test]
    fn anything_else_is_shown_as_it_is() {
        assert_eq!(account_name(u32::MAX - 7), None);
        assert_eq!(display_user_name("4294967288"), "4294967288");
        assert_eq!(display_user_name("alice"), "alice");
        assert_eq!(display_user_name("0x0"), "0x0");
        assert_eq!(display_user_name(""), "");
    }

    /// The Rules page's summary of a daemon-reported `user.name` row.
    #[test]
    fn the_rules_summary_names_the_account() {
        let rule = crate::rules::row_store::Rule {
            operator: serde_json::json!({ "type": "simple", "operand": "user.name", "data": "0" }),
            ..Default::default()
        };
        assert_eq!(rule.operator_summary(), "user.name = root (0)");
    }
}
