//! A corpus of regular expressions against what Go's RE2 answers.
//!
//! The simulator rewrites RE2 patterns for the `regex` crate and models
//! character classes with a strict allowlist. What this table pins is the
//! outcome, not the rewritten text: **a pattern is either answered the way Go
//! answers it or reported as not simulated — never the other definite
//! answer.** Each row is `(pattern, subject, Go's answer, whether the
//! simulator models the pattern)`; the Go answers are worked out from Go's
//! `regexp/syntax` grammar by hand, and patterns are matched sensitively so
//! `Compile` doesn't lowercase them.
//!
//! Every adversarial pattern from the review rounds is here, and so are the
//! common forms that stay simulated.

use super::*;

/// What the simulator said about one pattern and subject.
#[derive(Debug, PartialEq, Eq)]
enum Said {
    Match,
    NoMatch,
    NotSimulated,
}

/// `sensitive` is the operator's flag: when false the daemon lowercases both
/// the pattern source and the subject before matching.
fn said_with(pattern: &str, subject: &str, sensitive: bool) -> Said {
    let input = base_with(|i| i.process_path = Some(subject.to_string()));
    let operator = if sensitive {
        op_sensitive("regexp", "process.path", pattern)
    } else {
        op("regexp", "process.path", pattern)
    };
    let result = run(operator, &input);
    if !result.unsupported_operands.is_empty() {
        Said::NotSimulated
    } else if result.matched_rule.is_some() {
        Said::Match
    } else {
        Said::NoMatch
    }
}

fn check(corpus: &[(&str, &str, bool, bool)]) {
    check_with(true, corpus);
}

fn check_with(sensitive: bool, corpus: &[(&str, &str, bool, bool)]) {
    for &(pattern, subject, go, modelled) in corpus {
        let got = said_with(pattern, subject, sensitive);
        // The invariant: never the other definite answer.
        match got {
            Said::Match => assert!(
                go,
                "{pattern} vs {subject:?}: simulator says match, Go says no"
            ),
            Said::NoMatch => {
                assert!(
                    !go,
                    "{pattern} vs {subject:?}: simulator says no match, Go says match"
                )
            }
            Said::NotSimulated => {}
        }
        // Which patterns are modelled at all.
        assert_eq!(
            got != Said::NotSimulated,
            modelled,
            "{pattern} vs {subject:?}: modelled = {modelled}, simulator said {got:?}"
        );
    }
}

/// Outside a character class: ASCII Perl classes, literals RE2 reads
/// differently from the crate, braces, escapes.
#[test]
fn syntax_outside_classes_answers_like_go() {
    check(&[
        // RE2's \w \d \s \b are ASCII only.
        ("^/home/\\w+/bin$", "/home/josé/bin", false, true),
        ("^/home/\\w+/bin$", "/home/jose/bin", true, true),
        ("^\\d+$", "\u{663}\u{664}", false, true), // Arabic-Indic digits
        ("^\\d+$", "34", true, true),
        ("^\\S+$", "\u{a0}", true, true), // NBSP is not RE2 space
        ("^\\s+$", "\u{a0}", false, true),
        ("^\\s$", "\u{b}", false, true), // \v is not RE2 space
        ("^\\s+$", " \t\n\r\u{c}", true, true),
        ("^\\W$", "é", true, true),
        ("^\\D$", "é", true, true),
        ("\\bé\\b", "é", false, true),
        ("a\\b", "aé", true, true),
        // A backslash before punctuation is that character.
        ("a\\<b", "a<b", true, true),
        ("a\\>b", "a>b", true, true),
        ("a\\<b", "ab", false, true),
        ("^\\[a-z\\]$", "[a-z]", true, true),
        ("^\\\\w$", "\\w", true, true),
        ("^\\\\w$", "a", false, true),
        // A `{` that isn't a repeat is text.
        ("^x{,2}$", "x{,2}", true, true),
        ("^x{,2}$", "xx", false, true),
        ("^x{,2}$", "x", false, true),
        ("^a{1}$", "a", true, true),
        ("^ba{0}$", "b", true, true),
        ("^ba{0,2}$", "baa", true, true),
        ("^x{2,3}$", "xxx", true, true),
        ("^a\\{1\\}$", "a{1}", true, true),
        // ...and so is one whose bound has a leading zero.
        ("a{01}", "a", false, true),
        ("a{01}", "a{01}", true, true),
        ("^a{0,01}$", "a{0,01}", true, true),
        ("^a{01,}$", "a{01,}", true, true),
        ("^a{00}$", "a{00}", true, true),
        // Braced escapes keep their braces.
        ("^\\p{Greek}+$", "αβγ", true, true),
        ("^\\p{Greek}+$", "abc", false, true),
        ("^\\P{Greek}$", "a", true, true),
        ("^\\P{Greek}$", "α", false, true),
        ("^\\x{263a}$", "☺", true, true),
        ("^\\x{1F}$", "\u{1f}", true, true),
        ("^\\x41$", "A", true, true),
        ("^[0-9]{1,3}\\.[0-9]{1,3}$", "1.2", true, true),
    ]);
}

/// The class forms the simulator models, after an optional leading `^`: ASCII
/// letters and digits as literals, ranges between two letters or digits, a
/// single ASCII punctuation character other than `\ [ ] ^ -` (Snitchwatch's
/// own wildcard rules are `[^.]*`), `\d \w \s` and their negations, and a
/// whole POSIX class in item position.
#[test]
fn common_character_classes_stay_simulated() {
    check(&[
        ("^[a-z0-9]+$", "abc123", true, true),
        ("^[a-z0-9]+$", "ABC", false, true),
        ("^[^0-9]+$", "abc", true, true),
        ("^[^0-9]+$", "a1", false, true),
        ("^[[:alpha:]_]+$", "ab_c", true, true),
        ("^[[:alpha:]_]+$", "ab-c", false, true),
        ("^[[:alpha:]]+$", "abc", true, true),
        ("^[[:alpha:]]+$", "ab1", false, true),
        ("^[[:^alpha:]]+$", "123", true, true),
        ("^[[:^alpha:]]+$", "a1", false, true),
        ("^[[:upper:][:digit:]]+$", "A1", true, true),
        ("^[[:upper:][:digit:]]+$", "a1", false, true),
        ("^[[:word:]]+$", "a_1", true, true),
        ("^[[:xdigit:]]+$", "fF9", true, true),
        ("^[[:punct:]]$", "!", true, true),
        // POSIX space includes \v; Perl \s does not.
        ("^[[:space:]]$", "\u{b}", true, true),
        ("^[\\s]$", "\u{b}", false, true),
        ("^[\\w]+$", "a_1", true, true),
        ("^[\\w]$", "é", false, true),
        ("^[^\\w]+$", "é", true, true),
        ("^[\\W]$", "é", true, true),
        ("^[^\\s]+$", "a b", false, true),
        ("^[\\s]+$", " \t", true, true),
        ("^[\\d\\s]+$", "1 2", true, true),
        ("^[^\\D]$", "1", true, true),
        ("^[^\\D]$", "a", false, true),
        ("^[^[:alpha:]]$", "1", true, true),
        ("^[0-9A-Fa-f]+$", "dEaD", true, true),
        ("^[0-9A-Fa-f]+$", "xyz", false, true),
        ("^[A-Z][a-z]+$", "Hello", true, true),
        ("^[A-Z][a-z]+$", "hello", false, true),
        ("^[a-c]$", "d", false, true),
        ("^[_]$", "_", true, true),
        // Plain punctuation is a single literal. Snitchwatch's own wildcard
        // host rules are `^[^.]*\\.example$`.
        (
            "^[^.]*\\.tracker\\.example$",
            "ads.tracker.example",
            true,
            true,
        ),
        (
            "^[^.]*\\.tracker\\.example$",
            "a.b.tracker.example",
            false,
            true,
        ),
        ("^[^.]$", "a", true, true),
        ("^[^.]$", ".", false, true),
        ("^[\\w.]+$", "a.b", true, true),
        ("^[\\w.]+$", "a-b", false, true),
        ("^[^/]+$", "ab", true, true),
        ("^[^/]+$", "a/b", false, true),
        ("[a&&b]", "&", true, true),
        ("[a&&b]", "c", false, true),
        ("[~~]", "~", true, true),
        ("^[a&|~]+$", "~|&a", true, true),
        ("^[a_]+$", "a_a", true, true),
    ]);
}

/// Every adversarial class from the review rounds, and the common forms the
/// allowlist gives up on: a `-` that isn't a range between two letters or
/// digits, an escaped character other than `\d \w \s`, a `[` that isn't a whole
/// POSIX class (including as a range end), a leading `]` or a mid-class `^`,
/// `\p`/`\x` in a class, and non-ASCII.
#[test]
fn every_other_class_form_is_not_simulated() {
    check(&[
        // Round 3 (the dash that starts a range versus the range dash).
        ("[^--a]", "0", false, false),
        ("[--a]", "0", true, false),
        ("[a-c--e]", "d", true, false),
        ("[\\d--z]", "A", true, false),
        ("[[:alpha:]--z]", "0", true, false),
        ("[*----a]", "0", true, false),
        // A POSIX class read as a range end.
        ("^[!-[:alpha:]a{01}$", "!a", false, false),
        ("^[!-[:alpha:]\\bé$", "aé", true, false),
        // Round 2 (set operators, unclosed `[:`, dashes after Perl classes).
        ("^[+--]$", ",", true, false),
        ("^[a-z--]$", "-", true, false),
        ("^[[:x]]]$", "x]]", true, false),
        ("[\\w-.]", ".", true, false),
        // Round 1.
        ("^[[a]]$", "[]", true, false),
        // A punctuation character can't be a range end or start one.
        ("[.-z]", "m", true, false),
        ("^[a.-]$", "-", true, false),
        // Common forms with a literal dash or an escape.
        ("^[\\w.-]+$", "a-b", true, false),
        ("^[a-z0-9.-]+$", "a-b.c", true, false),
        ("^[ \\t]$", " ", true, false),
        ("^[a-]$", "-", true, false),
        ("^[-a]$", "-", true, false),
        ("^[]a]$", "]", true, false),
        ("^[^]a]$", "b", true, false),
        ("^[a^]$", "^", true, false),
        ("^[\\.]$", ".", true, false),
        ("^[\\]]$", "]", true, false),
        ("^[\\n]$", "\n", true, false),
        ("^[[:alpha:]-z]$", "-", true, false),
        // \p and \x inside a class, non-ASCII, unclosed.
        ("^[\\p{Greek}]$", "α", true, false),
        ("^[\\x41]$", "A", true, false),
        ("^[é]$", "é", true, false),
        ("^[a-é]$", "b", true, false),
        ("^[a-z", "a", false, false),
        ("^[]$", "a", false, false),
    ]);
}

/// Case folding: the `(?i)` flag inside a pattern (Go folds with Unicode
/// simple folding, class members included), and the operator's own
/// non-sensitive mode, where the daemon lowercases the pattern text and the
/// subject (`strings.ToLower`, one rune at a time) before matching.
#[test]
fn case_folding_answers_like_go() {
    check(&[
        // `(?i)`: k, K and the Kelvin sign are one orbit; so are s, S and ſ.
        ("(?i)^k$", "\u{212A}", true, true),
        ("(?i)^s$", "ſ", true, true),
        ("(?i)^[a-c]$", "B", true, true),
        ("(?i)^[[:upper:]]$", "k", true, true),
        // A negated POSIX class is negated after folding, so no letter of
        // either case is in it.
        ("(?i)^[[:^upper:]]$", "K", false, true),
        ("(?i)^[[:^upper:]]$", "k", false, true),
        ("(?i)^[[:^upper:]]$", "1", true, true),
    ]);
    check_with(
        false,
        &[
            // Both sides are lowercased: the Kelvin sign becomes `k`, but the
            // long s is already lowercase and is not `s`.
            ("k", "\u{212A}", true, true),
            ("^s$", "ſ", false, true),
            // The class bounds are lowercased with the rest of the pattern...
            ("^[A-Z]$", "Q", true, true),
            // ...so a range that was in order can arrive reversed (Go refuses
            // to compile `[z-a]`; the simulator doesn't guess).
            ("^[Z-a]$", "m", false, false),
            // ...and `\D` becomes `\d`.
            ("^\\D$", "5", true, true),
            ("^\\D$", "x", false, true),
            ("^[[:UPPER:]]$", "Q", false, true),
        ],
    );
}
