//! The commit-id check `build.rs` applies before recording a commit. Kept in
//! its own file so `tests/build_identity.rs` can compile the same code.

/// A hex object id (SHA-1 or SHA-256 length); anything else is not recorded.
pub fn is_commit_id(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
