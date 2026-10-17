use super::*;

// -- sanitize_for_display / MEDIUM-1 (issue #14 security review round 2) --

#[test]
fn sanitize_for_display_neutralizes_markup_metacharacters() {
    let out = sanitize_for_display("<b>evil.example</b>", 64);
    assert!(!out.contains('<'));
    assert!(!out.contains('>'));
    assert_eq!(out, "&lt;b&gt;evil.example&lt;/b&gt;");
}

#[test]
fn sanitize_for_display_strips_ansi_escape_sequences() {
    let out = sanitize_for_display("evil\x1b[31mred\x1b[0m.example", 64);
    assert!(!out.contains('\x1b'), "ESC byte must not survive: {out:?}");
    assert_eq!(out, "evil[31mred[0m.example");
}

#[test]
fn sanitize_for_display_strips_newlines_and_control_chars() {
    let out = sanitize_for_display("evil.example\nInjected: fake line\r\n", 64);
    assert!(!out.contains('\n'));
    assert!(!out.contains('\r'));
    assert_eq!(out, "evil.exampleInjected: fake line");
}

#[test]
fn sanitize_for_display_caps_length_by_characters_not_bytes() {
    let long_host = "a".repeat(500);
    let out = sanitize_for_display(&long_host, 64);
    assert!(
        out.chars().count() <= 65,
        "expected <= 64 chars plus the truncation marker, got {}",
        out.chars().count()
    );
    assert!(out.ends_with('…'));
}

#[test]
fn sanitize_for_display_leaves_ordinary_hostnames_untouched() {
    assert_eq!(sanitize_for_display("github.com", 64), "github.com");
}

#[test]
fn sanitize_for_display_strips_rtl_override() {
    // U+202E RIGHT-TO-LEFT OVERRIDE can make attacker-controlled bytes
    // *render* as a different, more trustworthy-looking string than
    // what they actually are — `char::is_control()` alone doesn't
    // catch it (it's category Cf, not Cc).
    let out = sanitize_for_display("evil\u{202E}moc.elpmaxe", 64);
    assert!(
        !out.contains('\u{202E}'),
        "RTL override must not survive: {out:?}"
    );
    assert_eq!(out, "evilmoc.elpmaxe");
}

#[test]
fn sanitize_for_display_strips_zero_width_characters() {
    // U+200B ZERO WIDTH SPACE can hide characters entirely or defeat
    // naive substring-based review; also Cf, not Cc.
    let out = sanitize_for_display("ev\u{200B}il.example", 64);
    assert!(
        !out.contains('\u{200B}'),
        "zero-width space must not survive: {out:?}"
    );
    assert_eq!(out, "evil.example");
}
