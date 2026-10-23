//! Persisted user preferences: the RDAP opt-in, the duration an Allow
//! remembers, and the silent mode (LS redesign phase 1).
//!
//! Pure `std::fs` + `serde_json` logic, no Qt/bridge dependency — mirrors
//! `crate::autostart`'s split between plain-Rust read/write functions and
//! the cxx-qt controller (`settings_controller.rs`) that calls them off the
//! Qt thread. Stored as a small JSON file rather than a presence-only flag
//! (as autostart's `.desktop` file is) since this is expected to grow more
//! than one preference.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// What happens to a new prompt without anyone being asked (LS redesign
/// phase 1). `Off` shows the alert; `Allow`/`Deny` answer it once at arrival
/// and mark the row "needs a rule".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SilentMode {
    #[default]
    Off,
    Allow,
    Deny,
}

impl SilentMode {
    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    /// `None` for anything but the three tokens: a UI bug must not turn
    /// into a silent Deny.
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "off" => Some(Self::Off),
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// The duration tokens a default may hold (`pending_decision::parse_duration`
/// vocabulary). The one list the alert, the sheet and the settings page use is
/// `qml/Durations.qml`; a test keeps them equal.
pub const DURATION_TOKENS: [&str; 4] = ["this_time", "for_5_minutes", "until_restart", "forever"];

/// The label `qml/Durations.qml` shows for a duration token (a test keeps the
/// two equal), for surfaces that cannot read the QML list.
pub fn duration_label(token: &str) -> &'static str {
    match normalize_duration(token) {
        "this_time" => "Once",
        "for_5_minutes" => "5 minutes",
        "until_restart" => "Until firewall restarts",
        _ => "Forever",
    }
}

/// What an Allow remembers until the user picks otherwise. Not `this_time`:
/// the daemon stores only `always` rules, so a once-only Allow means the same
/// prompt next time.
pub const DEFAULT_DURATION: &str = "forever";

/// `token` when it names a duration (the pre-rename `until_quit` included),
/// else [`DEFAULT_DURATION`].
pub fn normalize_duration(token: &str) -> &'static str {
    let token = if token == "until_quit" {
        "until_restart"
    } else {
        token
    };
    DURATION_TOKENS
        .into_iter()
        .find(|known| *known == token)
        .unwrap_or(DEFAULT_DURATION)
}

fn default_duration() -> String {
    DEFAULT_DURATION.to_string()
}

/// Persisted preferences. `#[serde(default)]` on every field means an older
/// or partially-written file still parses — missing fields fall back to
/// their default rather than failing the whole read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Whether the pending-decision insight panel is allowed to query
    /// `rdap.org` for registration info. Opt-in, default off: a firewall
    /// app must not send every pending connection's remote IP to a
    /// third-party service unless the user has explicitly asked for it.
    /// Reverse DNS (via the system resolver) is unaffected by this flag.
    #[serde(default)]
    pub rdap_enabled: bool,
    /// The duration token inline and notification Allow/Deny remember for
    /// (and the alert preselects). See [`normalize_duration`].
    #[serde(default = "default_duration")]
    pub default_duration: String,
    /// `None`: the user has not chosen yet (onboarding asks). Acts as `Off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silent_mode: Option<SilentMode>,
    /// A running learning window (a time-bounded "Allow and record"), if any.
    /// See [`Learning`] and `crate::learning`. Read through [`read_settings`],
    /// which clamps it, so a hand-edited file cannot create a longer window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning: Option<Learning>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rdap_enabled: false,
            default_duration: default_duration(),
            silent_mode: None,
            learning: None,
        }
    }
}

/// Default length of a learning window: 24 hours.
pub const LEARNING_DEFAULT_MS: i64 = 24 * 60 * 60 * 1000;
/// The longest a learning window may ever be: 7 days.
pub const LEARNING_MAX_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// A learning window, persisted next to `silent_mode` while it runs. It can
/// only END EARLY: it is over when the wall clock passes `ends_ms` OR the
/// running time counted in `elapsed_ms` reaches `duration_ms`, so a clock set
/// backwards cannot stretch it (`crate::learning::is_expired`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Learning {
    /// Wall-clock start, milliseconds since the Unix epoch.
    pub started_ms: i64,
    /// Wall-clock end, milliseconds since the Unix epoch.
    pub ends_ms: i64,
    /// Intended length of the window.
    pub duration_ms: i64,
    /// Time the GUI has actually been running inside the window, saved about
    /// once a minute.
    pub elapsed_ms: i64,
    /// What `silent_mode` was before the window, restored when it ends.
    /// `None` is "unchosen" and is restored as unchosen, not as `Off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_mode: Option<SilentMode>,
}

impl Learning {
    /// A window starting at `now_ms` lasting `duration_ms`, bounded by
    /// [`LEARNING_MAX_MS`] (a negative length is an already-over window).
    pub fn begin(now_ms: i64, duration_ms: i64, previous_mode: Option<SilentMode>) -> Self {
        Self {
            started_ms: now_ms,
            ends_ms: now_ms.saturating_add(duration_ms),
            duration_ms,
            elapsed_ms: 0,
            previous_mode,
        }
        .clamped()
    }

    /// The same window forced inside the limits: never longer than
    /// [`LEARNING_MAX_MS`], never ending later than `started_ms` plus that.
    /// Any negative figure makes it an already-over window (zero length), so
    /// a damaged file ends learning instead of extending it.
    pub fn clamped(&self) -> Self {
        if [
            self.started_ms,
            self.ends_ms,
            self.duration_ms,
            self.elapsed_ms,
        ]
        .iter()
        .any(|v| *v < 0)
        {
            return Self {
                duration_ms: 0,
                elapsed_ms: 0,
                ..self.clone()
            };
        }
        Self {
            duration_ms: self.duration_ms.min(LEARNING_MAX_MS),
            ends_ms: self
                .ends_ms
                .min(self.started_ms.saturating_add(LEARNING_MAX_MS)),
            ..self.clone()
        }
    }
}

impl Settings {
    /// The duration token to use, always a known one.
    pub fn duration_token(&self) -> &'static str {
        normalize_duration(&self.default_duration)
    }

    /// The mode in force: not chosen acts as `Off`.
    pub fn effective_silent_mode(&self) -> SilentMode {
        self.silent_mode.unwrap_or_default()
    }
}

/// Read persisted settings, degrading to [`Settings::default`] on any
/// failure (missing file, unreadable, malformed JSON) — a corrupt or absent
/// settings file must never crash or block startup.
pub fn read_settings(path: &Path) -> Settings {
    let settings: Settings = std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default();
    Settings {
        learning: settings.learning.as_ref().map(Learning::clamped),
        ..settings
    }
}

/// Persist `settings` as pretty-printed JSON, into an owner-only directory it
/// creates or re-checks (0700), as an owner-only file (0600).
pub fn write_settings(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let body = serde_json::to_string_pretty(settings)
        .unwrap_or_else(|_| "{\"rdap_enabled\":false}".to_string());
    // Re-checks the directory on every write (#170): the cached no-HOME root
    // check can go stale if `/tmp/snitchwatch-<uid>` is recreated meanwhile.
    crate::private_files::write_private(path, body.as_bytes())
}

/// Convenience read for the one flag `PendingInsight::lookup` needs, without
/// callers having to know the full [`Settings`] shape.
pub fn read_rdap_enabled(path: &Path) -> bool {
    read_settings(path).rdap_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_settings_defaults_to_rdap_disabled_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        assert_eq!(read_settings(&p), Settings::default());
        assert!(!read_rdap_enabled(&p));
    }

    #[test]
    fn read_settings_defaults_to_rdap_disabled_when_file_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(read_settings(&p), Settings::default());
    }

    #[test]
    fn write_then_read_settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("settings.json");
        write_settings(
            &p,
            &Settings {
                rdap_enabled: true,
                ..Settings::default()
            },
        )
        .unwrap();
        assert!(read_rdap_enabled(&p));
    }

    /// #170: a settings write re-checks its directory: a foreign-owned one is
    /// refused and nothing lands in it; a symlinked file isn't followed.
    #[test]
    fn write_settings_refuses_a_foreign_dir_and_a_symlinked_file() {
        let (foreign, _guard) = crate::private_files::foreign_owned_dir();
        let p = foreign.join("settings.json");
        assert!(write_settings(&p, &Settings::default()).is_err());
        assert!(!p.exists());

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        let link = dir.path().join("settings.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_settings(&link, &Settings::default()).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    #[test]
    fn write_settings_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write_settings(
            &p,
            &Settings {
                rdap_enabled: true,
                ..Settings::default()
            },
        )
        .unwrap();
        write_settings(
            &p,
            &Settings {
                rdap_enabled: false,
                ..Settings::default()
            },
        )
        .unwrap();
        assert!(!read_rdap_enabled(&p));
    }

    #[test]
    fn read_settings_tolerates_a_partial_json_object() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, "{}").unwrap();
        assert_eq!(read_settings(&p), Settings::default());
    }

    #[test]
    fn an_old_file_gets_a_remembering_default_and_no_silent_choice() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, r#"{"rdap_enabled":true}"#).unwrap();
        let settings = read_settings(&p);
        assert!(settings.rdap_enabled);
        assert_eq!(settings.duration_token(), "forever");
        assert_eq!(settings.silent_mode, None);
        assert_eq!(settings.effective_silent_mode(), SilentMode::Off);
    }

    #[test]
    fn duration_and_silent_mode_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let settings = Settings {
            default_duration: "until_restart".into(),
            silent_mode: Some(SilentMode::Deny),
            ..Settings::default()
        };
        write_settings(&p, &settings).unwrap();
        assert_eq!(read_settings(&p), settings);
        assert!(std::fs::read_to_string(&p)
            .unwrap()
            .contains("\"silent_mode\": \"deny\""));
    }

    #[test]
    fn an_unchosen_silent_mode_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write_settings(&p, &Settings::default()).unwrap();
        assert!(!std::fs::read_to_string(&p).unwrap().contains("silent_mode"));
    }

    #[test]
    fn a_garbled_duration_falls_back_to_forever_and_the_old_token_is_renamed() {
        let with = |d: &str| Settings {
            default_duration: d.into(),
            ..Settings::default()
        };
        assert_eq!(with("bogus").duration_token(), "forever");
        assert_eq!(with("").duration_token(), "forever");
        assert_eq!(with("until_quit").duration_token(), "until_restart");
        for token in DURATION_TOKENS {
            assert_eq!(with(token).duration_token(), token);
        }
    }

    #[test]
    fn silent_mode_tokens_round_trip_and_reject_unknown() {
        for mode in [SilentMode::Off, SilentMode::Allow, SilentMode::Deny] {
            assert_eq!(SilentMode::from_token(mode.token()), Some(mode));
        }
        assert_eq!(SilentMode::from_token("deny "), None);
        assert_eq!(SilentMode::from_token(""), None);
    }

    fn window() -> Learning {
        Learning {
            started_ms: 1_000,
            ends_ms: 1_000 + LEARNING_DEFAULT_MS,
            duration_ms: LEARNING_DEFAULT_MS,
            elapsed_ms: 60_000,
            previous_mode: Some(SilentMode::Deny),
        }
    }

    #[test]
    fn an_old_file_has_no_learning_window() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, r#"{"rdap_enabled":true,"silent_mode":"deny"}"#).unwrap();
        let settings = read_settings(&p);
        assert_eq!(settings.learning, None);
        assert_eq!(settings.silent_mode, Some(SilentMode::Deny));
        write_settings(&p, &settings).unwrap();
        assert!(!std::fs::read_to_string(&p).unwrap().contains("learning"));
    }

    #[test]
    fn a_learning_window_round_trips_including_an_unchosen_previous_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        for previous_mode in [None, Some(SilentMode::Off), Some(SilentMode::Deny)] {
            let settings = Settings {
                silent_mode: Some(SilentMode::Allow),
                learning: Some(Learning {
                    previous_mode,
                    ..window()
                }),
                ..Settings::default()
            };
            write_settings(&p, &settings).unwrap();
            assert_eq!(read_settings(&p), settings);
        }
    }

    #[test]
    fn a_hostile_file_cannot_create_a_longer_window() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(
            &p,
            r#"{"silent_mode":"allow","learning":{"started_ms":5,"ends_ms":9223372036854775807,
               "duration_ms":9223372036854775807,"elapsed_ms":0}}"#,
        )
        .unwrap();
        let learning = read_settings(&p).learning.unwrap();
        assert_eq!(learning.duration_ms, LEARNING_MAX_MS);
        assert_eq!(learning.ends_ms, 5 + LEARNING_MAX_MS);
        assert_eq!(learning.previous_mode, None);
    }

    #[test]
    fn negative_figures_make_an_already_over_window() {
        for hostile in [
            Learning {
                duration_ms: -1,
                ..window()
            },
            Learning {
                ends_ms: -1,
                ..window()
            },
            Learning {
                started_ms: -1,
                ..window()
            },
            Learning {
                elapsed_ms: -1,
                ..window()
            },
        ] {
            let clamped = hostile.clamped();
            assert_eq!(clamped.duration_ms, 0, "{hostile:?}");
            assert!(clamped.elapsed_ms >= clamped.duration_ms);
        }
    }

    #[test]
    fn begin_clamps_the_length_and_survives_extreme_input() {
        let l = Learning::begin(1_000, LEARNING_DEFAULT_MS, None);
        assert_eq!(
            (l.started_ms, l.ends_ms, l.elapsed_ms),
            (1_000, 1_000 + LEARNING_DEFAULT_MS, 0)
        );
        let long = Learning::begin(1_000, i64::MAX, Some(SilentMode::Off));
        assert_eq!(long.duration_ms, LEARNING_MAX_MS);
        assert_eq!(long.ends_ms, 1_000 + LEARNING_MAX_MS);
        let late = Learning::begin(i64::MAX, LEARNING_MAX_MS, None);
        assert_eq!(late.ends_ms, i64::MAX);
        assert_eq!(Learning::begin(1_000, -5, None).duration_ms, 0);
    }
}
