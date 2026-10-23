use super::*;

fn friendly(path: &str, raw: &str) -> ProgramName {
    friendly_name(Some(path), raw, None)
}

#[test]
fn desktop_name_wins_and_needs_a_marker() {
    let n = friendly_name(Some("/usr/bin/x"), "x", Some("Pretty"));
    assert_eq!(
        (n.label.as_str(), n.source),
        ("Pretty", NameSource::DesktopFile)
    );
    assert!(n.needs_marker());
}

#[test]
fn versions_directory_gives_app_and_version() {
    let n = friendly("/home/u/.local/share/claude/versions/2.1.308", "2.1.308");
    assert_eq!(
        (n.label.as_str(), n.source),
        ("claude 2.1.308", NameSource::Heuristic)
    );
}

#[test]
fn a_crafted_versions_path_is_a_marked_heuristic() {
    let n = friendly("/home/u/x/claude/versions/9.9", "9.9");
    assert_eq!(
        (n.label.as_str(), n.source),
        ("claude 9.9", NameSource::Heuristic)
    );
    assert!(n.needs_marker());
}

#[test]
fn semver_shapes() {
    assert_eq!(
        friendly("/a/app/versions/1.2.3.4-rc.1", "x").label,
        "app 1.2.3.4-rc.1"
    );
    assert_eq!(friendly("/a/app/versions/1.2+b5", "x").label, "app 1.2+b5");
    for bad in [
        "1",
        "1.2.3.4.5",
        "1..2",
        "v1.2",
        "1.2-",
        "1.2-a_b",
        "1.2-a+b",
        "1.2.x",
        "",
    ] {
        let n = friendly(&format!("/a/app/versions/{bad}"), "raw");
        assert_eq!(n.source, NameSource::Raw, "{bad}");
    }
}

#[test]
fn denylisted_or_malformed_app_component_is_not_a_hit() {
    for app in [
        "bin", "sbin", "lib", "lib64", "usr", "opt", "local", "share", ".", "..", "a b",
    ] {
        let n = friendly(&format!("/x/{app}/versions/1.2"), "1.2");
        assert_eq!(n.source, NameSource::Raw, "{app}");
    }
    let n = friendly("/usr/bin/versions/1.2", "1.2");
    assert_eq!((n.label.as_str(), n.source), ("1.2", NameSource::Raw));
    let long = "a".repeat(41);
    assert_eq!(
        friendly(&format!("/x/{long}/versions/1.2"), "r").source,
        NameSource::Raw
    );
    let ok = "a".repeat(40);
    assert_eq!(
        friendly(&format!("/x/{ok}/versions/1.2"), "r").source,
        NameSource::Heuristic
    );
}

#[test]
fn versions_needs_a_parent_component() {
    assert_eq!(friendly("versions/1.2", "r").source, NameSource::Raw);
    assert_eq!(friendly("/versions/1.2", "r").source, NameSource::Raw);
}

#[test]
fn cpu_feature_suffix_is_stripped() {
    for (path, want) in [
        ("/opt/x/pasta.avx2", "pasta"),
        ("/opt/x/pasta.AVX", "pasta"),
        ("/opt/x/pasta.avx512", "pasta"),
        ("/opt/x/pasta.sse2", "pasta"),
        ("/opt/x/pasta.sse4", "pasta"),
    ] {
        let n = friendly(path, "r");
        assert_eq!(
            (n.label.as_str(), n.source),
            (want, NameSource::Heuristic),
            "{path}"
        );
    }
}

#[test]
fn bare_or_unrelated_suffixes_are_raw() {
    assert_eq!(friendly("/opt/x/.avx2", "raw").source, NameSource::Raw);
    assert_eq!(friendly("/opt/x/pasta.avx3", "raw").source, NameSource::Raw);
    assert_eq!(friendly("/opt/x/pasta", "pasta").source, NameSource::Raw);
}

#[test]
fn control_characters_in_the_path_disable_heuristics() {
    assert_eq!(
        friendly("/x/app\n/versions/1.2", "raw").source,
        NameSource::Raw
    );
    assert_eq!(
        friendly("/opt/pasta\u{7}.avx2", "raw").source,
        NameSource::Raw
    );
}

#[test]
fn no_path_is_raw_and_unknown_stays() {
    let n = friendly_name(None, "<unknown>", None);
    assert_eq!((n.label.as_str(), n.source), ("<unknown>", NameSource::Raw));
    assert!(!n.needs_marker());
    assert!(!n.altered);
    let n = friendly_name(Some(""), "ssh", None);
    assert_eq!((n.label.as_str(), n.source), ("ssh", NameSource::Raw));
}

#[test]
fn raw_fallback_is_sanitized() {
    assert_eq!(friendly_name(None, "ev\u{202E}il\n", None).label, "evil");
}

#[test]
fn marker_is_needed_for_everything_but_raw() {
    assert!(friendly_name(None, "x", Some("Pretty")).needs_marker());
    assert!(friendly("/a/app/versions/1.2", "1.2").needs_marker());
    assert!(!friendly_name(None, "x", None).needs_marker());
}

#[test]
fn sanitize_strips_overrides_and_controls() {
    assert_eq!(sanitize_display("a\u{202E}b\nc\td\u{0}e"), "ab c de");
    for c in [
        '\u{61C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202D}', '\u{2066}', '\u{2069}',
    ] {
        assert_eq!(sanitize_display(&format!("a{c}b")), "ab", "{c:?}");
    }
    assert_eq!(sanitize_display("  a  b  "), "a b");
}

#[test]
fn sanitize_caps_at_80_chars_on_a_char_boundary() {
    assert_eq!(sanitize_display(&"x".repeat(200)).chars().count(), 80);
    let wide = "é".repeat(100);
    let out = sanitize_display(&wide);
    assert_eq!(out, "é".repeat(80));
    let emoji = "🦀".repeat(81);
    assert_eq!(sanitize_display(&emoji).chars().count(), 80);
}

#[test]
fn sanitize_falls_back_when_nothing_is_left() {
    for s in ["", "   ", "\n\t", "\u{202E}\u{200F}"] {
        assert_eq!(sanitize_display(s), "<unnamed>", "{s:?}");
    }
}

#[test]
fn line_and_paragraph_separators_are_stripped() {
    assert_eq!(sanitize_display("a\u{2028}b\u{2029}c"), "a b c");
}

#[test]
fn format_characters_are_stripped() {
    assert_eq!(sanitize_display("Fire\u{200B}fox"), "Firefox");
    for c in [
        '\u{AD}',
        '\u{600}',
        '\u{6DD}',
        '\u{70F}',
        '\u{180E}',
        '\u{2060}',
        '\u{206F}',
        '\u{FEFF}',
        '\u{FFF9}',
        '\u{34F}',
        '\u{FE0F}',
        '\u{E0100}',
    ] {
        assert_eq!(sanitize_display(&format!("a{c}b")), "ab", "{c:?}");
    }
}

#[test]
fn tag_characters_are_stripped() {
    assert_eq!(sanitize_display("a\u{E0001}\u{E0041}\u{E007F}b"), "ab");
}

#[test]
fn fillers_alone_leave_nothing() {
    assert_eq!(sanitize_display("\u{3164}"), "<unnamed>");
    for c in ['\u{115F}', '\u{1160}', '\u{FFA0}'] {
        assert_eq!(sanitize_display(&c.to_string()), "<unnamed>", "{c:?}");
    }
}

#[test]
fn combining_marks_are_capped_at_two_per_base() {
    let zalgo = format!("a{}b", "\u{301}".repeat(500));
    assert_eq!(sanitize_display(&zalgo), "a\u{301}\u{301}b");
    let mixed = "e\u{300}\u{1AB0}\u{1DC0}\u{20D0}\u{FE20}x";
    assert_eq!(sanitize_display(mixed), "e\u{300}\u{1AB0}x");
    // A new base resets the count.
    assert_eq!(
        sanitize_display("a\u{301}\u{301}b\u{301}\u{301}"),
        "a\u{301}\u{301}b\u{301}\u{301}"
    );
}

#[test]
fn non_ascii_spaces_become_one_plain_space() {
    assert_eq!(sanitize_display("a\u{3000}b"), "a b");
    assert_eq!(sanitize_display("a\u{A0}\u{2003} \u{3000}b"), "a b");
}

#[test]
fn trailing_space_after_the_cap_is_trimmed() {
    let s = format!("{} tail", "x".repeat(79));
    assert_eq!(sanitize_display(&s), "x".repeat(79));
}

#[test]
fn a_raw_name_that_was_altered_gets_a_marker() {
    let n = friendly_name(None, "ev\u{202E}il", None);
    assert_eq!(
        (n.label.as_str(), n.source, n.altered),
        ("evil", NameSource::Raw, true)
    );
    assert!(n.needs_marker());
    let n = friendly_name(None, &"r".repeat(200), None);
    assert!(n.altered && n.needs_marker());
    let n = friendly_name(None, "\u{200B}\u{3164}", None);
    assert_eq!((n.label.as_str(), n.altered), ("<unnamed>", true));
    assert!(n.needs_marker());
    let n = friendly_name(Some("/usr/bin/ssh"), "ssh", None);
    assert!(!n.altered && !n.needs_marker());
}

#[test]
fn desktop_name_beats_a_matching_heuristic() {
    let n = friendly_name(Some("/a/app/versions/1.2"), "1.2", Some("Pretty"));
    assert_eq!(
        (n.label.as_str(), n.source),
        ("Pretty", NameSource::DesktopFile)
    );
}

#[test]
fn an_empty_desktop_name_falls_through() {
    // A name that sanitizes to nothing is no name: heuristic, then raw.
    for empty in ["", "\u{200B}", "   "] {
        let n = friendly_name(Some("/a/app/versions/1.2"), "1.2", Some(empty));
        assert_eq!(
            (n.label.as_str(), n.source),
            ("app 1.2", NameSource::Heuristic),
            "{empty:?}"
        );
        let n = friendly_name(None, "ssh", Some(empty));
        assert_eq!(
            (n.label.as_str(), n.source),
            ("ssh", NameSource::Raw),
            "{empty:?}"
        );
    }
}

#[test]
fn whitespace_controls_separate_words_instead_of_fusing_them() {
    assert_eq!(sanitize_display("Foo\nBar"), "Foo Bar");
    assert_eq!(sanitize_display("a\t\r\u{B}\u{C}\u{85}b"), "a b");
    assert_eq!(sanitize_display("a\u{0}b"), "ab");
}

#[test]
fn only_whitespace_controls_clean_to_unnamed() {
    assert_eq!(
        sanitize_display("\t\n\r\u{B}\u{C}\u{85}\u{2028}\u{2029}"),
        "<unnamed>"
    );
}

#[test]
fn blank_looking_characters_are_stripped() {
    for c in [
        '\u{2800}', '\u{17B4}', '\u{17B5}', '\u{890}', '\u{891}', '\u{8E2}',
    ] {
        assert_eq!(sanitize_display(&format!("a{c}b")), "ab", "{c:?}");
        assert_eq!(sanitize_display(&c.to_string()), "<unnamed>", "{c:?}");
    }
}

#[test]
fn range_endpoints_are_stripped() {
    for c in [
        '\u{605}',
        '\u{FFFB}',
        '\u{E007F}',
        '\u{600}',
        '\u{FFF9}',
        '\u{E0000}',
    ] {
        assert_eq!(sanitize_display(&format!("a{c}b")), "ab", "{c:?}");
    }
}
