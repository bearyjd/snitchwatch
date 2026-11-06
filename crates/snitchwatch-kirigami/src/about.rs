//! About page logic: what build is running, as plain data.
//!
//! Built from injectable inputs (build constants, the text of
//! `/.flatpak-info`, the Qt runtime version, the connected daemon's version)
//! so it is unit-testable without a Flatpak. `about_model` binds it to QML.
//! The Flatpak file is untrusted-ish: read at most [`MAX_FLATPAK_INFO_BYTES`],
//! parsed tolerantly, and every shown value is stripped of control and
//! bidi-override characters and length-capped.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Where the Flatpak runtime describes the running instance.
pub const FLATPAK_INFO_PATH: &str = "/.flatpak-info";
/// A real file is ~1 KiB; anything past this is not read.
pub const MAX_FLATPAK_INFO_BYTES: u64 = 64 * 1024;
/// Longest value shown or copied; longer ones end in an ellipsis.
pub const MAX_VALUE_CHARS: usize = 200;
pub const UNKNOWN: &str = "unknown";
pub const NOT_RECORDED: &str = "not recorded in this build";
pub const NOT_FLATPAK: &str = "Not running in Flatpak";
pub const NOT_CONNECTED: &str = "not connected";

/// Compile-time identity, from `Cargo.toml` and `build.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    /// Source commit, `None` when the build could not record one.
    pub commit: Option<String>,
    /// The tree had uncommitted changes when it was built.
    pub dirty: bool,
}

impl BuildInfo {
    pub fn current() -> Self {
        let commit = env!("SNITCHWATCH_BUILD_COMMIT_RESOLVED");
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: (!commit.is_empty()).then(|| commit.to_string()),
            dirty: env!("SNITCHWATCH_BUILD_DIRTY") == "1",
        }
    }
}

/// The fields we read from `/.flatpak-info`; `None` when absent or empty.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FlatpakInfo {
    pub app: Option<String>,
    pub runtime: Option<String>,
    pub app_commit: Option<String>,
    pub runtime_commit: Option<String>,
    pub branch: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlatpakState {
    NotFlatpak,
    /// The file exists but could not be read as a regular file.
    Unreadable,
    Flatpak(FlatpakInfo),
}

/// One labelled row of the About page and of the copied text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
}

/// Drops control and bidi-override characters, trims, and caps the length.
/// `None` when nothing printable is left.
fn clean(raw: &str) -> Option<String> {
    let kept: String = raw
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    let kept = kept.trim();
    if kept.is_empty() {
        return None;
    }
    if kept.chars().count() <= MAX_VALUE_CHARS {
        return Some(kept.to_string());
    }
    let mut cut: String = kept.chars().take(MAX_VALUE_CHARS - 1).collect();
    cut.push('\u{2026}');
    Some(cut)
}

/// Tolerant INI read: unknown sections, keys and malformed lines are skipped;
/// the first occurrence of a key wins.
pub fn parse_flatpak_info(text: &str) -> FlatpakInfo {
    let mut info = FlatpakInfo::default();
    let mut section = "";
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match (section, key.trim()) {
            ("Application", "name") => &mut info.app,
            ("Application", "runtime") => &mut info.runtime,
            ("Instance", "app-commit") => &mut info.app_commit,
            ("Instance", "runtime-commit") => &mut info.runtime_commit,
            ("Instance", "branch") => &mut info.branch,
            ("Instance", "arch") => &mut info.arch,
            _ => continue,
        };
        if slot.is_none() {
            *slot = clean(value);
        }
    }
    info
}

/// Reads `path` (at most [`MAX_FLATPAK_INFO_BYTES`]; a FIFO or a directory
/// never blocks) and parses it. A missing file means we are not in a Flatpak.
pub fn load_flatpak_state(path: &Path) -> FlatpakState {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path);
    let file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FlatpakState::NotFlatpak,
        Err(_) => return FlatpakState::Unreadable,
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return FlatpakState::Unreadable;
    }
    let mut bytes = Vec::new();
    if file
        .take(MAX_FLATPAK_INFO_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return FlatpakState::Unreadable;
    }
    FlatpakState::Flatpak(parse_flatpak_info(&String::from_utf8_lossy(&bytes)))
}

fn field(key: &'static str, label: &'static str, value: String) -> Field {
    Field { key, label, value }
}

fn or_unknown(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| UNKNOWN.to_string())
}

/// The rows shown and copied, in order. `daemon_version` is `None` (or blank)
/// while no daemon has reported one.
pub fn fields(
    build: &BuildInfo,
    flatpak: &FlatpakState,
    qt_version: &str,
    daemon_version: Option<&str>,
) -> Vec<Field> {
    let commit = match (&build.commit, build.dirty) {
        (None, _) => NOT_RECORDED.to_string(),
        (Some(c), false) => c.clone(),
        (Some(c), true) => format!("{c} (uncommitted changes)"),
    };
    let mut rows = vec![
        field(
            "gui_version",
            "Snitchwatch GUI version",
            clean(&build.version).unwrap_or_else(|| UNKNOWN.to_string()),
        ),
        field("build_commit", "Source commit", commit),
    ];
    let info = match flatpak {
        FlatpakState::NotFlatpak => {
            rows.push(field("flatpak", "Flatpak", NOT_FLATPAK.to_string()));
            None
        }
        FlatpakState::Unreadable => {
            rows.push(field(
                "flatpak",
                "Flatpak",
                "Flatpak info unreadable".to_string(),
            ));
            Some(FlatpakInfo::default())
        }
        FlatpakState::Flatpak(info) => {
            rows.push(field(
                "flatpak",
                "Flatpak",
                "Running in Flatpak".to_string(),
            ));
            Some(info.clone())
        }
    };
    if let Some(info) = info {
        rows.extend([
            field("flatpak_app", "Flatpak app", or_unknown(&info.app)),
            field(
                "flatpak_app_commit",
                "Flatpak app commit",
                or_unknown(&info.app_commit),
            ),
            field("flatpak_branch", "Flatpak branch", or_unknown(&info.branch)),
            field(
                "flatpak_runtime",
                "Flatpak runtime",
                or_unknown(&info.runtime),
            ),
            field(
                "flatpak_runtime_commit",
                "Flatpak runtime commit",
                or_unknown(&info.runtime_commit),
            ),
        ]);
    }
    rows.push(field(
        "qt_version",
        "Qt (running)",
        clean(qt_version).unwrap_or_else(|| UNKNOWN.to_string()),
    ));
    rows.push(field(
        "daemon_version",
        "opensnitchd (daemon)",
        daemon_version
            .and_then(clean)
            .unwrap_or_else(|| NOT_CONNECTED.to_string()),
    ));
    rows
}

/// Plain text for a bug report: a heading, then `label: value` per row.
pub fn report_text(fields: &[Field]) -> String {
    let mut text = String::from("Snitchwatch version info\n");
    for f in fields {
        text.push_str(&format!("{}: {}\n", f.label, f.value));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "[Application]\nname=org.snitchwatch.Snitchwatch\nruntime=runtime/org.kde.Platform/x86_64/6.8\n\n[Instance]\ninstance-id=123\nbranch=stable\narch=x86_64\nflatpak-version=1.16.1\napp-commit=aaaa1111\nruntime-commit=bbbb2222\n";

    fn build() -> BuildInfo {
        BuildInfo {
            version: "0.1.1".into(),
            commit: Some("c0ffee".into()),
            dirty: false,
        }
    }

    fn value<'a>(fields: &'a [Field], key: &str) -> &'a str {
        &fields.iter().find(|f| f.key == key).expect(key).value
    }

    #[test]
    fn parses_the_fields_of_a_real_file() {
        let info = parse_flatpak_info(REAL);
        assert_eq!(info.app.as_deref(), Some("org.snitchwatch.Snitchwatch"));
        assert_eq!(
            info.runtime.as_deref(),
            Some("runtime/org.kde.Platform/x86_64/6.8")
        );
        assert_eq!(info.app_commit.as_deref(), Some("aaaa1111"));
        assert_eq!(info.runtime_commit.as_deref(), Some("bbbb2222"));
        assert_eq!(info.branch.as_deref(), Some("stable"));
        assert_eq!(info.arch.as_deref(), Some("x86_64"));
    }

    #[test]
    fn missing_and_empty_fields_are_none() {
        let info = parse_flatpak_info("[Application]\nname=\n[Instance]\nbranch=  \n");
        assert_eq!(info, FlatpakInfo::default());
    }

    #[test]
    fn garbage_parses_to_nothing_without_panicking() {
        let garbage = "\u{0}\u{1}\u{2}=\n[[[\n===\n[Instance\nbranch\n\u{fffd}\u{fffd}";
        assert_eq!(parse_flatpak_info(garbage), FlatpakInfo::default());
        assert_eq!(parse_flatpak_info(""), FlatpakInfo::default());
    }

    #[test]
    fn keys_only_count_in_their_own_section_and_the_first_wins() {
        let info = parse_flatpak_info(
            "[Other]\nbranch=wrong\napp-commit=wrong\n[Instance]\nbranch=first\nbranch=second\n[Application]\nbranch=wrong\n",
        );
        assert_eq!(info.branch.as_deref(), Some("first"));
        assert_eq!(info.app_commit, None);
    }

    #[test]
    fn values_are_cleaned_and_capped() {
        let long = "x".repeat(5_000);
        let text = format!(
            "[Instance]\nbranch=sta\u{1b}[31mble\u{202e}\napp-commit={long}\n[Application]\nname=a\tb\n"
        );
        let info = parse_flatpak_info(&text);
        assert_eq!(info.branch.as_deref(), Some("sta[31mble"));
        let commit = info.app_commit.expect("commit");
        assert_eq!(commit.chars().count(), MAX_VALUE_CHARS);
        assert!(commit.ends_with('…'));
        assert_eq!(info.app.as_deref(), Some("ab"));
    }

    #[test]
    fn crlf_comments_and_spacing_are_tolerated() {
        let info = parse_flatpak_info(
            "# c\r\n; c\r\n[Instance]\r\n  branch = beta \r\n[Application]\r\nname=n\r\n",
        );
        assert_eq!(info.branch.as_deref(), Some("beta"));
        assert_eq!(info.app.as_deref(), Some("n"));
    }

    #[test]
    fn a_missing_file_is_not_flatpak() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_flatpak_state(&dir.path().join("none")),
            FlatpakState::NotFlatpak
        );
    }

    #[test]
    fn a_directory_is_unreadable_not_a_hang_or_not_flatpak() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_flatpak_state(dir.path()), FlatpakState::Unreadable);
    }

    #[test]
    fn a_file_is_loaded_and_garbage_bytes_do_not_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("info");
        std::fs::write(&path, REAL).unwrap();
        match load_flatpak_state(&path) {
            FlatpakState::Flatpak(i) => assert_eq!(i.branch.as_deref(), Some("stable")),
            other => panic!("{other:?}"),
        }
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x5b, 0x80]).unwrap();
        assert_eq!(
            load_flatpak_state(&path),
            FlatpakState::Flatpak(FlatpakInfo::default())
        );
    }

    #[test]
    fn only_the_first_bytes_of_an_oversize_file_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("info");
        // Valid data first, a huge tail after: the tail must be ignored and a
        // key past the cap must not be seen.
        let mut text = String::from("[Instance]\nbranch=stable\n");
        text.push_str(&"#".repeat(MAX_FLATPAK_INFO_BYTES as usize));
        text.push_str("\narch=late\n");
        std::fs::write(&path, text).unwrap();
        match load_flatpak_state(&path) {
            FlatpakState::Flatpak(i) => {
                assert_eq!(i.branch.as_deref(), Some("stable"));
                assert_eq!(i.arch, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn non_flatpak_shows_the_state_and_no_flatpak_rows() {
        let f = fields(&build(), &FlatpakState::NotFlatpak, "6.8.1", None);
        assert_eq!(value(&f, "flatpak"), NOT_FLATPAK);
        assert!(f.iter().all(|x| !x.key.starts_with("flatpak_")));
        assert_eq!(value(&f, "gui_version"), "0.1.1");
        assert_eq!(value(&f, "build_commit"), "c0ffee");
        assert_eq!(value(&f, "qt_version"), "6.8.1");
        assert_eq!(value(&f, "daemon_version"), NOT_CONNECTED);
    }

    #[test]
    fn flatpak_rows_show_unknown_for_anything_missing() {
        let state = FlatpakState::Flatpak(FlatpakInfo {
            branch: Some("stable".into()),
            ..Default::default()
        });
        let f = fields(&build(), &state, "", Some("1.8.0"));
        assert_eq!(value(&f, "flatpak"), "Running in Flatpak");
        assert_eq!(value(&f, "flatpak_branch"), "stable");
        assert_eq!(value(&f, "flatpak_app_commit"), UNKNOWN);
        assert_eq!(value(&f, "flatpak_runtime"), UNKNOWN);
        assert_eq!(value(&f, "qt_version"), UNKNOWN);
        assert_eq!(value(&f, "daemon_version"), "1.8.0");
        let f = fields(&build(), &FlatpakState::Unreadable, "6", None);
        assert_eq!(value(&f, "flatpak"), "Flatpak info unreadable");
        assert_eq!(value(&f, "flatpak_branch"), UNKNOWN);
    }

    #[test]
    fn build_commit_states() {
        let mut b = build();
        b.dirty = true;
        let f = fields(&b, &FlatpakState::NotFlatpak, "6", None);
        assert_eq!(value(&f, "build_commit"), "c0ffee (uncommitted changes)");
        b.commit = None;
        let f = fields(&b, &FlatpakState::NotFlatpak, "6", None);
        assert_eq!(value(&f, "build_commit"), NOT_RECORDED);
    }

    #[test]
    fn daemon_version_is_cleaned_and_empty_means_not_connected() {
        let long = "9".repeat(1_000);
        let f = fields(&build(), &FlatpakState::NotFlatpak, "6", Some(&long));
        assert_eq!(value(&f, "daemon_version").chars().count(), MAX_VALUE_CHARS);
        let f = fields(
            &build(),
            &FlatpakState::NotFlatpak,
            "6",
            Some("1.\n8\u{202e}"),
        );
        assert_eq!(value(&f, "daemon_version"), "1.8");
        let f = fields(&build(), &FlatpakState::NotFlatpak, "6", Some("  "));
        assert_eq!(value(&f, "daemon_version"), NOT_CONNECTED);
    }

    #[test]
    fn report_text_is_one_labelled_line_per_field() {
        let f = fields(&build(), &FlatpakState::NotFlatpak, "6.8.1", Some("1.8.0"));
        let text = report_text(&f);
        assert!(text.starts_with("Snitchwatch version info\n"), "{text}");
        for field in &f {
            assert!(
                text.contains(&format!("{}: {}\n", field.label, field.value)),
                "{text}"
            );
        }
        assert_eq!(text.lines().count(), f.len() + 1);
        assert!(text.ends_with('\n'));
    }
}
