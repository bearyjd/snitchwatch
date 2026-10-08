//! Checks for a GUI-sourced `regexp` operator's pattern.
//!
//! opensnitchd compiles the pattern with Go's RE2 `regexp`, after
//! lowercasing it when `sensitive` is false (`operator.go` `Compile`), and
//! matches it unanchored. Rust's `regex` is close to RE2 but not the same
//! dialect, so a pattern is walked as a `regex_syntax` AST (never scanned as
//! text: `\\D` is a backslash and a `D`, not `\D`) and refused when:
//!
//! - it is empty: an empty pattern matches every connection, and a GUI
//!   rule's missing `data` arrives as `""`;
//! - it is case-insensitive and lowercasing changes what it means. `\D`,
//!   `\S`, `\W` and `\B` become `\d`, `\s`, `\w` and `\b`, so `^\D*$` turns
//!   into `^\d*$` and matches the empty `dest.host` of every bare-IP
//!   connection; `\A` becomes the bell character. `\p`/`\P` classes are
//!   refused too: Go's class names are case-sensitive and capitalized, so a
//!   lowercased one never compiles there, while Rust matches names loosely;
//! - it uses syntax Go doesn't share: a repetition count over 1000, class
//!   set operations (`&&`, `--`, `~~`), nested classes, flags other than
//!   `i`, `m`, `s` and `U`, the `\<`/`\>`/`\b{…}` assertions, `\u`/`\U`
//!   escapes, or `\p{name=value}` properties;
//! - its compiled program is over 1 MiB.
//!
//! Errors are fixed text: `regex_syntax` and `regex` errors quote the
//! pattern, which is client-supplied.

use std::borrow::Cow;

use regex_syntax::ast::{
    self, parse::ParserBuilder, AssertionKind, Ast, ClassSetBinaryOp, ClassSetItem,
    ClassUnicodeKind, Flag, Flags, FlagsItemKind, GroupKind, HexLiteralKind, Literal, LiteralKind,
    RepetitionKind, RepetitionRange,
};

/// Go's `regexp/syntax` refuses a larger `{n,m}` count.
const MAX_REPEAT: u32 = 1000;
/// Cap on the compiled program, well under `regex`'s 10 MiB default.
const SIZE_LIMIT: usize = 1 << 20;

const INVALID: &str = "operator data is not a valid regular expression";
const CASE_CHANGES_MEANING: &str = "a case-insensitive regular expression can't use \\D, \\S, \
     \\W, \\B or \\A: the firewall service lowercases the pattern, which changes their meaning";
const CASE_BREAKS_CLASS: &str = "a case-insensitive regular expression can't use \\p or \\P \
     classes: the firewall service lowercases the pattern, which breaks the class name";

pub(super) fn validate_regexp(data: &str, sensitive: bool) -> Result<(), String> {
    if data.is_empty() {
        return Err("an empty regular expression matches every connection".to_string());
    }
    if !sensitive {
        // The pattern as written: once lowercased, `\D` already reads `\d`.
        ast::visit(&parse(data)?, CaseFoldCheck).map_err(str::to_string)?;
    }
    let effective = if sensitive {
        Cow::Borrowed(data)
    } else {
        Cow::Owned(data.to_lowercase())
    };
    ast::visit(&parse(&effective)?, DialectCheck).map_err(str::to_string)?;
    regex::RegexBuilder::new(&effective)
        .size_limit(SIZE_LIMIT)
        .build()
        .map(|_| ())
        .map_err(|_| INVALID.to_string())
}

fn parse(pattern: &str) -> Result<Ast, String> {
    ParserBuilder::new()
        .build()
        .parse(pattern)
        .map_err(|_| INVALID.to_string())
}

/// Escapes whose meaning changes when the daemon lowercases the pattern.
struct CaseFoldCheck;

impl ast::Visitor for CaseFoldCheck {
    type Output = ();
    type Err = &'static str;

    fn finish(self) -> Result<(), &'static str> {
        Ok(())
    }

    fn visit_pre(&mut self, ast: &Ast) -> Result<(), &'static str> {
        match ast {
            Ast::ClassPerl(class) if class.negated => Err(CASE_CHANGES_MEANING),
            Ast::Assertion(assertion)
                if matches!(
                    assertion.kind,
                    AssertionKind::NotWordBoundary | AssertionKind::StartText
                ) =>
            {
                Err(CASE_CHANGES_MEANING)
            }
            Ast::ClassUnicode(_) => Err(CASE_BREAKS_CLASS),
            _ => Ok(()),
        }
    }

    fn visit_class_set_item_pre(&mut self, item: &ClassSetItem) -> Result<(), &'static str> {
        match item {
            ClassSetItem::Perl(class) if class.negated => Err(CASE_CHANGES_MEANING),
            ClassSetItem::Unicode(_) => Err(CASE_BREAKS_CLASS),
            _ => Ok(()),
        }
    }
}

/// Syntax Rust's `regex` accepts but Go's RE2 refuses or reads differently.
struct DialectCheck;

impl ast::Visitor for DialectCheck {
    type Output = ();
    type Err = &'static str;

    fn finish(self) -> Result<(), &'static str> {
        Ok(())
    }

    fn visit_pre(&mut self, ast: &Ast) -> Result<(), &'static str> {
        match ast {
            Ast::Flags(set) => check_flags(&set.flags),
            Ast::Group(group) => match &group.kind {
                GroupKind::NonCapturing(flags) => check_flags(flags),
                _ => Ok(()),
            },
            Ast::Repetition(repetition) => match &repetition.op.kind {
                RepetitionKind::Range(
                    RepetitionRange::Exactly(n) | RepetitionRange::AtLeast(n),
                ) if *n > MAX_REPEAT => Err(REPEAT_TOO_LARGE),
                RepetitionKind::Range(RepetitionRange::Bounded(min, max))
                    if *min > MAX_REPEAT || *max > MAX_REPEAT =>
                {
                    Err(REPEAT_TOO_LARGE)
                }
                _ => Ok(()),
            },
            Ast::Assertion(assertion) => match assertion.kind {
                AssertionKind::StartLine
                | AssertionKind::EndLine
                | AssertionKind::StartText
                | AssertionKind::EndText
                | AssertionKind::WordBoundary
                | AssertionKind::NotWordBoundary => Ok(()),
                _ => Err("a regular expression can't use \\<, \\> or \\b{...}"),
            },
            Ast::Literal(literal) => check_literal(literal),
            Ast::ClassUnicode(class) => check_unicode_class(&class.kind),
            _ => Ok(()),
        }
    }

    fn visit_class_set_item_pre(&mut self, item: &ClassSetItem) -> Result<(), &'static str> {
        match item {
            ClassSetItem::Bracketed(_) => Err("a regular expression can't nest [...] classes"),
            ClassSetItem::Literal(literal) => check_literal(literal),
            ClassSetItem::Range(range) => {
                check_literal(&range.start).and_then(|()| check_literal(&range.end))
            }
            ClassSetItem::Unicode(class) => check_unicode_class(&class.kind),
            _ => Ok(()),
        }
    }

    fn visit_class_set_binary_op_pre(
        &mut self,
        _op: &ClassSetBinaryOp,
    ) -> Result<(), &'static str> {
        Err("a regular expression can't use class set operations (&&, --, ~~)")
    }
}

const REPEAT_TOO_LARGE: &str = "a regular expression repetition count is over 1000";

fn check_flags(flags: &Flags) -> Result<(), &'static str> {
    let all_shared = flags.items.iter().all(|item| {
        matches!(
            item.kind,
            FlagsItemKind::Negation
                | FlagsItemKind::Flag(
                    Flag::CaseInsensitive
                        | Flag::MultiLine
                        | Flag::DotMatchesNewLine
                        | Flag::SwapGreed
                )
        )
    });
    if all_shared {
        Ok(())
    } else {
        Err("a regular expression can only use the i, m, s and U flags")
    }
}

fn check_literal(literal: &Literal) -> Result<(), &'static str> {
    match literal.kind {
        LiteralKind::HexFixed(HexLiteralKind::UnicodeShort | HexLiteralKind::UnicodeLong)
        | LiteralKind::HexBrace(HexLiteralKind::UnicodeShort | HexLiteralKind::UnicodeLong) => {
            Err("a regular expression can't use \\u or \\U escapes; use \\x{...}")
        }
        _ => Ok(()),
    }
}

fn check_unicode_class(kind: &ClassUnicodeKind) -> Result<(), &'static str> {
    match kind {
        ClassUnicodeKind::NamedValue { .. } => {
            Err("a regular expression can't use \\p{name=value} classes")
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_regexp;

    fn insensitive(pattern: &str) -> Result<(), String> {
        validate_regexp(pattern, false)
    }

    fn sensitive(pattern: &str) -> Result<(), String> {
        validate_regexp(pattern, true)
    }

    #[test]
    fn an_empty_pattern_is_refused_but_an_anchored_empty_match_is_not() {
        assert!(insensitive("").is_err());
        assert!(sensitive("").is_err());
        assert_eq!(insensitive("^$"), Ok(()));
    }

    #[test]
    fn escapes_that_lowercasing_changes_are_refused_when_insensitive() {
        for pattern in [
            r"^\D*$",
            r"\S+",
            r"a\Wb",
            r"a\B",
            r"\Aexample",
            r"[\D]",
            r"[^\W]",
            r"[a\S]",
        ] {
            assert!(insensitive(pattern).is_err(), "{pattern}");
            assert_eq!(sensitive(pattern), Ok(()), "{pattern} keeps its meaning");
        }
    }

    #[test]
    fn unicode_classes_are_refused_when_insensitive() {
        for pattern in [r"\pL", r"\p{Greek}", r"\P{Greek}", r"[\pN]"] {
            assert!(insensitive(pattern).is_err(), "{pattern}");
            assert_eq!(sensitive(pattern), Ok(()), "{pattern}");
        }
    }

    #[test]
    fn the_walk_reads_escapes_not_text() {
        // A literal backslash followed by `D` is not `\D`.
        assert_eq!(insensitive(r"\\D"), Ok(()));
        assert_eq!(insensitive(r"[\\D]"), Ok(()));
        assert_eq!(insensitive(r"^\d\s\w\b\z$"), Ok(()));
        assert_eq!(
            insensitive(r"^(?:[^.]+\.)*example\.com$"),
            Ok(()),
            "the verdict builder's pattern"
        );
    }

    #[test]
    fn repetition_counts_over_1000_are_refused() {
        for pattern in ["a{1001}", "a{1001,}", "a{2,1001}", "a{1001,1002}"] {
            assert!(sensitive(pattern).is_err(), "{pattern}");
        }
        assert_eq!(sensitive("a{1000}"), Ok(()));
        assert_eq!(sensitive("a{0,1000}"), Ok(()));
    }

    #[test]
    fn class_set_operations_and_nested_classes_are_refused() {
        for pattern in ["[a-z&&b]", "[a-z--b]", "[a-z~~b]", "[a[bc]]", "[^[a]]"] {
            assert!(sensitive(pattern).is_err(), "{pattern}");
        }
        assert_eq!(
            sensitive("[[:alpha:]]"),
            Ok(()),
            "an ASCII class isn't nesting"
        );
        assert_eq!(sensitive("[^a-z0-9.]"), Ok(()));
    }

    #[test]
    fn only_go_flags_are_accepted() {
        for pattern in ["(?x)a", "(?u)a", "(?R)a", "(?ix:a)", "(?-u)a"] {
            assert!(sensitive(pattern).is_err(), "{pattern}");
        }
        for pattern in ["(?i)a", "(?ims)a", "(?U)a+", "(?i-s:a.)", "(?sU)a"] {
            assert_eq!(sensitive(pattern), Ok(()), "{pattern}");
        }
        // Lowercased, `(?U)` reads `(?u)`, which Go doesn't have.
        assert!(insensitive("(?U)a+").is_err());
    }

    #[test]
    fn rust_only_escapes_are_refused() {
        for pattern in [
            r"\<a",
            r"a\>",
            r"\b{start}a",
            r"\b{end}",
            r"\u0041",
            r"\U00000041",
            r"\u{41}",
            r"[\u0041]",
            r"\p{sc=Greek}",
            r"[\p{sc=Greek}]",
        ] {
            assert!(sensitive(pattern).is_err(), "{pattern}");
        }
        assert_eq!(sensitive(r"\x41\x{41}[\x41-\x{5A}]"), Ok(()));
    }

    #[test]
    fn a_pattern_over_the_size_limit_is_refused() {
        let pattern = "(?:[a-z]{1000}){20}";
        assert!(regex::Regex::new(pattern).is_ok(), "fits regex's default");
        assert!(sensitive(pattern).is_err());
    }

    #[test]
    fn errors_never_quote_the_pattern() {
        for pattern in [
            r"(MARKER",
            r"\DMARKER",
            "MARKER{1001}",
            r"\pMARKER",
            "[MARKER&&a]",
        ] {
            let err = insensitive(pattern).unwrap_err();
            assert!(!err.contains("MARKER") && !err.contains("marker"), "{err}");
        }
    }
}
