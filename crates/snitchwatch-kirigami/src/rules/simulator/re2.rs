//! Reading an RE2 pattern (Go's `regexp`, which opensnitchd compiles) with the
//! `regex` crate, only where the two are known to agree.
//!
//! The syntaxes are close, but not the same, and a difference that compiles
//! gives a wrong *answer* rather than an error. So the pattern is rewritten
//! before the crate sees it, in two parts.
//!
//! **Outside character classes**, the differences known so far are fixed:
//!
//!   * `\w \d \s \b` (and `\W \D \S \B`) are ASCII-only in RE2 but Unicode in
//!     the crate (where `\s` also covers `\v` and NBSP). Each becomes its
//!     ASCII set; the negated forms become negated sets, which still match
//!     any non-ASCII character as RE2's do.
//!   * A backslash before punctuation is that character in RE2, but the crate
//!     reads `\<` and `\>` as word-boundary assertions.
//!   * A `{` that isn't `{n}`, `{n,}` or `{n,m}` is text in RE2 (a bound with
//!     a leading zero, `{01}`, isn't a number to Go either), but the crate
//!     rejects `{,2}` and reads `{01}` as `{1}`. It is escaped. `\p{..}` and
//!     `\x{..}` keep their braces.
//!
//! **Inside a character class** nothing is patched. Go's class grammar (an
//! item, then an optional `-` and a range end, with `[:name:]` only in item
//! position, `]` and `-` literal in some places and `[` always literal as a
//! range end) differs from the crate's in many small ways, and every
//! patched difference so far has uncovered the next one. So a class is read
//! by an allowlist, and any class that uses something else makes the whole
//! pattern **not simulated** ([`UnmodelledClass`]) instead of guessing. The
//! allowlist is:
//!
//!   * an optional leading `^`;
//!   * ASCII letters and digits as literals, and a range whose two ends are
//!     both ASCII letters or digits;
//!   * ASCII punctuation other than `\ [ ] ^ -` as a single literal, which
//!     is never a range end and is never followed by a `-`. (Snitchwatch's own
//!     wildcard-host rules are `^[^.]*\.example$`, so `.` has to be here; none
//!     of these characters is special in Go's class grammar, and written as
//!     hex escapes none can form one of the crate's `&&`, `--` or `~~`
//!     operators.)
//!   * `\d \w \s \D \W \S`;
//!   * a whole POSIX class `[:name:]` / `[:^name:]` for the names Go knows,
//!     in item position only.
//!
//! Accepted items are written for the crate with `\x{..}` hex escapes and
//! ranges only (plus nested ASCII sets), so no literal punctuation ever
//! reaches the crate's class parser.
//!
//! Syntax only RE2 has (`\Q..\E`) is left alone and fails to compile; the
//! caller then reports the condition as not simulated rather than as a miss.

use std::iter::Peekable;
use std::str::Chars;

/// The pattern has a character class outside the allowlist.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct UnmodelledClass;

pub(super) fn to_regex_crate(pattern: &str) -> Result<String, UnmodelledClass> {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => escape(&mut chars, &mut out),
            '[' => class(&mut chars, &mut out)?,
            '{' => brace(&mut chars, &mut out),
            _ => out.push(c),
        }
    }
    Ok(out)
}

/// After a backslash, outside a class: ASCII sets for the Perl classes, an
/// ASCII word boundary for `\b`/`\B`, plain `<` and `>`, braced `\p{..}`,
/// `\P{..}` and `\x{..}` copied whole, and everything else (including `\\`
/// and `\[`) copied as a pair so the escaped character is never read as
/// syntax.
fn escape(chars: &mut Peekable<Chars>, out: &mut String) {
    let Some(esc) = chars.next() else {
        out.push('\\');
        return;
    };
    match esc {
        'w' | 'W' | 'd' | 'D' | 's' | 'S' => {
            let negated = esc.is_ascii_uppercase();
            out.push_str(&ascii_set(perl_ranges(esc.to_ascii_lowercase()), negated));
        }
        '<' | '>' => out.push(esc),
        'p' | 'P' | 'x' if chars.peek() == Some(&'{') => {
            out.push('\\');
            out.push(esc);
            for c in chars.by_ref() {
                out.push(c);
                if c == '}' {
                    break;
                }
            }
        }
        'b' => out.push_str(r"(?-u:\b)"),
        'B' => out.push_str(r"(?-u:\B)"),
        _ => {
            out.push('\\');
            out.push(esc);
        }
    }
}

/// A `{` outside a class: RE2 reads `{n}`, `{n,}` and `{n,m}` as repeats and
/// any other `{` (such as `{,2}`) as text.
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
    // Go's parseInt refuses a leading zero, so `{01}` is text.
    let number = |text: &str| {
        text.bytes().all(|b| b.is_ascii_digit()) && !(text.len() > 1 && text.starts_with('0'))
    };
    !min.is_empty() && number(min) && max.is_none_or(number)
}

// ---- character classes ----------------------------------------------------

type Ranges = &'static [(u8, u8)];

const DIGIT: Ranges = &[(b'0', b'9')];
const WORD: Ranges = &[(b'0', b'9'), (b'A', b'Z'), (b'a', b'z'), (b'_', b'_')];
/// Perl `\s`: `\t \n \f \r` and space (no `\v`).
const PERL_SPACE: Ranges = &[(9, 10), (12, 13), (32, 32)];

/// Go's POSIX classes (`regexp/syntax/perl_groups.go`).
const POSIX: &[(&str, Ranges)] = &[
    ("alnum", &[(b'0', b'9'), (b'A', b'Z'), (b'a', b'z')]),
    ("alpha", &[(b'A', b'Z'), (b'a', b'z')]),
    ("ascii", &[(0, 0x7F)]),
    ("blank", &[(9, 9), (32, 32)]),
    ("cntrl", &[(0, 0x1F), (0x7F, 0x7F)]),
    ("digit", DIGIT),
    ("graph", &[(0x21, 0x7E)]),
    ("lower", &[(b'a', b'z')]),
    ("print", &[(0x20, 0x7E)]),
    (
        "punct",
        &[(0x21, 0x2F), (0x3A, 0x40), (0x5B, 0x60), (0x7B, 0x7E)],
    ),
    ("space", &[(9, 13), (32, 32)]),
    ("upper", &[(b'A', b'Z')]),
    ("word", WORD),
    ("xdigit", &[(b'0', b'9'), (b'A', b'F'), (b'a', b'f')]),
];

fn perl_ranges(class: char) -> Ranges {
    match class {
        'w' => WORD,
        'd' => DIGIT,
        _ => PERL_SPACE,
    }
}

/// `\x{..}` for one byte.
fn hex(byte: u8) -> String {
    format!(r"\x{{{byte:X}}}")
}

/// The ranges as members of a class, in hex escapes only.
fn members(ranges: Ranges) -> String {
    let mut out = String::new();
    for &(lo, hi) in ranges {
        out.push_str(&hex(lo));
        if hi != lo {
            out.push('-');
            out.push_str(&hex(hi));
        }
    }
    out
}

/// An ASCII set as a class of its own. Negated, it still matches every
/// non-ASCII character, as RE2's `\W`, `\D` and `\S` do.
fn ascii_set(ranges: Ranges, negated: bool) -> String {
    format!("[{}{}]", if negated { "^" } else { "" }, members(ranges))
}

fn is_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// ASCII punctuation that is an ordinary character in Go's class grammar:
/// everything but `\ [ ] ^ -`.
fn is_plain_punctuation(c: char) -> bool {
    c.is_ascii_punctuation() && !matches!(c, '\\' | '[' | ']' | '^' | '-')
}

/// After the `[` of a class, in Go's grammar: an optional `^`, then items up
/// to the closing `]`. Anything outside the module's allowlist is an error.
fn class(chars: &mut Peekable<Chars>, out: &mut String) -> Result<(), UnmodelledClass> {
    out.push('[');
    if chars.peek() == Some(&'^') {
        chars.next();
        out.push('^');
    }
    let mut items = 0;
    loop {
        let c = chars.next().ok_or(UnmodelledClass)?;
        match c {
            // A `]` before any item is a literal in Go.
            ']' if items > 0 => {
                out.push(']');
                return Ok(());
            }
            c if is_alnum(c) => range_or_literal(c, chars, out)?,
            // A `-` after it (`[+--]`) is refused on the next turn: `-` is
            // not an item, so punctuation never starts a range.
            c if is_plain_punctuation(c) => out.push_str(&hex(c as u8)),
            '\\' => perl_class(chars, out)?,
            '[' => posix_class(chars, out)?,
            _ => return Err(UnmodelledClass),
        }
        items += 1;
    }
}

/// A letter or digit, and a range if a `-` follows: both ends must be letters
/// or digits, in order. (A `-` that isn't a range between two such ends is
/// outside the allowlist.)
fn range_or_literal(
    lo: char,
    chars: &mut Peekable<Chars>,
    out: &mut String,
) -> Result<(), UnmodelledClass> {
    out.push_str(&hex(lo as u8));
    if chars.peek() != Some(&'-') {
        return Ok(());
    }
    chars.next();
    match chars.next() {
        Some(hi) if is_alnum(hi) && hi >= lo => {
            out.push('-');
            out.push_str(&hex(hi as u8));
            Ok(())
        }
        _ => Err(UnmodelledClass),
    }
}

/// After a backslash inside a class: only `\d \w \s \D \W \S`.
fn perl_class(chars: &mut Peekable<Chars>, out: &mut String) -> Result<(), UnmodelledClass> {
    match chars.next() {
        Some(esc @ ('d' | 'w' | 's' | 'D' | 'W' | 'S')) => {
            let negated = esc.is_ascii_uppercase();
            out.push_str(&ascii_set(perl_ranges(esc.to_ascii_lowercase()), negated));
            Ok(())
        }
        _ => Err(UnmodelledClass),
    }
}

/// After a `[` in item position inside a class: a whole `[:name:]` or
/// `[:^name:]` for a name Go knows. A `[` that isn't one (a literal bracket
/// in Go, or one read as a range end) is outside the allowlist.
fn posix_class(chars: &mut Peekable<Chars>, out: &mut String) -> Result<(), UnmodelledClass> {
    if chars.next() != Some(':') {
        return Err(UnmodelledClass);
    }
    let mut name = String::new();
    loop {
        match chars.next() {
            Some(':') => break,
            Some(c) if c.is_ascii_alphabetic() || (c == '^' && name.is_empty()) => name.push(c),
            _ => return Err(UnmodelledClass),
        }
    }
    if chars.next() != Some(']') {
        return Err(UnmodelledClass);
    }
    let (negated, name) = match name.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, name.as_str()),
    };
    let (_, ranges) = POSIX
        .iter()
        .find(|(known, _)| *known == name)
        .ok_or(UnmodelledClass)?;
    out.push_str(&ascii_set(ranges, negated));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::to_regex_crate as rewrite;
    use super::UnmodelledClass;

    fn ok(pattern: &str) -> String {
        rewrite(pattern).unwrap_or_else(|_| panic!("{pattern} was refused"))
    }

    fn refused(pattern: &str) {
        assert_eq!(rewrite(pattern), Err(UnmodelledClass), "{pattern}");
    }

    #[test]
    fn perl_classes_become_ascii_sets() {
        assert_eq!(
            ok(r"\w\d"),
            r"[\x{30}-\x{39}\x{41}-\x{5A}\x{61}-\x{7A}\x{5F}][\x{30}-\x{39}]"
        );
        assert_eq!(ok(r"\D"), r"[^\x{30}-\x{39}]");
        assert_eq!(ok(r"\s"), r"[\x{9}-\x{A}\x{C}-\x{D}\x{20}]");
    }

    #[test]
    fn word_boundaries_become_ascii() {
        assert_eq!(ok(r"\bx\B"), r"(?-u:\b)x(?-u:\B)");
    }

    #[test]
    fn accepted_class_items_are_written_as_hex_escapes_only() {
        assert_eq!(ok("[a-c_]"), r"[\x{61}-\x{63}\x{5F}]");
        assert_eq!(ok("[^0-9]"), r"[^\x{30}-\x{39}]");
        assert_eq!(ok("[x7]"), r"[\x{78}\x{37}]");
        // Punctuation is written as hex too, so `&&`, `~~` and `--` can never
        // reach the crate as operators.
        assert_eq!(ok("[a&&b]"), r"[\x{61}\x{26}\x{26}\x{62}]");
        assert_eq!(ok("[~~]"), r"[\x{7E}\x{7E}]");
        assert_eq!(ok("[^.]"), r"[^\x{2E}]");
        assert_eq!(ok(r"[\d]"), r"[[\x{30}-\x{39}]]");
        assert_eq!(ok(r"[^\S]"), r"[^[^\x{9}-\x{A}\x{C}-\x{D}\x{20}]]");
        assert_eq!(ok("[[:alpha:]x]"), r"[[\x{41}-\x{5A}\x{61}-\x{7A}]\x{78}]");
        assert_eq!(ok("[[:^digit:]]"), r"[[^\x{30}-\x{39}]]");
    }

    #[test]
    fn no_literal_punctuation_reaches_the_crates_class_parser() {
        for pattern in [
            "[a-z0-9_]",
            "[^a-f]",
            r"[\w\d\s]",
            "[[:punct:]a]",
            "[[:cntrl:]]",
        ] {
            let written = ok(pattern);
            let inside_classes: String = written
                .chars()
                .filter(|c| !matches!(c, '[' | ']' | '^' | '-' | '{' | '}' | '\\' | 'x'))
                .filter(|c| !c.is_ascii_hexdigit())
                .collect();
            assert_eq!(inside_classes, "", "{pattern} -> {written}");
        }
    }

    #[test]
    fn classes_outside_the_allowlist_are_refused() {
        for pattern in [
            "[^--a]",
            "[--a]",
            "[a-c--e]",
            r"[\d--z]",
            "[[:alpha:]--z]",
            "[*----a]",
            "[!-[:alpha:]a",
            "[+--]",
            "[a-z--]",
            "[[:x]]]",
            r"[\w-.]",
            "[[a]]",
            "[.-z]",
            "[a.-]",
            "[/-a]",
            r"[\w.-]",
            "[a-]",
            "[-a]",
            "[]a]",
            "[^]a]",
            "[a^]",
            r"[\.]",
            r"[\]]",
            r"[\n]",
            r"[\p{Greek}]",
            r"[\x41]",
            "[é]",
            "[a-é]",
            "[a-[]",
            "[z-a]",
            "[a b]",
            "[a-z",
            "[]",
            "[[:alpha:]",
            "[[:nosuch:]]",
            "[[:alpha]]",
            "[[:^:]]",
        ] {
            refused(pattern);
        }
    }

    #[test]
    fn a_posix_class_is_only_read_in_item_position() {
        refused("[a-[:alpha:]]");
        refused("[!-[:alpha:]");
        assert!(rewrite("[[:alpha:]]").is_ok());
        assert!(rewrite("[a[:alpha:]]").is_ok());
    }

    #[test]
    fn an_escaped_bracket_outside_a_class_is_not_a_class() {
        assert_eq!(ok(r"\[a-z\]"), r"\[a-z\]");
        assert_eq!(ok(r"\\[a]"), r"\\[\x{61}]");
    }

    #[test]
    fn escaped_angle_brackets_are_plain_characters() {
        assert_eq!(ok(r"a\<b\>"), "a<b>");
    }

    #[test]
    fn only_digit_repeats_without_leading_zeros_are_repeats() {
        assert_eq!(ok("x{2}y{2,}z{2,3}"), "x{2}y{2,}z{2,3}");
        assert_eq!(ok("x{0}y{0,5}"), "x{0}y{0,5}");
        assert_eq!(ok("x{,2}"), r"x\{,2}");
        assert_eq!(ok("x{"), r"x\{");
        assert_eq!(ok("x{1,2,3}"), r"x\{1,2,3}");
        assert_eq!(ok("x{a}"), r"x\{a}");
        assert_eq!(ok("x{01}"), r"x\{01}");
        assert_eq!(ok("x{0,01}"), r"x\{0,01}");
        assert_eq!(ok("x{1,2}{3}"), "x{1,2}{3}");
    }

    #[test]
    fn braced_escapes_keep_their_braces() {
        assert_eq!(ok(r"\p{Greek}\x{1F}\P{L}"), r"\p{Greek}\x{1F}\P{L}");
    }

    #[test]
    fn syntax_it_does_not_know_is_left_alone() {
        assert_eq!(ok(r"\Qa.b\E"), r"\Qa.b\E");
        assert_eq!(ok("trailing\\"), "trailing\\");
    }
}
