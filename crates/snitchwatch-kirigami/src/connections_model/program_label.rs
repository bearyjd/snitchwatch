//! The friendly program label roles (`programLabel`, `programLabelUnverified`,
//! `programPathTail`) of the Connections list.
//!
//! Display only. Group keys stay path-based (`connections::grouping::keys`)
//! and the label is never an input to any grouping, filter or answer decision.
//! A label that [`needs_marker`](ProgramName::needs_marker) comes with the
//! tail of the real path (the QML also shows the full path in a tooltip).

use super::*;
use crate::app_names::{friendly_name, is_stripped, sanitize_display, ProgramName};

/// Label for a program given its path (if known) and raw process name.
pub(super) fn program_label_for(process_path: Option<&str>, raw: &str) -> ProgramName {
    let path = process_path.filter(|p| !p.trim().is_empty());
    let desktop = path.and_then(crate::app_icons::name_for_path);
    label_with(path, raw, desktop.as_deref())
}

/// Same, with the `.desktop` name supplied (the pure part).
fn label_with(process_path: Option<&str>, raw: &str, desktop_name: Option<&str>) -> ProgramName {
    friendly_name(
        process_path.filter(|p| !p.trim().is_empty()),
        raw,
        desktop_name,
    )
}

/// Last two components of `path`, `…/`-prefixed when others were dropped,
/// each sanitized; empty for no path.
pub fn path_tail(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|c| !c.trim().is_empty()).collect();
    if parts.is_empty() {
        return String::new();
    }
    let kept: Vec<String> = parts[parts.len().saturating_sub(2)..]
        .iter()
        .map(|c| sanitize_display(c))
        .collect();
    let prefix = if parts.len() > 2 {
        "…/"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    format!("{prefix}{}", kept.join("/"))
}

/// Longest `programPathFull`, in characters, ellipsis included.
const MAX_FULL_CHARS: usize = 300;

/// The full path for the tooltip, with every control, bidi, format or
/// whitespace character other than a plain space replaced by a visible escape
/// (`\n`, `\u{202e}`), never stripped, so an RLO cannot reorder the real
/// path. Capped at [`MAX_FULL_CHARS`] with a trailing `…`; empty for no path.
pub fn path_full_display(path: &str) -> String {
    let escaped: String = path
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            c if c != ' ' && (c.is_whitespace() || is_stripped(c)) => {
                format!("\\u{{{:x}}}", c as u32)
            }
            c => c.to_string(),
        })
        .collect();
    if escaped.chars().count() <= MAX_FULL_CHARS {
        return escaped;
    }
    let mut capped: String = escaped.chars().take(MAX_FULL_CHARS - 1).collect();
    capped.push('…');
    capped
}

/// The role's value for `name`; the tail is shown only while the label needs
/// the unverified marker.
fn answer(name: &ProgramName, path: &str, role: i32) -> QVariant {
    match role {
        ROLE_PROGRAM_LABEL => QVariant::from(&QString::from(&name.label)),
        ROLE_PROGRAM_LABEL_UNVERIFIED => QVariant::from(&name.needs_marker()),
        ROLE_PROGRAM_PATH_TAIL if name.needs_marker() => {
            QVariant::from(&QString::from(&path_tail(path)))
        }
        ROLE_PROGRAM_PATH_TAIL => QVariant::from(&QString::from("")),
        ROLE_PROGRAM_PATH_FULL => QVariant::from(&QString::from(&path_full_display(path))),
        _ => QVariant::default(),
    }
}

/// Program label roles of a connection row.
pub(super) fn role_for_row(row: &ConnectionRow, role: i32) -> QVariant {
    let path = process_path_text(row);
    answer(&program_label_for(Some(path), &row.process), path, role)
}

/// Program label roles of a program header, from its group key (a path when it
/// starts with `/`, else a bare process name) and its label (the raw name).
pub(super) fn role_for_program_header(key: &str, label: &str, role: i32) -> QVariant {
    let key = key.trim_start();
    let path = key.starts_with('/').then_some(key);
    answer(&program_label_for(path, label), path.unwrap_or(""), role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_names::NameSource;

    const VERSIONED: &str = "/home/u/.local/share/claude/versions/2.1.308";
    const CRAFTED: &str = "/home/u/x/claude/versions/9.9";

    fn text(v: QVariant) -> String {
        v.value::<QString>().map(|s| s.to_string()).unwrap()
    }
    fn flag(v: QVariant) -> bool {
        v.value::<bool>().unwrap()
    }

    #[test]
    fn desktop_name_is_unverified_with_a_tail() {
        let n = label_with(Some("/opt/acme/bin/acme"), "acme", Some("Acme Chat"));
        assert_eq!(
            (n.label.as_str(), n.source),
            ("Acme Chat", NameSource::DesktopFile)
        );
        assert!(n.needs_marker());
    }

    #[test]
    fn heuristic_label_is_unverified_and_exposes_the_tail() {
        let n = program_label_for(Some(VERSIONED), "2.1.308");
        assert_eq!(n.label, "claude 2.1.308");
        assert!(n.needs_marker());
        assert_eq!(path_tail(VERSIONED), "…/versions/2.1.308");
    }

    #[test]
    fn crafted_path_is_unverified_and_its_real_path_is_exposed() {
        let n = program_label_for(Some(CRAFTED), "9.9");
        assert_eq!(n.label, "claude 9.9");
        assert!(n.needs_marker());
        assert_eq!(text(answer_tail(CRAFTED, "9.9")), "…/versions/9.9");
    }

    fn answer_tail(path: &str, raw: &str) -> QVariant {
        answer(
            &program_label_for(Some(path), raw),
            path,
            ROLE_PROGRAM_PATH_TAIL,
        )
    }

    #[test]
    fn plain_path_is_raw_unmarked_and_has_no_tail() {
        let n = program_label_for(Some("/usr/bin/curl"), "curl");
        assert_eq!((n.label.as_str(), n.source), ("curl", NameSource::Raw));
        assert!(!n.needs_marker());
        assert_eq!(text(answer_tail("/usr/bin/curl", "curl")), "");
        assert!(!flag(answer(
            &n,
            "/usr/bin/curl",
            ROLE_PROGRAM_LABEL_UNVERIFIED
        )));
    }

    #[test]
    fn no_or_empty_path_is_raw() {
        for path in [None, Some(""), Some("  ")] {
            let n = program_label_for(path, "curl");
            assert_eq!((n.label.as_str(), n.source), ("curl", NameSource::Raw));
            assert!(!n.needs_marker());
        }
    }

    #[test]
    fn altered_raw_name_is_marked_with_its_tail() {
        let path = "/usr/bin/evil\u{202E}gpj";
        assert!(flag(answer(
            &program_label_for(Some(path), "evil\u{202E}gpj"),
            path,
            ROLE_PROGRAM_LABEL_UNVERIFIED
        )));
        assert_eq!(text(answer_tail(path, "evil\u{202E}gpj")), "…/bin/evilgpj");
    }

    #[test]
    fn program_header_uses_the_key_path_not_the_label() {
        // A path key: the heuristic applies and the key is the real path.
        assert_eq!(
            text(role_for_program_header(
                VERSIONED,
                "2.1.308",
                ROLE_PROGRAM_LABEL
            )),
            "claude 2.1.308"
        );
        assert!(flag(role_for_program_header(
            VERSIONED,
            "2.1.308",
            ROLE_PROGRAM_LABEL_UNVERIFIED
        )));
        assert_eq!(
            text(role_for_program_header(
                VERSIONED,
                "2.1.308",
                ROLE_PROGRAM_PATH_TAIL
            )),
            "…/versions/2.1.308"
        );
        // A bare-name key (no path known): raw, unmarked, no tail, even when
        // the NAME looks like a path-shaped lie.
        assert_eq!(
            text(role_for_program_header("curl", "curl", ROLE_PROGRAM_LABEL)),
            "curl"
        );
        assert!(!flag(role_for_program_header(
            "curl",
            "curl",
            ROLE_PROGRAM_LABEL_UNVERIFIED
        )));
        assert_eq!(
            text(role_for_program_header(
                "curl",
                "curl",
                ROLE_PROGRAM_PATH_TAIL
            )),
            ""
        );
        assert!(!flag(role_for_program_header(
            "a/versions/1.2",
            "a/versions/1.2",
            ROLE_PROGRAM_LABEL_UNVERIFIED
        )));
    }

    #[test]
    fn path_tail_cases() {
        assert_eq!(path_tail(""), "");
        assert_eq!(path_tail("   "), "");
        assert_eq!(path_tail("/curl"), "/curl");
        assert_eq!(path_tail("/usr/curl"), "/usr/curl");
        assert_eq!(path_tail("/usr/bin/curl"), "…/bin/curl");
        assert_eq!(path_tail("/a/b/c/d/e"), "…/d/e");
        assert_eq!(path_tail("/a/b/c/"), "…/b/c");
        assert_eq!(path_tail("/a//b///c"), "…/b/c");
        assert_eq!(path_tail("/x/ev\u{202E}il\n/b\u{0}in"), "…/evil/bin");
        assert_eq!(path_tail("/x/y\u{7}/\u{202E}"), "…/y/<unnamed>");
    }

    fn no_raw_unsafe_char(s: &str) -> bool {
        !s.chars().any(|c| c.is_control() || is_stripped(c))
    }

    #[test]
    fn full_path_shows_unsafe_characters_as_visible_escapes() {
        let shown = path_full_display("/a/b\u{202E}gpj.exe\n/x\0y\t\u{2028}z w");
        assert_eq!(shown, "/a/b\\u{202e}gpj.exe\\n/x\\u{0}y\\t\\u{2028}z w");
        assert!(no_raw_unsafe_char(&shown));
        assert_eq!(path_full_display(""), "");
        assert_eq!(path_full_display("/usr/bin/curl"), "/usr/bin/curl");
    }

    #[test]
    fn full_path_is_capped_with_an_ellipsis() {
        let long = format!("/{}", "a".repeat(10_000));
        let shown = path_full_display(&long);
        assert_eq!(shown.chars().count(), 300);
        assert!(shown.ends_with('…'));
        let nasty = "\u{202E}".repeat(10_000);
        let shown = path_full_display(&nasty);
        assert!(shown.chars().count() <= 300 && no_raw_unsafe_char(&shown));
        assert_eq!(path_full_display(&"b".repeat(300)).chars().count(), 300);
    }

    #[test]
    fn full_path_role_is_escaped_for_rows_and_path_headers_only() {
        let evil = "/home/u/x\u{202E}/claude/versions/9.9";
        let want = "/home/u/x\\u{202e}/claude/versions/9.9";
        assert_eq!(
            text(role_for_program_header(evil, "9.9", ROLE_PROGRAM_PATH_FULL)),
            want
        );
        assert_eq!(
            text(role_for_program_header(
                "curl",
                "curl",
                ROLE_PROGRAM_PATH_FULL
            )),
            ""
        );
    }

    #[test]
    fn header_key_with_leading_whitespace_is_still_a_path() {
        let key = format!("  {VERSIONED}");
        assert_eq!(
            text(role_for_program_header(&key, "2.1.308", ROLE_PROGRAM_LABEL)),
            "claude 2.1.308"
        );
        assert_eq!(
            text(role_for_program_header(
                &key,
                "2.1.308",
                ROLE_PROGRAM_PATH_FULL
            )),
            VERSIONED
        );
    }
}
