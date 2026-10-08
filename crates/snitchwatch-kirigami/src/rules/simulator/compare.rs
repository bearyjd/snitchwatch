//! The daemon's string comparisons, reproduced exactly: `Operator.simpleCmp`
//! (`strings.EqualFold`), `reCmp` (lowercase subject, pattern lowercased at
//! `Compile`) and `hashCmp` (`vendor/opensnitch/daemon/rule/operator.go`).

use super::re2;

/// `Operator.simpleCmp`: `strings.EqualFold` unless `sensitive`, then `==`.
pub(super) fn simple_cmp(subject: &str, data: &str, sensitive: bool) -> bool {
    if sensitive {
        subject == data
    } else {
        go_equal_fold(subject, data)
    }
}

/// `Operator.hashCmp`: an exact `==` whatever `sensitive` says, and an empty
/// subject is a deliberate fake match ("to avoid displaying a pop-up").
pub(super) fn hash_cmp(subject: &str, data: &str) -> bool {
    subject.is_empty() || subject == data
}

/// The compiled form of a `regexp` operator, as `Operator.Compile` and
/// `reCmp` leave it: unless `sensitive`, the pattern source and the subject
/// are both lowercased (so `\D` becomes `\d`), and the match is unanchored.
pub(super) struct Regexp {
    re: regex::Regex,
    sensitive: bool,
}

/// Compiled-program size cap: bounded repeats such as `{1,253}` need more
/// than the crate's 10 MiB default, and rules opensnitchd already accepted
/// must not be misread as a miss for being big.
const REGEX_SIZE_LIMIT: usize = 64 * 1024 * 1024;

/// Why [`Regexp::compile`] failed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RegexpError {
    /// The compiled program passes the size cap.
    TooLarge,
    /// The `regex` crate doesn't accept the syntax.
    Syntax,
    /// A character class outside what `re2` models.
    ClassForm,
}

impl Regexp {
    /// An error is a difference between the two engines, not a pattern that
    /// cannot match: opensnitchd compiled it (a rule that fails there is never
    /// loaded).
    pub(super) fn compile(pattern: &str, sensitive: bool) -> Result<Self, RegexpError> {
        Self::compile_with_limit(pattern, sensitive, REGEX_SIZE_LIMIT)
    }

    fn compile_with_limit(
        pattern: &str,
        sensitive: bool,
        size_limit: usize,
    ) -> Result<Self, RegexpError> {
        let pattern = if sensitive {
            pattern.to_string()
        } else {
            go_to_lower(pattern)
        };
        let rewritten = re2::to_regex_crate(&pattern).map_err(|_| RegexpError::ClassForm)?;
        let re = regex::RegexBuilder::new(&rewritten)
            .size_limit(size_limit)
            .build()
            .map_err(|e| match e {
                regex::Error::CompiledTooBig(_) => RegexpError::TooLarge,
                _ => RegexpError::Syntax,
            })?;
        Ok(Self { re, sensitive })
    }

    pub(super) fn is_match(&self, subject: &str) -> bool {
        if self.sensitive {
            self.re.is_match(subject)
        } else {
            self.re.is_match(&go_to_lower(subject))
        }
    }
}

/// `strings.EqualFold`: Unicode simple case folding, char by char.
fn go_equal_fold(a: &str, b: &str) -> bool {
    let mut left = a.chars();
    let mut right = b.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x == y || fold(x) == fold(y) => {}
            _ => return false,
        }
    }
}

/// A representative of `c`'s simple-case-folding orbit: lowercase of the
/// uppercase, using only one-to-one mappings (so `ς`/`σ`/`Σ`, `K` (Kelvin) /
/// `k`/`K` and `ſ`/`s`/`S` meet, while `ß` stays apart from `SS`). `ı`
/// (dotless i) uppercases to `I` but Unicode doesn't fold it with `i`, so it
/// is left alone.
fn fold(c: char) -> char {
    if c == '\u{131}' {
        return c;
    }
    let upper = single(c.to_uppercase()).unwrap_or(c);
    single(upper.to_lowercase()).unwrap_or(upper)
}

fn single(mut chars: impl Iterator<Item = char>) -> Option<char> {
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// `strings.ToLower`: `unicode.ToLower` per rune, one-to-one. That differs
/// from `str::to_lowercase` for `İ` (U+0130), which Go maps to a plain `i`
/// and Rust to `i` plus a combining dot, and for the context-sensitive final
/// sigma, which Go never applies.
fn go_to_lower(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{130}' => 'i',
            _ => single(c.to_lowercase()).unwrap_or(c),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_fold_matches_go_on_the_awkward_cases() {
        // Go: EqualFold("K", "k") with K = U+212A KELVIN SIGN is true.
        assert!(go_equal_fold("\u{212A}", "k"));
        assert!(go_equal_fold("ſ", "S"));
        assert!(go_equal_fold("ς", "Σ"));
        assert!(go_equal_fold("σ", "ς"));
        assert!(go_equal_fold("ẞ", "ß"));
        assert!(go_equal_fold("ǅ", "ǆ"));
        assert!(go_equal_fold("/opt/Ünï", "/opt/ünï"));
        // Not folded together by Unicode simple case folding.
        assert!(!go_equal_fold("ı", "i"));
        assert!(!go_equal_fold("İ", "i"));
        assert!(!go_equal_fold("ß", "ss"));
        assert!(!go_equal_fold("a", "ab"));
        assert!(!go_equal_fold("ab", "a"));
        assert!(go_equal_fold("", ""));
    }

    #[test]
    fn to_lower_is_go_to_lower_not_rust_to_lowercase() {
        assert_eq!(go_to_lower("ABC/Ünï"), "abc/ünï");
        assert_eq!(go_to_lower("\u{130}"), "i");
        // `str::to_lowercase` would give ς for a word-final Σ.
        assert_eq!(go_to_lower("ΑΣ"), "ασ");
        assert_eq!(go_to_lower(r"^\D\S\W$"), r"^\d\s\w$");
    }

    #[test]
    fn a_compile_failure_says_whether_the_pattern_was_too_big_or_unreadable() {
        let too_big = Regexp::compile_with_limit(r"\pL{1,50}", true, 1024);
        assert_eq!(too_big.err(), Some(RegexpError::TooLarge));
        assert_eq!(Regexp::compile("(", true).err(), Some(RegexpError::Syntax));
        assert!(Regexp::compile(r"\pL{1,50}", true).is_ok());
    }

    #[test]
    fn a_class_outside_the_allowlist_is_its_own_error() {
        assert_eq!(
            Regexp::compile("[a-]", true).err(),
            Some(RegexpError::ClassForm)
        );
        assert!(Regexp::compile("[a-z]", true).is_ok());
    }

    #[test]
    fn hash_cmp_is_exact_and_an_empty_subject_matches() {
        assert!(hash_cmp("deadbeef", "deadbeef"));
        assert!(!hash_cmp("deadbeef", "DEADBEEF"));
        assert!(hash_cmp("", "anything"));
    }
}
