//! Pure network-matcher glob evaluation, no D-Bus dependency.
//!
//! A profile's `network_matchers` are glob patterns (same LS-style subset as
//! `translator::glob` — `*`/`**`/`?`) matched against the *current*
//! NetworkManager active-connection identity: either its connection id (the
//! human-readable name set in `nmcli`/`plasma-nm`, e.g. `"Home Wi-Fi"`) or,
//! for Wi-Fi connections, the SSID. Matching against either lets a profile's
//! matcher list say `"Home*"` and catch both without the caller having to
//! know which one NetworkManager will actually expose for a given connection
//! type.

use crate::translator::glob::glob_to_regex;

/// True if any of `matchers` (glob patterns) matches `candidate`
/// case-sensitively. An empty matcher list never matches anything (a
/// profile with no matchers is manual-activation-only).
pub fn matches_any(matchers: &[String], candidate: &str) -> bool {
    matchers.iter().any(|glob| match glob_to_regex(glob) {
        Ok(re) => re.is_match(candidate),
        Err(_) => false,
    })
}

/// Find the first profile (in iteration order) whose matcher list matches
/// `connection_id`, given as `(profile_id, matchers)` pairs. Returns `None`
/// if `connection_id` is `None` (no active connection) or nothing matches.
pub fn find_matching_profile<'a, I>(profiles: I, connection_id: Option<&str>) -> Option<String>
where
    I: IntoIterator<Item = (&'a str, &'a [String])>,
{
    let candidate = connection_id?;
    profiles
        .into_iter()
        .find(|(_, matchers)| matches_any(matchers, candidate))
        .map(|(id, _)| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(matches_any(&["Home Wi-Fi".to_string()], "Home Wi-Fi"));
        assert!(!matches_any(&["Home Wi-Fi".to_string()], "Office Wi-Fi"));
    }

    #[test]
    fn glob_star_matches_prefix() {
        assert!(matches_any(&["Home*".to_string()], "Home Wi-Fi 5G"));
        assert!(!matches_any(&["Home*".to_string()], "Office"));
    }

    #[test]
    fn empty_matcher_list_never_matches() {
        assert!(!matches_any(&[], "anything"));
    }

    #[test]
    fn multiple_matchers_any_can_match() {
        let matchers = vec!["Office*".to_string(), "Work-VPN".to_string()];
        assert!(matches_any(&matchers, "Work-VPN"));
        assert!(matches_any(&matchers, "Office-5G"));
        assert!(!matches_any(&matchers, "Home"));
    }

    #[test]
    fn find_matching_profile_returns_first_match() {
        let home = vec!["Home*".to_string()];
        let office = vec!["Office*".to_string()];
        let profiles: Vec<(&str, &[String])> =
            vec![("home", home.as_slice()), ("office", office.as_slice())];
        assert_eq!(
            find_matching_profile(profiles.clone(), Some("Office-5G")),
            Some("office".to_string())
        );
        assert_eq!(find_matching_profile(profiles, Some("Unknown")), None);
    }

    #[test]
    fn find_matching_profile_none_connection_never_matches() {
        let home = vec!["Home*".to_string()];
        let profiles: Vec<(&str, &[String])> = vec![("home", home.as_slice())];
        assert_eq!(find_matching_profile(profiles, None), None);
    }
}
