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

/// Extract the path component from one `rpm -V` line, or `None` for blank /
/// unparseable lines. The path is the last whitespace-delimited token —
/// rpm-verify paths are absolute and don't contain spaces in practice, and
/// the leading columns (attributes + optional file-type char, or the
/// `missing`/`prelink` tokens) never do.
fn parse_verify_line(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let last = line.split_whitespace().next_back()?;
    // A well-formed rpm -V line's path is absolute.
    if last.starts_with('/') {
        Some(last)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
