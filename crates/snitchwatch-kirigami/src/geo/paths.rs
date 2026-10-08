//! GeoLite2-Country `.mmdb` discovery.
//!
//! We never download or vendor a GeoIP database (see the design spec's
//! privacy/non-goals). Instead we look for one an operator has placed
//! themselves, in this priority order:
//!
//!   1. `$SNITCHWATCH_GEOIP_DB` — explicit override, any path.
//!   2. `$XDG_DATA_HOME/snitchwatch/GeoLite2-Country.mmdb` (falling back to
//!      `~/.local/share` when `XDG_DATA_HOME` is unset), matching
//!      `crate::paths::data_dir`.
//!   3. Common system package locations:
//!      `/usr/share/GeoIP/GeoLite2-Country.mmdb`,
//!      `/var/lib/GeoIP/GeoLite2-Country.mmdb`.
//!
//! If none exist, [`discover_with`] returns `None` and the caller degrades
//! to the no-DB setup state — this is expected/common, not an error.
//!
//! Both the filesystem check (an `exists` closure) and the environment values
//! (`env_override`, `home`, `xdg_data_home`) are injected into
//! [`discover_with`], so every branch of the priority order is unit-testable
//! without touching the real filesystem, `/usr`, `/var`, or — crucially — the
//! process environment, which is global state that parallel tests would race
//! on. The one place the real environment is read is
//! [`super::resolver::discover_and_open`].

use std::path::{Path, PathBuf};

/// The default (best) location for an operator to place their own database —
/// surfaced in the QML setup placeholder when no database is found anywhere.
pub fn default_suggested_path(home: &str, xdg_data_home: Option<&str>) -> PathBuf {
    data_home_dir(home, xdg_data_home).join("GeoLite2-Country.mmdb")
}

fn data_home_dir(home: &str, xdg_data_home: Option<&str>) -> PathBuf {
    match xdg_data_home {
        Some(p) if !p.is_empty() => PathBuf::from(p).join("snitchwatch"),
        _ => PathBuf::from(home).join(".local/share/snitchwatch"),
    }
}

/// Ordered discovery candidates, cheapest/most-specific first. Pure: no
/// filesystem access.
fn candidate_paths(home: &str, xdg_data_home: Option<&str>) -> Vec<PathBuf> {
    vec![
        data_home_dir(home, xdg_data_home).join("GeoLite2-Country.mmdb"),
        PathBuf::from("/usr/share/GeoIP/GeoLite2-Country.mmdb"),
        PathBuf::from("/var/lib/GeoIP/GeoLite2-Country.mmdb"),
    ]
}

/// Core discovery logic, parameterised over an `exists` check so tests can
/// simulate any candidate — including the hardcoded system paths — existing
/// or not, without touching the real filesystem.
///
/// An `env_override` that doesn't exist is not fatal: we log a warning and
/// keep searching the rest of the priority order, since a stale/typo'd
/// override shouldn't fully disable the panel when a database is available
/// elsewhere.
pub fn discover_with(
    exists: impl Fn(&Path) -> bool,
    env_override: Option<&str>,
    home: &str,
    xdg_data_home: Option<&str>,
) -> Option<PathBuf> {
    if let Some(p) = env_override {
        let candidate = PathBuf::from(p);
        if exists(&candidate) {
            return Some(candidate);
        }
        tracing::warn!(
            path = p,
            "SNITCHWATCH_GEOIP_DB is set but the file does not exist; \
             falling back to the standard search paths"
        );
    }
    candidate_paths(home, xdg_data_home)
        .into_iter()
        .find(|p| exists(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn env_override_wins_when_present() {
        let found = discover_with(
            |p| p == Path::new("/custom/geoip.mmdb"),
            Some("/custom/geoip.mmdb"),
            "/home/alice",
            None,
        );
        assert_eq!(found, Some(PathBuf::from("/custom/geoip.mmdb")));
    }

    #[test]
    fn env_override_missing_falls_back_to_xdg_data_home() {
        let xdg_candidate = PathBuf::from("/xdg/snitchwatch/GeoLite2-Country.mmdb");
        let existing: HashSet<PathBuf> = [xdg_candidate.clone()].into_iter().collect();
        let found = discover_with(
            |p| existing.contains(p),
            Some("/nonexistent/override.mmdb"),
            "/home/alice",
            Some("/xdg"),
        );
        assert_eq!(found, Some(xdg_candidate));
    }

    #[test]
    fn xdg_data_home_used_when_set() {
        let found = discover_with(
            |p| p == Path::new("/xdg/snitchwatch/GeoLite2-Country.mmdb"),
            None,
            "/home/alice",
            Some("/xdg"),
        );
        assert_eq!(
            found,
            Some(PathBuf::from("/xdg/snitchwatch/GeoLite2-Country.mmdb"))
        );
    }

    #[test]
    fn falls_back_to_home_local_share_when_xdg_unset() {
        let found = discover_with(
            |p| p == Path::new("/home/alice/.local/share/snitchwatch/GeoLite2-Country.mmdb"),
            None,
            "/home/alice",
            None,
        );
        assert_eq!(
            found,
            Some(PathBuf::from(
                "/home/alice/.local/share/snitchwatch/GeoLite2-Country.mmdb"
            ))
        );
    }

    #[test]
    fn falls_back_to_system_paths_in_order() {
        let existing: HashSet<PathBuf> = [
            PathBuf::from("/usr/share/GeoIP/GeoLite2-Country.mmdb"),
            PathBuf::from("/var/lib/GeoIP/GeoLite2-Country.mmdb"),
        ]
        .into_iter()
        .collect();
        let found = discover_with(|p| existing.contains(p), None, "/home/alice", None);
        // /usr/share/GeoIP is checked before /var/lib/GeoIP.
        assert_eq!(
            found,
            Some(PathBuf::from("/usr/share/GeoIP/GeoLite2-Country.mmdb"))
        );
    }

    #[test]
    fn var_lib_used_when_usr_share_absent() {
        let existing: HashSet<PathBuf> = [PathBuf::from("/var/lib/GeoIP/GeoLite2-Country.mmdb")]
            .into_iter()
            .collect();
        let found = discover_with(|p| existing.contains(p), None, "/home/alice", None);
        assert_eq!(
            found,
            Some(PathBuf::from("/var/lib/GeoIP/GeoLite2-Country.mmdb"))
        );
    }

    #[test]
    fn none_found_returns_none() {
        let found = discover_with(|_| false, None, "/home/alice", None);
        assert_eq!(found, None);
    }

    #[test]
    fn default_suggested_path_prefers_xdg_data_home() {
        assert_eq!(
            default_suggested_path("/home/alice", Some("/xdg")),
            PathBuf::from("/xdg/snitchwatch/GeoLite2-Country.mmdb")
        );
        assert_eq!(
            default_suggested_path("/home/alice", None),
            PathBuf::from("/home/alice/.local/share/snitchwatch/GeoLite2-Country.mmdb")
        );
    }

    #[test]
    fn env_override_is_found_on_the_real_filesystem() {
        // Every other test above injects a fake `exists`; this one runs the
        // production check (`Path::exists`) against a real temp file. The
        // override is passed as a parameter, never via `set_var`, so it can't
        // race with any other test over `SNITCHWATCH_GEOIP_DB`.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("GeoLite2-Country.mmdb");
        std::fs::write(
            &db_path,
            b"not a real mmdb, existence is all that matters here",
        )
        .unwrap();

        let found = discover_with(
            |p| p.exists(),
            db_path.to_str(),
            "/nonexistent-home-for-tests",
            None,
        );

        assert_eq!(found, Some(db_path));
    }
}
