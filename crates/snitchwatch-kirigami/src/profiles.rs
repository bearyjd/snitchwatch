//! Profiles domain: the Profiles tab's data model.
//!
//! - [`row_store`]: pure, Qt-free store for the flat profile list — fully
//!   unit-tested here.
//! - The cxx-qt `QAbstractListModel` wrapper that binds this to QML lives in
//!   the top-level [`crate::profiles_model`] module (kept flat under `src/`
//!   with the other `#[cxx_qt::bridge]` files, per the same cxx-qt-build
//!   one-directory constraint noted in [`crate::connections`]).

pub mod row_store;

/// Split a comma-separated network-matcher editor string into trimmed,
/// non-empty glob patterns. Shared by `ProfilesModel`'s create/update
/// invokables so both parse the simple string-list editor identically.
pub fn parse_matchers(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Derive a stable, URL/filename-safe profile id from a user-entered display
/// name (e.g. `"At Home"` -> `"at-home"`), disambiguating against
/// `existing_ids` by appending `-2`, `-3`, … if needed. Mirrors
/// `snitchwatch_bridge::blocklists::derive_id`'s sanitization approach, but
/// takes existing ids into account since profile names (unlike blocklist
/// URLs) have no other natural uniqueness source.
pub fn derive_profile_id(name: &str, existing_ids: &[String]) -> String {
    let mut slug: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        slug = "profile".to_string();
    }
    if !existing_ids.iter().any(|id| id == &slug) {
        return slug;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{slug}-{n}");
        if !existing_ids.iter().any(|id| id == &candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_matchers_trims_and_drops_empty_entries() {
        assert_eq!(
            parse_matchers(" Home* , Office-5G ,, "),
            vec!["Home*".to_string(), "Office-5G".to_string()]
        );
    }

    #[test]
    fn parse_matchers_empty_string_yields_empty_vec() {
        assert!(parse_matchers("").is_empty());
        assert!(parse_matchers("   ").is_empty());
    }

    #[test]
    fn derive_profile_id_slugifies_name() {
        assert_eq!(derive_profile_id("At Home", &[]), "at-home");
        assert_eq!(derive_profile_id("Public Wi-Fi!!", &[]), "public-wi-fi");
    }

    #[test]
    fn derive_profile_id_disambiguates_collisions() {
        let existing = vec!["at-home".to_string()];
        assert_eq!(derive_profile_id("At Home", &existing), "at-home-2");
        let existing2 = vec!["at-home".to_string(), "at-home-2".to_string()];
        assert_eq!(derive_profile_id("At Home", &existing2), "at-home-3");
    }

    #[test]
    fn derive_profile_id_empty_name_falls_back_to_profile() {
        assert_eq!(derive_profile_id("!!!", &[]), "profile");
    }
}
