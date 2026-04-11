//! Glob → regex conversion for host operands.
//!
//! Supports the LS-style subset:
//!   * `*` matches any sequence of non-`.` characters within one label
//!   * `**` matches any sequence including `.`
//!   * literal `.` is escaped to `\.`
//!   * the result is anchored with `^` and `$`

use regex::Regex;

#[derive(Debug, thiserror::Error)]
pub enum GlobError {
    #[error("invalid regex produced: {0}")]
    InvalidRegex(#[from] regex::Error),
}

/// Convert an LS-style glob to an anchored regex string.
pub fn glob_to_regex_string(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len() * 2 + 2);
    out.push('^');

    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    out.push_str(".*");
                } else {
                    out.push_str("[^.]*");
                }
            }
            '.' => out.push_str("\\."),
            '?' => out.push_str("[^.]"),
            // Escape regex metacharacters
            '+' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }

    out.push('$');
    out
}

/// Convert and compile.
pub fn glob_to_regex(glob: &str) -> Result<Regex, GlobError> {
    Ok(Regex::new(&glob_to_regex_string(glob))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_host_round_trips() {
        assert_eq!(glob_to_regex_string("github.com"), r"^github\.com$");
        let re = glob_to_regex("github.com").unwrap();
        assert!(re.is_match("github.com"));
        assert!(!re.is_match("api.github.com"));
        assert!(!re.is_match("github.com.evil.com"));
    }

    #[test]
    fn star_matches_one_label() {
        let re = glob_to_regex("*.github.com").unwrap();
        assert!(re.is_match("api.github.com"));
        assert!(re.is_match("raw.github.com"));
        assert!(!re.is_match("github.com"), "no label means no match");
        assert!(!re.is_match("api.cdn.github.com"), "two labels need **");
    }

    #[test]
    fn double_star_matches_multiple_labels() {
        let re = glob_to_regex("**.github.com").unwrap();
        assert!(re.is_match("api.github.com"));
        assert!(re.is_match("api.cdn.github.com"));
        assert!(re.is_match(".github.com")); // edge case: empty subdomain
    }

    #[test]
    fn dots_are_escaped() {
        let re = glob_to_regex("a.b.c").unwrap();
        assert!(re.is_match("a.b.c"));
        assert!(!re.is_match("aXbXc"), "dots must be literal, not regex .");
    }

    #[test]
    fn metacharacters_are_escaped() {
        let re = glob_to_regex("a+b").unwrap();
        assert!(re.is_match("a+b"));
        assert!(!re.is_match("ab"));
    }

    #[test]
    fn question_mark_matches_one_non_dot_char() {
        let re = glob_to_regex("a?c.com").unwrap();
        assert!(re.is_match("abc.com"));
        assert!(!re.is_match("ac.com"));
        assert!(!re.is_match("a.c.com"));
    }
}
