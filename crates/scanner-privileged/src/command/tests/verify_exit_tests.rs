//! `rpm -Va` exit allowance: drift is data (a non-zero exit with parseable
//! verify lines), a failed rpm is not. The argv selection is tested through
//! `allowance`, the exit handling through `capture_allowing` fixtures.
use super::*;

const DRIFT: &str = "S.5....T.  c /etc/foo.conf\\n";

fn verify(script: &str, allow: Allow) -> io::Result<String> {
    capture_allowing(
        Command::new("/usr/bin/sh").args(["-c", script]),
        Duration::from_millis(250),
        allow,
    )
}

fn selected_for_verify() -> Allow {
    allowance("rpm", &RPM_VERIFY_ARGS)
}

#[test]
fn rpm_verify_args_are_pinned_and_noscripts() {
    assert_eq!(
        RPM_VERIFY_ARGS,
        ["-Va", "--nomtime", "--noscripts", "--nodeps"]
    );
    assert!(RPM_VERIFY_ARGS.contains(&"--noscripts"));
}

#[test]
fn drift_exit_codes_one_and_three_are_data() {
    let _lock = serial();
    for code in [1, 3] {
        let script = format!("printf '{DRIFT}'; exit {code}");
        let out = verify(&script, selected_for_verify()).unwrap();
        assert_eq!(out, "S.5....T.  c /etc/foo.conf\n");
    }
}

#[test]
fn only_the_exact_verify_argv_gets_the_allowance() {
    assert_eq!(selected_for_verify(), Allow::VerifyDrift);
    for args in [
        &["-Va"][..],
        &["-Va", "--nomtime", "--nodeps"],
        &["-Va", "--nomtime", "--noscripts", "--nodeps", "x"],
        &["-V", "bash"],
    ] {
        assert_eq!(allowance("rpm", args), Allow::Strict);
    }
    assert_eq!(allowance("chkrootkit", &RPM_VERIFY_ARGS), Allow::Strict);
    let _lock = serial();
    let script = format!("printf '{DRIFT}'; exit 1");
    assert!(verify(&script, allowance("rpm", &["-V", "bash"])).is_err());
}

#[test]
fn rpmdb_failure_with_no_verify_line_is_an_error() {
    let _lock = serial();
    let script = "echo 'error: rpmdb open failed' >&2; exit 1";
    assert!(verify(script, selected_for_verify()).is_err());
    let garbage = "printf 'not a verify line\\n'; exit 1";
    assert!(verify(garbage, selected_for_verify()).is_err());
}

#[test]
fn death_by_signal_is_an_error_even_with_drift_output() {
    let _lock = serial();
    let script = format!("printf '{DRIFT}'; kill -9 $$");
    assert!(verify(&script, selected_for_verify()).is_err());
}

#[test]
fn exit_zero_is_ok_and_a_stray_exit_without_drift_on_other_argv_is_not() {
    let _lock = serial();
    assert_eq!(verify("exit 0", selected_for_verify()).unwrap(), "");
    assert!(verify("exit 1", Allow::Strict).is_err());
}

#[test]
fn owner_query_not_owned_is_data_for_both_argv_shapes() {
    let _lock = serial();
    let miss = "file /x is not owned by any package\\n";
    let script = format!("printf '{miss}'; exit 1");
    for args in [
        &["-qf", "/x"][..],
        &["-qf", "--queryformat", OWNER_QUERYFORMAT, "/x"],
    ] {
        let out = verify(&script, allowance("rpm", args)).unwrap();
        assert_eq!(out, "file /x is not owned by any package\n");
    }
    let other = allowance("rpm", &["-qf", "--queryformat", "%{VERSION}", "/x"]);
    assert_eq!(other, Allow::Strict);
}

#[test]
fn partial_verify_with_stderr_errors_is_not_accepted_as_a_full_result() {
    let _lock = serial();
    let script = format!("printf '{DRIFT}'; echo 'error: cannot open Packages index' >&2; exit 1");
    assert!(verify(&script, selected_for_verify()).is_err());
}
