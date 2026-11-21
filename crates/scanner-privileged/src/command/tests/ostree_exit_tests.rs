//! `ostree ls` absent-path allowance: a path missing from the commit is data,
//! anything else non-zero is a failure. Argv selection is tested through
//! `allowance`, the exit handling through `capture_allowing` fixtures.
use super::*;

const COMMIT: &str = "d39c44ff4c9cf2ef204e6e025429558ac2772758b62e458a2a7d068fdd793410";
const PATH: &str = "/usr/nope";
const GONE: &str = "error: Inspecting path '/usr/nope': No such file or directory: /usr/nope\\n";
const NOT_DIR: &str = "error: Inspecting path '/usr/nope': Not a directory\\n";

fn ls_allow() -> Allow {
    let mut args: Vec<&str> = OSTREE_LS_ARGS.to_vec();
    args.extend([COMMIT, PATH]);
    allowance("ostree", &args)
}

/// Shell command printing `text` (with `\n` escapes) to stderr.
fn say(text: &str) -> String {
    format!("printf '{}' >&2", text.replace('\'', "'\\''"))
}

fn verify(script: &str, allow: Allow) -> io::Result<String> {
    capture_allowing(
        Command::new("/usr/bin/sh").args(["-c", script]),
        Duration::from_millis(250),
        allow,
    )
}

#[test]
fn ostree_argv_constants_are_pinned() {
    assert_eq!(OSTREE_LS_ARGS, ["ls", "-d", "-C"]);
    assert!(OSTREE_LS_ARGS.contains(&"-d"));
    assert_eq!(OSTREE_CHECKSUM_ARGS, ["checksum"]);
}

#[test]
fn the_exact_ls_shape_with_the_pinned_stderr_is_data() {
    let _lock = serial();
    assert_eq!(ls_allow(), Allow::OstreeAbsent(PATH.to_string()));
    for text in [GONE, NOT_DIR] {
        let out = verify(&format!("{}; exit 1", say(text)), ls_allow()).unwrap();
        assert_eq!(out, OSTREE_ABSENT_MARKER);
    }
}

#[test]
fn exit_zero_with_empty_stdout_is_not_the_absent_marker() {
    let _lock = serial();
    assert_eq!(verify("exit 0", ls_allow()).unwrap(), "");
}

#[test]
fn the_same_exit_under_any_other_ostree_argv_is_an_error() {
    let _lock = serial();
    let other: [&[&str]; 5] = [
        &["ls", "-C", COMMIT, PATH],
        &["ls", "-d", "-C", "unknown", PATH],
        &["ls", "-d", "-C", COMMIT],
        &["ls", "-d", "-C", COMMIT, PATH, "extra"],
        &["checksum", PATH],
    ];
    for args in other {
        assert_eq!(allowance("ostree", args), Allow::Strict, "{args:?}");
        let script = format!("{}; exit 1", say(GONE));
        assert!(verify(&script, allowance("ostree", args)).is_err());
    }
    assert_eq!(
        allowance("rpm", &["ls", "-d", "-C", COMMIT, PATH]),
        Allow::Strict
    );
}

#[test]
fn a_different_stderr_or_any_stdout_is_an_error() {
    let _lock = serial();
    let wrong = "error: Inspecting path '/usr/nope': Permission denied\\n";
    for script in [
        format!("{}; exit 1", say(wrong)),
        "exit 1".to_string(),
        format!("{}; printf 'x'; exit 1", say(GONE)),
        format!("{}; exit 1", say(&format!("{GONE}x"))),
        format!("{}; exit 2", say(GONE)),
    ] {
        assert!(verify(&script, ls_allow()).is_err(), "{script}");
    }
}

#[test]
fn the_pinned_stderr_must_name_the_requested_path() {
    let _lock = serial();
    let other = "error: Inspecting path '/usr/other': No such file or directory: /usr/other\\n";
    let script = format!("{}; exit 1", say(other));
    assert!(verify(&script, ls_allow()).is_err());
}

#[test]
fn death_by_signal_is_an_error_even_with_the_absent_stderr() {
    let _lock = serial();
    let script = format!("{}; kill -9 $$", say(GONE));
    assert!(verify(&script, ls_allow()).is_err());
}
