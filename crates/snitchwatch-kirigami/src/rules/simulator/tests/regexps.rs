//! Regular-expression tests for `simulate`: lowercasing, anchoring, RE2
//! syntax the `regex` crate reads differently, and patterns it can't compile.
//! Builders live in the parent module.

use super::*;

#[test]
fn regexp_lowercases_pattern_and_subject_unless_sensitive() {
    // Pattern uppercase, subject lowercase.
    assert!(matched(
        op("regexp", "process.path", "^/USR/BIN/CURL$"),
        &base()
    ));
    // Pattern lowercase, subject uppercase.
    let loud = base_with(|i| i.process_path = Some("/USR/BIN/CURL".to_string()));
    assert!(matched(
        op("regexp", "process.path", "^/usr/bin/curl$"),
        &loud
    ));
    // Sensitive: neither is touched.
    assert!(!matched(
        op_sensitive("regexp", "process.path", "^/USR/BIN/CURL$"),
        &base()
    ));
    assert!(!matched(
        op_sensitive("regexp", "process.path", "^/usr/bin/curl$"),
        &loud
    ));
    assert!(matched(
        op_sensitive("regexp", "process.path", "^/USR/BIN/CURL$"),
        &loud
    ));
}

#[test]
fn regexp_lowercasing_also_rewrites_class_escapes_like_the_daemon() {
    // `Operator.Compile` lowercases the whole pattern, so `\D` becomes `\d`.
    let digits = base_with(|i| i.process_path = Some("123".to_string()));
    let letters = base_with(|i| i.process_path = Some("abc".to_string()));
    let pattern = op("regexp", "process.path", r"^\D+$");
    assert!(matched(pattern.clone(), &digits));
    assert!(!matched(pattern, &letters));
    // Sensitive keeps `\D` as written.
    let pattern = op_sensitive("regexp", "process.path", r"^\D+$");
    assert!(!matched(pattern.clone(), &digits));
    assert!(matched(pattern, &letters));
}

#[test]
fn regexp_is_unanchored() {
    assert!(matched(op("regexp", "process.path", "curl"), &base()));
    assert!(!matched(op("regexp", "process.path", "^curl"), &base()));
}

#[test]
fn a_pattern_the_simulators_engine_cannot_compile_is_unsupported_never_a_miss() {
    // Every cached enabled rule already compiled under RE2 (the loader skips
    // rules that fail), so a failure here is a syntax difference between
    // engines, not a rule that cannot match. `\Q..\E` is RE2-only.
    for operator in [
        op_sensitive("regexp", "process.path", r"\Q/usr/bin\E/curl"),
        op("regexp", "process.path", "(unclosed"),
    ] {
        let result = run(operator.clone(), &base());
        assert_eq!(result.matched_rule, None, "{operator}");
        assert_eq!(result.unsupported_operands.len(), 1, "{operator}");
        let u = &result.unsupported_operands[0];
        assert_eq!(u.operand, "process.path");
        assert!(u.reason.contains("RE2"), "{}", u.reason);
    }
}

#[test]
fn a_big_bounded_repeat_still_compiles() {
    // Hostname-shaped patterns with `{1,253}` blow past the regex crate's
    // default 10 MiB program limit when `\w` is the Unicode class.
    let pattern = op("regexp", "dest.host", r"^[\w.-]{1,253}$");
    let host = |n: usize| base_with(|i| i.dest_host = "a".repeat(n));
    assert!(matched(pattern.clone(), &host(253)));
    assert!(!matched(pattern.clone(), &host(254)));
    assert!(!matched(pattern, &host(0)));
}

#[test]
fn a_unicode_class_repeat_needs_more_than_the_default_program_size() {
    // `\pL` stays a Unicode class under RE2; repeated up to RE2's limit of
    // 1000 it is far past the regex crate's 10 MiB default program size.
    let pattern = op_sensitive("regexp", "process.path", r"^\pL{1,1000}$");
    let at = |p: &str| base_with(|i| i.process_path = Some(p.to_string()));
    let result = run(pattern.clone(), &at("éa"));
    assert_eq!(result.matched_rule.as_deref(), Some("r"), "{result:?}");
    assert!(!matched(pattern, &at("é1")));
}

#[test]
fn re2_reads_escaped_punctuation_and_an_unparsed_repeat_as_literals() {
    // RE2: a backslash before punctuation is that character (`\<` is `<`, not
    // a word-boundary assertion), and `{,2}` isn't a repeat, so it is text.
    let cases: &[(&str, &str, bool)] = &[
        ("a<b", r"a\<b", true),
        ("a>b", r"a\>b", true),
        ("ab", r"a\<b", false),
        ("x{,2}", r"^x{,2}$", true),
        ("xx", r"^x{,2}$", false),
        ("x", r"^x{,2}$", false),
        ("xxx", r"^x{2,3}$", true),
    ];
    for (subject, pattern, expected) in cases {
        let input = base_with(|i| i.process_path = Some(subject.to_string()));
        assert_eq!(
            matched(op_sensitive("regexp", "process.path", pattern), &input),
            *expected,
            "{pattern} vs {subject:?}"
        );
    }
}

#[test]
fn re2_perl_classes_are_ascii_only() {
    // RE2's `\w \d \s \b` are ASCII; Rust's are Unicode (and its `\s`
    // includes `\v` and NBSP). (subject, pattern, expected)
    let cases: &[(&str, &str, bool)] = &[
        ("/home/josé/bin", r"^/home/\w+/bin$", false),
        ("/home/jose/bin", r"^/home/\w+/bin$", true),
        ("\u{663}\u{664}", r"^\d+$", false), // Arabic-Indic digits
        ("34", r"^\d+$", true),
        ("\u{a0}", r"^\S+$", true), // NBSP is not RE2 space
        ("\u{a0}", r"^\s+$", false),
        ("\u{b}", r"^\s$", false), // \v is not RE2 space
        (" \t\n\r\u{c}", r"^\s+$", true),
        ("é", r"^\W$", true),
        ("é", r"^\D$", true),
        ("é", r"^[^\w]$", true),
        ("é", r"^[\W]$", true),
        ("é", r"^[\w]$", false),
        ("é", r"\bé\b", false), // ASCII word boundary: é is not a word char
        ("aé", r"a\b", true),
        ("a_1", r"^[\w]+$", true),
        ("a.b-c", r"^[\w.-]+$", true),
        ("a b", r"^[^\s]+$", false),
        ("é", r"^[^\w]+$", true),
    ];
    for (subject, pattern, expected) in cases {
        let input = base_with(|i| i.process_path = Some(subject.to_string()));
        // Sensitive, so the pattern isn't lowercased and `\W` stays `\W`.
        assert_eq!(
            matched(op_sensitive("regexp", "process.path", pattern), &input),
            *expected,
            "{pattern} vs {subject:?}"
        );
    }
}

#[test]
fn a_bracket_inside_a_class_is_a_literal_like_in_go() {
    // Go: `[[a]]` is the class {'[', 'a'} followed by a literal `]`; the
    // regex crate would read a nested class.
    let at = |p: &str| base_with(|i| i.process_path = Some(p.to_string()));
    let pattern = op_sensitive("regexp", "process.path", r"^[[a]]$");
    assert!(matched(pattern.clone(), &at("[]")));
    assert!(matched(pattern.clone(), &at("a]")));
    assert!(!matched(pattern, &at("a")));
    // A POSIX class still works, and a leading `]` is a literal.
    assert!(matched(
        op_sensitive("regexp", "process.path", r"^[[:alpha:]]+$"),
        &at("abc")
    ));
    assert!(matched(
        op_sensitive("regexp", "process.path", r"^[]a]+$"),
        &at("]a]")
    ));
}

#[test]
fn escaped_backslashes_are_not_read_as_classes() {
    // `\\w` is a literal backslash then `w`.
    let at = |p: &str| base_with(|i| i.process_path = Some(p.to_string()));
    let pattern = op_sensitive("regexp", "process.path", r"^\\w$");
    assert!(matched(pattern.clone(), &at("\\w")));
    assert!(!matched(pattern, &at("a")));
}

/// Sensitive-pattern table: (subject, pattern, expected) per Go's RE2.
fn assert_re2(cases: &[(&str, &str, bool)]) {
    for (subject, pattern, expected) in cases {
        let input = base_with(|i| i.process_path = Some(subject.to_string()));
        let result = run(op_sensitive("regexp", "process.path", pattern), &input);
        assert!(
            result.unsupported_operands.is_empty(),
            "{pattern} was reported as not simulated: {:?}",
            result.unsupported_operands
        );
        assert_eq!(
            result.matched_rule.is_some(),
            *expected,
            "{pattern} vs {subject:?}"
        );
    }
}

#[test]
fn class_set_operators_are_plain_characters_like_in_go() {
    // The regex crate reads `&&`, `--` and `~~` inside a class as set
    // operations; Go reads them as characters and ranges.
    assert_re2(&[
        ("&", "^[a&&b]$", true),
        ("a", "^[a&&b]$", true),
        ("b", "^[a&&b]$", true),
        ("c", "^[a&&b]$", false),
        // `+--` is the range '+' to '-': '+', ',' and '-'.
        (",", "^[+--]$", true),
        ("+", "^[+--]$", true),
        ("-", "^[+--]$", true),
        (".", "^[+--]$", false),
        ("~", "^[~~]$", true),
        ("a", "^[a~~b]$", true),
        ("~", "^[a~~b]$", true),
        // A range, then a literal '-'.
        ("-", "^[a-z--]$", true),
        ("m", "^[a-z--]$", true),
        ("A", "^[a-z--]$", false),
        ("-", "^[a-c-e]$", true),
        ("e", "^[a-c-e]$", true),
        ("d", "^[a-c-e]$", false),
        // After a Perl class a '-' is still a literal.
        ("-", r"^[\w-.]$", true),
        (".", r"^[\w-.]$", true),
        ("a", r"^[\w-.]$", true),
        ("!", r"^[\w-.]$", false),
    ]);
}

#[test]
fn a_repeat_with_a_leading_zero_is_text_like_in_go() {
    // Go's parseInt refuses leading zeros, so `a{01}` is the text "a{01}".
    assert_re2(&[
        ("a{01}", "^a{01}$", true),
        ("a", "^a{01}$", false),
        ("a{0,01}", "^a{0,01}$", true),
        ("a{01,}", "^a{01,}$", true),
        ("a{00}", "^a{00}$", true),
        // A lone zero is a fine bound.
        ("b", "^ba{0}$", true),
        ("baa", "^ba{0,2}$", true),
        ("a", "^a{1}$", true),
    ]);
}

#[test]
fn an_unclosed_posix_class_opener_is_a_literal_bracket() {
    // No `:]` follows, so `[:` is just '[' and ':' in the class (Go's
    // parseNamedClass finds nothing); the class closes at the first `]`.
    assert_re2(&[
        ("x]]", "^[[:x]]]$", true),
        ("[]]", "^[[:x]]]$", true),
        (":]]", "^[[:x]]]$", true),
        ("x]", "^[[:x]]]$", false),
        // A real POSIX class still works.
        ("abc", "^[[:alpha:]]+$", true),
        ("ab1", "^[[:alpha:]]+$", false),
        ("a1", "^[[:^alpha:]x]+$", false),
    ]);
}

#[test]
fn braced_unicode_and_hex_escapes_are_not_mistaken_for_repeats() {
    assert_re2(&[
        ("αβγ", r"^\p{Greek}+$", true),
        ("abc", r"^\p{Greek}+$", false),
        ("a", r"^\P{Greek}$", true),
        ("α", r"^\P{Greek}$", false),
        ("αx", r"^[\p{Greek}x]+$", true),
        ("☺", r"^\x{263a}$", true),
        ("A", r"^\x{41}$", true),
        ("A", r"^\x41$", true),
        ("a{1}", r"^a\{1\}$", true),
    ]);
}

#[test]
fn a_pattern_too_big_for_the_simulator_is_told_apart_from_a_syntax_difference() {
    // Valid in Go, but the compiled program passes the simulator's cap.
    let huge = op_sensitive(
        "regexp",
        "process.path",
        r"^\pL{1,1000}\pL{1,1000}\pL{1,1000}\pL{1,1000}$",
    );
    let result = run(huge, &base());
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unsupported_operands.len(), 1);
    let reason = &result.unsupported_operands[0].reason;
    assert!(reason.contains("too large"), "{reason}");
    assert!(!reason.contains("syntax differs"), "{reason}");

    let syntax = run(op_sensitive("regexp", "process.path", r"\Qa.b\E"), &base());
    let reason = &syntax.unsupported_operands[0].reason;
    assert!(reason.contains("syntax differs"), "{reason}");
    assert!(!reason.contains("too large"), "{reason}");
}
