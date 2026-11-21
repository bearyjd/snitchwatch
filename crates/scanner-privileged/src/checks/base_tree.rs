//! Pure parsing for the base-tree primitive (baseline spec §1 rule 2):
//! `ostree ls -d -C <commit> <path>` (what the commit holds) and
//! `ostree checksum <path>` (what is on disk). No I/O here; everything an
//! `ostree` child printed is untrusted and fails closed.

use crate::error::{Result, ScanError};

/// What the base commit holds at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// A commit entry and the checksum to compare with `ostree checksum <path>`:
/// the content object for files and symlinks, the dirmeta object for a
/// directory (its dirtree checksum would never match an on-disk read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitEntry {
    pub kind: EntryKind,
    pub checksum: String,
}

/// 64 lowercase hex digits: an OSTree commit or object checksum.
pub fn is_commit_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parse the one line `ostree ls -d -C <commit> <requested>` prints:
/// `MODE UID GID SIZE CHECKSUM [DIRMETA] PATH[ -> TARGET]`. The path must be
/// exactly the requested one, so a child listing, a path split by a newline or
/// a spoofed entry is rejected rather than compared.
pub fn parse_ls_entry(stdout: &str, requested: &str) -> Result<CommitEntry> {
    let line = stdout.strip_suffix('\n').unwrap_or(stdout);
    if line.contains('\n') {
        return Err(invalid(requested, "ostree ls printed more than one line"));
    }
    let (mode, rest) = next_field(line).ok_or_else(|| invalid(requested, "empty ls line"))?;
    let kind = match mode.chars().next() {
        Some('-') => EntryKind::File,
        Some('d') => EntryKind::Dir,
        Some('l') => EntryKind::Symlink,
        Some(_) | None => return Err(invalid(requested, "unsupported entry kind")),
    };
    let mut rest = rest;
    for _ in 0..3 {
        rest = next_field(rest)
            .ok_or_else(|| invalid(requested, "truncated ls line"))?
            .1;
    }
    let (first, mut rest) =
        next_field(rest).ok_or_else(|| invalid(requested, "missing checksum"))?;
    let mut checksum = first;
    if kind == EntryKind::Dir {
        let (meta, after) = next_field(rest).ok_or_else(|| invalid(requested, "no dirmeta"))?;
        checksum = meta;
        rest = after;
    }
    if !is_commit_hex(checksum) {
        return Err(invalid(requested, "checksum is not 64 lowercase hex"));
    }
    let path_ok = match kind {
        EntryKind::File | EntryKind::Dir => rest == requested,
        EntryKind::Symlink => rest
            .strip_prefix(requested)
            .and_then(|t| t.strip_prefix(" -> "))
            .is_some_and(|target| !target.is_empty()),
    };
    if !path_ok {
        return Err(invalid(requested, "ls entry is not the requested path"));
    }
    Ok(CommitEntry {
        kind,
        checksum: checksum.to_string(),
    })
}

/// Parse `ostree checksum <path>` output: exactly one 64-hex token.
pub fn parse_checksum(stdout: &str) -> Result<String> {
    let token = stdout.strip_suffix('\n').unwrap_or(stdout);
    if is_commit_hex(token) {
        Ok(token.to_string())
    } else {
        Err(invalid(
            "ostree checksum",
            "output is not one 64-hex checksum",
        ))
    }
}

/// Split off the first space-separated field (runs of spaces are one
/// separator, as `ostree ls` right-aligns the size column); the remainder
/// keeps its interior spaces.
fn next_field(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start_matches(' ');
    if text.is_empty() {
        return None;
    }
    let end = text.find(' ').unwrap_or(text.len());
    Some((&text[..end], text[end..].trim_start_matches(' ')))
}

fn invalid(what: &str, why: &str) -> ScanError {
    ScanError::Io {
        path: what.to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "9b88d8cc9a6c591204bc4a12dd85488122834f756cc02f24ca3a1ddc5d49273f";
    const B: &str = "989b825aa7d0db2e775b8cd6f45719fb557ebb5916a93b6d963b2577bb0e6279";

    fn file(path: &str) -> String {
        format!("-00644 1000 1000      3 {A} {path}\n")
    }

    #[test]
    fn a_regular_file_line_yields_file_and_its_checksum() {
        let e = parse_ls_entry(&file("/usr/bin/a"), "/usr/bin/a").unwrap();
        assert_eq!(e.kind, EntryKind::File);
        assert_eq!(e.checksum, A);
    }

    #[test]
    fn a_directory_line_yields_the_second_checksum_dirmeta() {
        let line = format!("d00755 1000 1000      0 {A} {B} /usr/bin\n");
        let e = parse_ls_entry(&line, "/usr/bin").unwrap();
        assert_eq!(e.kind, EntryKind::Dir);
        assert_eq!(e.checksum, B);
    }

    #[test]
    fn a_symlink_line_yields_the_first_checksum_and_needs_the_target_arrow() {
        let line = format!("l00777 1000 1000      0 {A} /usr/lnk -> x\n");
        let e = parse_ls_entry(&line, "/usr/lnk").unwrap();
        assert_eq!(e.kind, EntryKind::Symlink);
        assert_eq!(e.checksum, A);
        let bare = format!("l00777 1000 1000      0 {A} /usr/lnk\n");
        assert!(parse_ls_entry(&bare, "/usr/lnk").is_err());
    }

    #[test]
    fn a_path_with_spaces_round_trips() {
        let p = "/usr/share/a b  c/d e";
        let e = parse_ls_entry(&file(p), p).unwrap();
        assert_eq!(e.checksum, A);
    }

    #[test]
    fn a_first_line_naming_another_path_is_rejected() {
        // `ostree ls` without -d lists a directory's children.
        assert!(parse_ls_entry(&file("/usr/bin/a"), "/usr/bin").is_err());
        assert!(parse_ls_entry(&file("/usr/bin/ab"), "/usr/bin/a").is_err());
    }

    #[test]
    fn an_extra_line_is_rejected() {
        let two = format!("{}{}", file("/usr/bin"), file("/usr/bin/b"));
        assert!(parse_ls_entry(&two, "/usr/bin").is_err());
    }

    #[test]
    fn a_malformed_checksum_is_rejected() {
        let short = &A[..63];
        let upper = A.to_uppercase();
        let non_hex = format!("{}g", &A[..63]);
        for bad in [short, upper.as_str(), non_hex.as_str()] {
            let line = format!("-00644 1000 1000      3 {bad} /usr/bin/a\n");
            assert!(parse_ls_entry(&line, "/usr/bin/a").is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_and_unknown_kinds_are_rejected() {
        assert!(parse_ls_entry("", "/usr/bin/a").is_err());
        let fifo = format!("p00644 1000 1000      3 {A} /usr/bin/a\n");
        assert!(parse_ls_entry(&fifo, "/usr/bin/a").is_err());
    }

    #[test]
    fn checksum_output_is_exactly_one_hex_token() {
        assert_eq!(parse_checksum(&format!("{A}\n")).unwrap(), A);
        assert!(parse_checksum(&format!("{A} {B}\n")).is_err());
        assert!(parse_checksum(&format!("{A}\n{B}\n")).is_err());
        assert!(parse_checksum("").is_err());
        assert!(parse_checksum(&format!("{}\n", &A[..63])).is_err());
    }

    #[test]
    fn only_lowercase_64_hex_is_a_commit() {
        assert!(!is_commit_hex("unknown"));
        assert!(!is_commit_hex("abc"));
        assert!(!is_commit_hex(&A.to_uppercase()));
        assert!(is_commit_hex(A));
    }
}
