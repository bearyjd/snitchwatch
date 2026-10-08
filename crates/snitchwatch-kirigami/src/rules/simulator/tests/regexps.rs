//! Regular-expression tests for `simulate`: lowercasing, anchoring and
//! patterns the `regex` crate can't compile. How RE2 syntax is read lives in
//! `regexp_corpus.rs`. Builders live in the parent module.

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
    // A hostname-shaped bounded repeat compiles and bounds the length.
    let pattern = op("regexp", "dest.host", r"^[a-z0-9]{1,253}$");
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
