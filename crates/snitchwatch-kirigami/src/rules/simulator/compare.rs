//! The daemon's string comparisons, reproduced exactly: `Operator.simpleCmp`
//! (`strings.EqualFold`), `reCmp` (lowercase subject, pattern lowercased at
//! `Compile`) and `hashCmp` (`vendor/opensnitch/daemon/rule/operator.go`).

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

/// `Operator.reCmp` with the pattern as `Compile` leaves it: unless
/// `sensitive`, both the pattern *source* and the subject are lowercased (so
/// `\D` becomes `\d`). The match is unanchored. A pattern that doesn't
/// compile never matches: opensnitchd refuses to load such a rule.
pub(super) fn regexp_cmp(subject: &str, pattern: &str, sensitive: bool) -> bool {
    let (subject, pattern) = if sensitive {
        (subject.to_string(), pattern.to_string())
    } else {
        (go_to_lower(subject), go_to_lower(pattern))
    };
    regex::Regex::new(&pattern).is_ok_and(|re| re.is_match(&subject))
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
    fn hash_cmp_is_exact_and_an_empty_subject_matches() {
        assert!(hash_cmp("deadbeef", "deadbeef"));
        assert!(!hash_cmp("deadbeef", "DEADBEEF"));
        assert!(hash_cmp("", "anything"));
    }
}
