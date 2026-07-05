//! `scanner-cli` — runs one userspace-tier security scan and prints the
//! report (baseline design §4's three buckets) as JSON to stdout.
//!
//! Usage:
//!   scanner-cli
//!
//! Env vars (all optional):
//!   HOME                            user whose surfaces are scanned
//!   XDG_STATE_HOME                  base for scans.db (default ~/.local/state)
//!   SNITCHWATCH_SCANNER_STATE_DIR   explicit dir for scans.db (overrides above)
//!
//! Exit status:
//!   0  scan completed with no new/still-outstanding anomalies
//!   1  scan completed but found actionable anomalies
//!   2  scan could not run (configuration/IO error)
//!
//! The scan never requires elevated privileges — this is the userspace
//! tier. The deep, privilege-justifying integrity pass is the separate
//! privileged tier (Phase 6).

use std::process::ExitCode;

use scanner_cli::{report_to_json, run, ScannerConfig};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = match ScannerConfig::from_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("scanner-cli: configuration error: {err:#}");
            return ExitCode::from(2);
        }
    };

    let report = match run(&config) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("scanner-cli: scan failed: {err:#}");
            return ExitCode::from(2);
        }
    };

    match report_to_json(&report) {
        Ok(json) => println!("{json}"),
        Err(err) => {
            eprintln!("scanner-cli: failed to render report: {err:#}");
            return ExitCode::from(2);
        }
    }

    if report.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
