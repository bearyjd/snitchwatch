//! Real application worker + production parent IPC, with short test deadlines.
//! Include the Qt-free IPC module to reach its private command/deadline seam;
//! production always uses current_exe, the fixed flag, and the full limits.
use std::process::Command;
use std::time::{Duration, Instant};

#[path = "../src/geo/install/test_fixtures.rs"]
mod fixtures;
#[allow(dead_code)]
#[path = "../src/geo/install/worker.rs"]
mod worker;

// Tiny input, exponentially repeated unknown-field skips. The depth is below
// maxminddb's recursion guard, both in metadata and in Country deserialization.
fn skipped_dag_fixture(metadata: bool) -> Vec<u8> {
    let mut bytes = fixtures::fixture("DBIP-Country-Lite");
    let marker = bytes
        .windows(14)
        .position(|p| p == b"\xab\xcd\xefMaxMind.com")
        .unwrap();
    let start = if metadata { marker + 14 } else { 22 };
    let end = if metadata { bytes.len() } else { marker };
    bytes[start] += 1;
    let mut graph = vec![0x47];
    graph.extend_from_slice(b"unknown");
    let root = end - start + graph.len();
    for level in 0..40 {
        graph.extend([2, 4]); // array of two values
        let next = root + (level + 1) * 6;
        for _ in 0..2 {
            graph.extend([0x20 | (next >> 8) as u8, next as u8]);
        }
    }
    graph.push(0x40); // empty string leaf
    bytes.splice(end..end, graph);
    bytes
}

fn application_worker(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_snitchwatch-kirigami"));
    command
        .arg(worker::FLAG)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        // Any accidental Qt initialization must fail this probe immediately.
        .env("QT_QPA_PLATFORM", "snitchwatch-worker-must-not-start-qt")
        .env_remove("SNITCHWATCH_DEMO");
    command
}

#[test]
fn real_binary_protocol_accepts_country_types_before_any_application_startup() {
    let home = tempfile::tempdir().unwrap();
    for kind in ["DBIP-Country-Lite", "GeoLite2-Country", "GeoIP2-Country"] {
        worker::validate_with_command(
            &fixtures::fixture(kind),
            application_worker(home.path()),
            Duration::from_secs(5),
        )
        .unwrap();
    }
    for bytes in [
        b"not a database".to_vec(),
        fixtures::fixture("DBIP-City-Lite"),
    ] {
        assert!(worker::validate_with_command(
            &bytes,
            application_worker(home.path()),
            Duration::from_secs(5)
        )
        .is_err());
    }
    let mut extra_args = application_worker(home.path());
    extra_args.arg("/must/not/be/opened");
    assert!(worker::validate_with_command(b"", extra_args, Duration::from_secs(5)).is_err());
    assert_eq!(
        std::fs::read_dir(home.path()).unwrap().count(),
        0,
        "worker initialized application state"
    );
}

#[test]
fn skipped_metadata_and_country_dags_hit_the_parent_deadline_and_are_reaped() {
    let home = tempfile::tempdir().unwrap();
    for metadata in [true, false] {
        let started = Instant::now();
        let error = worker::validate_with_command(
            &skipped_dag_fixture(metadata),
            application_worker(home.path()),
            Duration::from_millis(150),
        )
        .unwrap_err();
        assert!(error.contains("resource limit"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        // The fixture's depth is below the library's guard. Its parser is
        // killed at the short parent deadline; the test never runs it freely.
    }
}

#[test]
fn inability_to_apply_resource_limits_rejects_before_parsing() {
    use std::os::unix::process::CommandExt;
    let home = tempfile::tempdir().unwrap();
    let mut command = application_worker(home.path());
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            if libc::setrlimit(libc::RLIMIT_CPU, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let error = worker::validate_with_command(b"", command, Duration::from_secs(5)).unwrap_err();
    assert!(error.contains("Could not apply"), "{error}");
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

#[test]
fn missing_crashed_noisy_and_invalid_workers_fail_closed() {
    let commands = [
        Command::new("/nonexistent/snitchwatch-validator"),
        shell("kill -KILL $$"),
        shell("printf '{\"Ok\":null}'; printf unexpected >&2"),
        shell("printf '{\"Ok\":null}garbage'"),
        shell("printf '{\"Ok\":null}'; head -c 9000 /dev/zero"),
        shell("printf '{\"Ok\":null}'; head -c 9000 /dev/zero >&2"),
        shell("exit 0"),
    ];
    for command in commands {
        assert!(worker::validate_with_command(b"", command, Duration::from_secs(2)).is_err());
    }
    assert!(worker::validate_with_command(
        &vec![0; 32 * 1024 * 1024 + 1],
        Command::new("/nonexistent/never-spawned"),
        Duration::from_secs(2)
    )
    .unwrap_err()
    .contains("32 MiB"));
}

#[test]
fn blocked_input_and_timeout_kill_and_reap_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let pidfile = directory.path().join("pid");
    let mut command = shell("printf '%s' $$ > \"$1\"; exec sleep 30");
    command.arg("worker-test").arg(&pidfile);
    let started = Instant::now();
    let error =
        worker::validate_with_command(&vec![0; 1024 * 1024], command, Duration::from_millis(150))
            .unwrap_err();
    assert!(error.contains("resource limit"));
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid: i32 = std::fs::read_to_string(pidfile).unwrap().parse().unwrap();
    // SAFETY: signal zero checks existence only. ESRCH also proves no zombie.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
#[ignore = "requires the separately downloaded, hash-verified official October 2026 MMDB"]
fn official_dbip_country_passes_the_real_isolated_validator() {
    let file = std::env::var_os("SNITCHWATCH_DBIP_PROOF").expect("set SNITCHWATCH_DBIP_PROOF");
    let bytes = std::fs::read(file).unwrap();
    assert_eq!(bytes.len(), 8_348_509);
    let home = tempfile::tempdir().unwrap();
    worker::validate_with_command(
        &bytes,
        application_worker(home.path()),
        Duration::from_secs(60),
    )
    .unwrap();
}
