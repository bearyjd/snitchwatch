//! Reading an RE2 pattern (Go's `regexp`, which opensnitchd compiles) with the
//! `regex` crate where the two would silently disagree.
//!
//! The syntaxes are close enough that most patterns mean the same thing. Four
//! differences change *answers* rather than failing to compile, so the pattern
//! is rewritten before the crate sees it:
//!
//!   * **Perl classes and `\b` are ASCII-only in RE2** (`\w` is
//!     `[0-9A-Za-z_]`, `\d` is `[0-9]`, `\s` is `[\t\n\f\r ]`) but Unicode in
//!     the `regex` crate (where `\s` also covers `\v` and NBSP). Each is
//!     replaced by its ASCII set; the negated forms become negated sets, which
//!     still match any non-ASCII character as RE2's do.
//!   * **`[` inside a class is a literal in RE2** (`[[a]]` is the class
//!     `{'[', 'a'}` then a literal `]`) but opens a nested class in the crate.
//!     It is escaped, except in a POSIX class such as `[[:alpha:]]`.
//!   * **A backslash before punctuation is that character in RE2**, but the
//!     crate reads `\<` and `\>` as word-boundary assertions; they become the
//!     plain characters.
//!   * **A `{` that isn't `{n}`, `{n,}` or `{n,m}` is text in RE2**; the crate
//!     reads `{,2}` as `{0,2}`. It is escaped.
//!
//! Syntax only RE2 has (`\Q..\E`) is left alone and fails to compile; the
//! caller then reports the condition as not simulated rather than as a miss.

use std::iter::Peekable;
use std::str::Chars;

pub(super) fn to_regex_crate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => escape(&mut chars, &mut out, in_class),
            '[' if in_class => bracket_in_class(&mut chars, &mut out),
            '[' => {
                in_class = true;
                open_class(&mut chars, &mut out);
            }
            '{' if !in_class => brace(&mut chars, &mut out),
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    out
}

/// After a backslash: ASCII sets for the Perl classes, an ASCII word boundary
/// for `\b`/`\B`, everything else (including `\\` and `\]`) copied as a pair
/// so the escaped character is never read as syntax.
fn escape(chars: &mut Peekable<Chars>, out: &mut String, in_class: bool) {
    let Some(esc) = chars.next() else {
        out.push('\\');
        return;
    };
    match esc {
        'w' => out.push_str("[0-9A-Za-z_]"),
        'W' => out.push_str("[^0-9A-Za-z_]"),
        'd' => out.push_str("[0-9]"),
        'D' => out.push_str("[^0-9]"),
        's' => out.push_str(r"[\t\n\x0C\r ]"),
        'S' => out.push_str(r"[^\t\n\x0C\r ]"),
        // RE2: a backslash before punctuation is just that character; the
        // crate would read `\<` and `\>` as word-boundary assertions.
        '<' | '>' => out.push(esc),
        'b' if !in_class => out.push_str(r"(?-u:\b)"),
        'B' if !in_class => out.push_str(r"(?-u:\B)"),
        _ => {
            out.push('\\');
            out.push(esc);
        }
    }
}

/// A `{` outside a class: RE2 reads `{n}`, `{n,}` and `{n,m}` as repeats and
/// any other `{` (such as `{,2}`) as text; the crate would read `{,2}` as
/// `{0,2}`.
fn brace(chars: &mut Peekable<Chars>, out: &mut String) {
    let mut look = chars.clone();
    let mut body = String::new();
    let closed = loop {
        match look.next() {
            Some('}') => break true,
            Some(c) if c.is_ascii_digit() || c == ',' => body.push(c),
            _ => break false,
        }
    };
    if !(closed && is_repeat_body(&body)) {
        out.push_str(r"\{");
        return;
    }
    out.push('{');
    out.push_str(&body);
    out.push('}');
    // The body and the closing brace.
    chars.nth(body.len());
}

fn is_repeat_body(body: &str) -> bool {
    let (min, max) = body
        .split_once(',')
        .map_or((body, None), |(min, max)| (min, Some(max)));
    let digits = |text: &str| text.bytes().all(|b| b.is_ascii_digit());
    !min.is_empty() && digits(min) && max.is_none_or(digits)
}

/// After the `[` that opens a class: keep a `^`, and a leading `]` is a
/// literal.
fn open_class(chars: &mut Peekable<Chars>, out: &mut String) {
    out.push('[');
    if chars.peek() == Some(&'^') {
        out.push('^');
        chars.next();
    }
    if chars.peek() == Some(&']') {
        out.push_str(r"\]");
        chars.next();
    }
}

/// A `[` inside a class: a POSIX class (`[:alpha:]`) is copied through its
/// `:]`, anything else is a literal bracket.
fn bracket_in_class(chars: &mut Peekable<Chars>, out: &mut String) {
    if chars.peek() != Some(&':') {
        out.push_str(r"\[");
        return;
    }
    out.push('[');
    while let Some(c) = chars.next() {
        out.push(c);
        if c == ':' && chars.peek() == Some(&']') {
            out.push(']');
            chars.next();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::to_regex_crate as rewrite;

    #[test]
    fn perl_classes_become_ascii_sets() {
        assert_eq!(rewrite(r"\w\d"), "[0-9A-Za-z_][0-9]");
        assert_eq!(rewrite(r"\W\D"), "[^0-9A-Za-z_][^0-9]");
        assert_eq!(rewrite(r"\s"), r"[\t\n\x0C\r ]");
    }

    #[test]
    fn word_boundaries_become_ascii_outside_classes_only() {
        assert_eq!(rewrite(r"\bx\B"), r"(?-u:\b)x(?-u:\B)");
        assert_eq!(rewrite(r"[\b]"), r"[\b]");
    }

    #[test]
    fn classes_nest_inside_a_class() {
        assert_eq!(rewrite(r"[\w.-]"), "[[0-9A-Za-z_].-]");
        assert_eq!(rewrite(r"[^\s]"), r"[^[\t\n\x0C\r ]]");
    }

    #[test]
    fn an_escaped_backslash_is_not_a_class() {
        assert_eq!(rewrite(r"\\w"), r"\\w");
        assert_eq!(rewrite(r"[\]\w]"), r"[\][0-9A-Za-z_]]");
    }

    #[test]
    fn brackets_in_a_class_are_literals_except_posix_classes() {
        assert_eq!(rewrite("[[a]]"), r"[\[a]]");
        assert_eq!(rewrite("[[:alpha:]x]"), "[[:alpha:]x]");
        assert_eq!(rewrite("[]a]"), r"[\]a]");
        assert_eq!(rewrite("[^]a]"), r"[^\]a]");
    }

    #[test]
    fn only_digit_repeats_are_repeats() {
        assert_eq!(rewrite("x{2}y{2,}z{2,3}"), "x{2}y{2,}z{2,3}");
        assert_eq!(rewrite("x{,2}"), r"x\{,2}");
        assert_eq!(rewrite("x{"), r"x\{");
        assert_eq!(rewrite("x{1,2,3}"), r"x\{1,2,3}");
        assert_eq!(rewrite("x{a}"), r"x\{a}");
        assert_eq!(rewrite("x{1,2}{3}"), "x{1,2}{3}");
        assert_eq!(rewrite("[{,2}]"), "[{,2}]");
    }

    #[test]
    fn escaped_angle_brackets_are_plain_characters() {
        assert_eq!(rewrite(r"a\<b\>"), "a<b>");
        assert_eq!(rewrite(r"[\<]"), "[<]");
    }

    #[test]
    fn syntax_it_does_not_know_is_left_alone() {
        assert_eq!(rewrite(r"\Qa.b\E"), r"\Qa.b\E");
        assert_eq!(rewrite("trailing\\"), "trailing\\");
    }
}
