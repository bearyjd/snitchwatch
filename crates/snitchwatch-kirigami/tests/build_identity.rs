//! The commit `build.rs` records must be a plain hex object id: it ends up in
//! a `cargo:rustc-env` line and in the About report, so anything else (a
//! newline, a stray word, an empty string) has to be refused.
#[path = "../build_commit.rs"]
mod build_commit;

use build_commit::is_commit_id;

#[test]
fn accepts_sha1_and_sha256_hex_ids() {
    assert!(is_commit_id("aaaa111"));
    assert!(is_commit_id(&"a".repeat(40)));
    assert!(is_commit_id(&"F".repeat(64)));
}

#[test]
fn refuses_everything_else() {
    assert!(!is_commit_id(""));
    assert!(!is_commit_id("abc123"), "too short");
    assert!(!is_commit_id(&"a".repeat(65)), "too long");
    assert!(!is_commit_id("abcdefg"), "non-hex letter");
    assert!(
        !is_commit_id("abcdef1\ncargo:rustc-cfg=x"),
        "newline injection"
    );
    assert!(!is_commit_id(" abcdef1"), "whitespace");
}
