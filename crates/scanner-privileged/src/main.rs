//! Privileged-tier scanner entrypoint.
//!
//! Invoked ON-DEMAND via polkit/pkexec (see
//! `packaging/polkit/org.snitchwatch.scanner.policy`). This process runs one
//! scan and exits. It is NOT a daemon: no listener, no timer, no background
//! work — it returns as soon as the ordered sub-checks finish.
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
    let pid = unsafe { libc::getpid() };
    if unsafe { libc::setpgid(0, 0) } != 0 && unsafe { libc::getpgrp() } != pid {
        return Err(std::io::Error::last_os_error().into());
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(540));
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    });
    Ok(())
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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

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
