//! Privileged-tier scanner entrypoint.
//!
//! Invoked ON-DEMAND via polkit/pkexec (see
//! `packaging/polkit/org.snitchwatch.scanner.policy`). This process runs one
//! scan and exits. It has no listener or persistent service. A bounded watchdog
//! terminates this invocation and its check children if the scan hangs.
//!
//! Usage:
//!   snitchwatch-scanner-privileged [--db <path>] [--json]
//!
//! `--db` overrides the scans.db location (default:
//! `/var/lib/snitchwatch-scanner/scans.db`, which requires the elevated
//! access pkexec grants). Override is useful for a non-privileged manual/dev
//! run against a writable path.

use std::path::PathBuf;
use std::process::Command;

use scanner_privileged::facts::LiveSystem;
use scanner_privileged::{run_privileged_scan, ScanReport, ScanStore};

const DEFAULT_DB: &str = "/var/lib/snitchwatch-scanner/scans.db";
// Keep in sync with scanner-broker's watchdog termination contract.
const WATCHDOG_TIMEOUT_MARKER: &[u8] = b"\nSNITCHWATCH_SCANNER_WATCHDOG_TIMEOUT\n";

#[derive(Debug)]
struct Args {
    db: PathBuf,
    json: bool,
}

fn parse_args_from(args: impl IntoIterator<Item = String>, elevated: bool) -> anyhow::Result<Args> {
    let mut db = PathBuf::from(DEFAULT_DB);
    let mut json = false;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--db" => {
                anyhow::ensure!(
                    !elevated,
                    "--db is only supported for unprivileged development scans"
                );
                db = PathBuf::from(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--db requires a path"))?,
                );
            }
            "--json" => json = true,
            _ => anyhow::bail!("unknown scanner argument: {arg}"),
        }
    }
    Ok(Args { db, json })
}

/// A root scan cannot be killed by its unprivileged broker. Bound its entire
/// process group here as well, including hung checks that inherit that group.
fn start_privileged_watchdog() -> anyhow::Result<()> {
    start_privileged_watchdog_after(std::time::Duration::from_secs(540))
}

fn start_privileged_watchdog_after(timeout: std::time::Duration) -> anyhow::Result<()> {
    let pid = unsafe { libc::getpid() };
    if unsafe { libc::setpgid(0, 0) } != 0 && unsafe { libc::getpgrp() } != pid {
        return Err(std::io::Error::last_os_error().into());
    }
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        notify_watchdog_timeout();
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    });
    Ok(())
}

/// Best effort only: a blocked or broken diagnostic must never delay the kill.
/// Broker stderr is a pipe. Other destinations may have blocking I/O semantics,
/// so omit the marker there rather than weakening the watchdog's deadline.
fn notify_watchdog_timeout() {
    unsafe {
        let mut metadata: libc::stat = std::mem::zeroed();
        if libc::fstat(libc::STDERR_FILENO, &mut metadata) != 0
            || metadata.st_mode & libc::S_IFMT != libc::S_IFIFO
        {
            return;
        }
        let flags = libc::fcntl(libc::STDERR_FILENO, libc::F_GETFL);
        if flags < 0
            || libc::fcntl(libc::STDERR_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
        {
            return;
        }
        // Shorter than PIPE_BUF: this single nonblocking write either emits the
        // complete marker or fails. The unconditional group kill follows either way.
        let _ = libc::write(
            libc::STDERR_FILENO,
            WATCHDOG_TIMEOUT_MARKER.as_ptr().cast(),
            WATCHDOG_TIMEOUT_MARKER.len(),
        );
    }
}

/// Best-effort active-deployment checksum from `rpm-ostree status --json`.
/// Falls back to "unknown" when rpm-ostree is unavailable (e.g. in a sandbox
/// or non-ostree host) so the scan can still run and record results.
fn deployment_checksum() -> String {
    let output = match Command::new("rpm-ostree")
        .args(["status", "--json"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return "unknown".to_string(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("deployments")?
                .as_array()?
                .iter()
                .find(|d| d.get("booted").and_then(|b| b.as_bool()).unwrap_or(false))
                .or_else(|| v.get("deployments")?.as_array()?.first())
                .and_then(|d| d.get("checksum").and_then(|c| c.as_str()).map(String::from))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn print_report(report: &ScanReport) {
    println!(
        "=== snitchwatch privileged scan (scan_id={}) ===",
        report.scan_id
    );

    let r = &report.reconciled;
    println!("\nNew anomalies ({}):", r.new.len());
    for f in &r.new {
        println!("  [NEW]  {} — {}", f.path, f.detail);
    }
    println!("\nStill outstanding ({}):", r.still_outstanding.len());
    for f in &r.still_outstanding {
        println!("  [OPEN] {} — {}", f.path, f.detail);
    }
    println!("\nResolved since last scan ({}):", r.resolved.len());
    for f in &r.resolved {
        println!("  [GONE] {}", f.path);
    }

    println!("\nInformational ({}):", report.informational.len());
    for f in &report.informational {
        println!("  [INFO] {} — {}", f.path, f.detail);
    }

    println!("\nSkipped / unavailable checks ({}):", report.skipped.len());
    for s in &report.skipped {
        println!("  [SKIP] {} — {}", s.check, s.reason);
    }
}

fn print_json(report: &ScanReport) {
    let value = serde_json::json!({
        "scan_id": report.scan_id,
        "new": report.reconciled.new.iter().map(|f| serde_json::json!({"path": f.path, "detail": f.detail})).collect::<Vec<_>>(),
        "still_outstanding": report.reconciled.still_outstanding.iter().map(|f| serde_json::json!({"path": f.path, "detail": f.detail})).collect::<Vec<_>>(),
        "resolved": report.reconciled.resolved.iter().map(|f| serde_json::json!({"path": f.path})).collect::<Vec<_>>(),
        "informational": report.informational.iter().map(|f| serde_json::json!({"path": f.path, "detail": f.detail})).collect::<Vec<_>>(),
        "skipped": report.skipped.iter().map(|s| serde_json::json!({"check": s.check, "reason": s.reason})).collect::<Vec<_>>(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn main() -> anyhow::Result<()> {
    init_logging();
    let elevated = unsafe { libc::geteuid() } == 0;
    let args = parse_args_from(std::env::args().skip(1), elevated)?;
    if elevated {
        start_privileged_watchdog()?;
    }

    if let Some(parent) = args.db.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let store = ScanStore::open(&args.db)?;
    let facts = LiveSystem::new();
    let checksum = deployment_checksum();

    tracing::info!(db = %args.db.display(), deployment = %checksum, "starting privileged scan");
    let report = run_privileged_scan(&facts, &store, &checksum)?;

    if args.json {
        print_json(&report);
    } else {
        print_report(&report);
    }

    // Exit non-zero if any new anomaly was found, so a caller (or the report
    // UI's invocation wrapper) can react without parsing output.
    if report.reconciled.new.is_empty() {
        Ok(())
    } else {
        std::process::exit(2);
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    #[test]
    fn watchdog_kills_its_group_with_a_bounded_best_effort_diagnostic() {
        use std::os::unix::process::ExitStatusExt;
        use std::time::Duration;
        const CHILD: &str = "SNITCHWATCH_TEST_WATCHDOG";
        if let Some(mode) = std::env::var_os(CHILD) {
            if mode == "full" {
                unsafe {
                    let mut pipe = [0; 2];
                    assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK), 0);
                    let bytes = [b'x'; 4096];
                    while libc::write(pipe[1], bytes.as_ptr().cast(), bytes.len()) > 0 {}
                    assert_eq!(
                        libc::dup2(pipe[1], libc::STDERR_FILENO),
                        libc::STDERR_FILENO
                    );
                    // Keep the read end alive without draining it.
                }
            } else if mode == "closed" {
                unsafe {
                    libc::close(libc::STDERR_FILENO);
                }
            }
            start_privileged_watchdog_after(Duration::from_millis(50)).unwrap();
            // This check child inherits the scanner's group and both pipes. A
            // surviving check would prevent wait_with_output from completing.
            let mut check = Command::new("sleep").arg("20").spawn().unwrap();
            let _ = check.wait();
            panic!("watchdog did not terminate the fixture");
        }
        for mode in ["normal", "full", "closed"] {
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "argument_tests::watchdog_kills_its_group_with_a_bounded_best_effort_diagnostic", "--nocapture"])
                .env(CHILD, mode)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn().unwrap();
            let pid = child.id() as i32;
            let (finished, result) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = finished.send(child.wait_with_output());
            });
            let output = match result.recv_timeout(Duration::from_secs(2)) {
                Ok(output) => output.unwrap(),
                Err(_) => {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                        libc::kill(pid, libc::SIGKILL);
                    }
                    let _ = result.recv_timeout(Duration::from_secs(2));
                    panic!("watchdog or a check child blocked with {mode} diagnostic pipe");
                }
            };
            assert_eq!(output.status.signal(), Some(libc::SIGKILL), "{mode}");
            if mode == "normal" {
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .any(|line| line == "SNITCHWATCH_SCANNER_WATCHDOG_TIMEOUT"),
                    "watchdog must identify its termination"
                );
            }
        }
    }
    #[test]
    fn json_stdout_contains_only_the_actual_producer_report() {
        const CHILD: &str = "SNITCHWATCH_TEST_JSON_PRODUCER";
        const MARKER: &str = "scanner producer output starts here\n";
        if std::env::var_os(CHILD).is_some() {
            // Separate libtest's preamble from the actual producer output. Exit
            // directly afterwards so libtest cannot append its own result text.
            use std::io::Write;
            print!("{MARKER}");
            std::io::stdout().flush().unwrap();
            init_logging();
            tracing::info!("starting privileged scan");
            tracing::warn!("fixture warning");
            tracing::error!("fixture error");
            print_json(&ScanReport {
                scan_id: 42,
                ..ScanReport::default()
            });
            std::io::stdout().flush().unwrap();
            std::process::exit(0);
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "argument_tests::json_stdout_contains_only_the_actual_producer_report",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let actual = stdout.split_once(MARKER).unwrap().1;
        let report: serde_json::Value = serde_json::from_str(actual)
            .unwrap_or_else(|error| panic!("JSON stdout contains diagnostics: {error}: {actual}"));
        assert_eq!(report["scan_id"], 42);
        for key in [
            "new",
            "still_outstanding",
            "resolved",
            "informational",
            "skipped",
        ] {
            assert!(report[key].is_array(), "missing report field {key}");
        }
        let stderr = String::from_utf8(output.stderr).unwrap();
        for message in [
            "starting privileged scan",
            "fixture warning",
            "fixture error",
        ] {
            assert!(
                stderr.contains(message),
                "missing diagnostic {message}: {stderr}"
            );
        }
    }
    #[test]
    fn elevated_scan_cannot_choose_its_write_path() {
        let args = ["--db", "/tmp/caller-selected.db", "--json"].map(String::from);
        assert!(parse_args_from(args.clone(), true)
            .unwrap_err()
            .to_string()
            .contains("unprivileged"));
        assert_eq!(
            parse_args_from(args, false).unwrap().db,
            PathBuf::from("/tmp/caller-selected.db")
        );
        let fixed = parse_args_from(["--json".into()], true).unwrap();
        assert_eq!(fixed.db, PathBuf::from(DEFAULT_DB));
        assert!(fixed.json);
    }
    #[test]
    fn invalid_arguments_are_rejected_before_any_scan() {
        assert!(parse_args_from(["--db".into()], false).is_err());
        assert!(parse_args_from(["--anything".into()], true).is_err());
    }
}
