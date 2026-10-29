//! The commit `build.rs` records must be a plain hex object id: it ends up in
//! a `cargo:rustc-env` line and in the About report, so anything else (a
//! newline, a stray word, an empty string) has to be refused.
#[path = "../build_commit.rs"]
mod build_commit;

use build_commit::{is_commit_id, is_directive_safe_path};

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

#[test]
fn directive_paths_refuse_control_characters() {
    assert!(is_directive_safe_path("/home/u/repo/.git/HEAD"));
    assert!(is_directive_safe_path("/home/u/my repo/.git/HEAD"));
    assert!(!is_directive_safe_path(""));
    assert!(!is_directive_safe_path("/r/.git/HEAD\ncargo:rustc-cfg=x"));
    assert!(!is_directive_safe_path("/r/.git/HEAD\rcargo:x"));
    assert!(!is_directive_safe_path("/r/\u{7}/HEAD"));
}
