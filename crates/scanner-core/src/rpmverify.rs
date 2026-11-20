//! Parsing of `rpm -V <pkg>` output for the layered-package file-integrity
//! check (baseline design §3 / classification rule 3).
//!
//! We reuse `rpm -V` rather than reimplementing a manifest-checksum database
//! (baseline design "Candidate approach evaluation"). `rpm -V` prints one
//! line *per file that fails verification*; files that verify cleanly are
//! not printed at all. Each failure line is:
//!
//! ```text
//! SM5DLUGTP c /etc/some.conf
//! missing     /usr/bin/gone
//! ```
//!
//! The first field is 9 verification attributes (`.` = ok, a letter = the
//! failing attribute), optionally followed by a file-type marker (`c`, `d`,
//! …), then the path. `missing` is a special leading token.

use std::path::Path;

use crate::classify::LayeredMatch;

/// Whether `rpm -V` output reports the given path as verifying cleanly.
///
/// Because `rpm -V` only emits failing files, a path that never appears in
/// the output verified cleanly. A path that appears (for any attribute, or
/// as `missing`) did not.
pub fn path_verifies_clean(verify_output: &str, path: &Path) -> bool {
    let target = path.to_string_lossy();
    !verify_output
        .lines()
        .any(|line| parse_verify_line(line).is_some_and(|p| p == target))
}

/// Convenience mapping to the classification input.
pub fn layered_match_for(verify_output: &str, path: &Path) -> LayeredMatch {
    if path_verifies_clean(verify_output, path) {
        LayeredMatch::Clean
    } else {
        LayeredMatch::Modified
    }
}

/// Parse one `rpm -V` line into a failure, or `None` for blank/unparseable
/// lines. Layout: an attribute field (9 attribute chars or `missing`), an
/// optional single-character file-type marker (`c`, `d`, `g`, `l`, `r`), then
/// the path, which is **everything after** those columns and may contain
/// spaces (a last-token parse would fail open on such a path).
fn parse_failure(line: &str) -> Option<VerifyFailure> {
    let rest = line.trim_start();
    let (attrs, rest) = rest.split_once(char::is_whitespace)?;
    let missing = attrs == "missing";
    if !missing && !is_attr_field(attrs) {
        return None;
    }
    let rest = rest.trim_start();
    let path = match rest.split_once(char::is_whitespace) {
        Some((marker, tail)) if marker.len() == 1 && !marker.starts_with('/') => tail.trim_start(),
        _ => rest,
    };
    path.starts_with('/').then(|| VerifyFailure {
        path: path.to_string(),
        attrs: attrs.to_string(),
        missing,
    })
}

/// The attribute field is 9 characters, each `.`, `?` or a letter/digit.
fn is_attr_field(s: &str) -> bool {
    s.len() == 9
        && s.chars()
            .all(|c| c == '.' || c == '?' || c.is_ascii_alphanumeric())
}

fn parse_verify_line(line: &str) -> Option<String> {
    parse_failure(line).map(|f| f.path)
}

/// One failing line of `rpm -V` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyFailure {
    pub path: String,
    pub attrs: String,
    pub missing: bool,
}

/// Parsed `rpm -V`/`rpm -Va` output: failing files plus the count of
/// non-blank lines that yielded no absolute path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub failures: Vec<VerifyFailure>,
    pub unparsed: usize,
}

/// Parse every failing entry of `rpm -V` output. Non-blank lines that do not
/// yield an absolute path are counted in `unparsed`, never dropped silently.
pub fn failing_entries(output: &str) -> VerifyReport {
    output.lines().filter(|l| !l.trim().is_empty()).fold(
        VerifyReport::default(),
        |mut acc, line| {
            match parse_failure(line) {
                Some(f) => acc.failures.push(f),
                None => acc.unparsed += 1,
            }
            acc
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn path_with_space_is_not_reported_clean() {
        let out = "S.5....T.  c /usr/share/a b.conf\n";
        assert!(!path_verifies_clean(
            out,
            &PathBuf::from("/usr/share/a b.conf")
        ));
    }

    #[test]
    fn failing_entries_keep_full_paths_and_missing_marker() {
        let out = "S.5....T.  c /etc/my app.conf\nmissing     /usr/bin/gone x\nmissing   c /etc/gone.conf\n..5......    /usr/lib/a b/c\n";
        let r = failing_entries(out);
        assert_eq!(r.unparsed, 0);
        let got: Vec<(&str, &str, bool)> = r
            .failures
            .iter()
            .map(|f| (f.path.as_str(), f.attrs.as_str(), f.missing))
            .collect();
        assert_eq!(
            got,
            vec![
                ("/etc/my app.conf", "S.5....T.", false),
                ("/usr/bin/gone x", "missing", true),
                ("/etc/gone.conf", "missing", true),
                ("/usr/lib/a b/c", "..5......", false),
            ]
        );
    }

    #[test]
    fn garbage_line_is_counted_not_returned() {
        let out = "error: rpmdb open failed\n\nS.5....T.    /usr/bin/x\nnot a verify line\n";
        let r = failing_entries(out);
        assert_eq!(r.failures.len(), 1);
        assert_eq!(r.unparsed, 2);
    }

    const SAMPLE: &str = "\
S.5....T.  c /etc/opensnitchd/default-config.json
.......T.    /usr/lib/opensnitch/rules
missing     /usr/bin/opensnitch-ui
";

    #[test]
    fn clean_path_not_in_output_verifies() {
        assert!(path_verifies_clean(
            SAMPLE,
            &PathBuf::from("/usr/bin/opensnitchd")
        ));
    }

    #[test]
    fn modified_config_does_not_verify() {
        let p = PathBuf::from("/etc/opensnitchd/default-config.json");
        assert!(!path_verifies_clean(SAMPLE, &p));
        assert_eq!(layered_match_for(SAMPLE, &p), LayeredMatch::Modified);
    }

    #[test]
    fn missing_file_does_not_verify() {
        let p = PathBuf::from("/usr/bin/opensnitch-ui");
        assert!(!path_verifies_clean(SAMPLE, &p));
        assert_eq!(layered_match_for(SAMPLE, &p), LayeredMatch::Modified);
    }

    #[test]
    fn empty_output_means_everything_clean() {
        assert!(path_verifies_clean("", &PathBuf::from("/usr/bin/anything")));
    }

    #[test]
    fn timestamp_only_diff_is_still_a_mismatch() {
        // A file listed for *any* attribute (here only mtime T) is modified.
        let p = PathBuf::from("/usr/lib/opensnitch/rules");
        assert_eq!(layered_match_for(SAMPLE, &p), LayeredMatch::Modified);
    }
}
