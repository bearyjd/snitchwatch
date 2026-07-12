//! Invoke Component B's on-demand privileged scanner via `pkexec` and hand
//! back its raw JSON report.
//!
//! Ported as a thin wrapper, not a reimplementation: `scanner-privileged`
//! already produces exactly the structured output this shell needs via its
//! `--json` flag (see `crates/scanner-privileged/src/main.rs::print_json`).
//! This module's only job is resolving `pkexec`/the scanner binary and
//! surfacing failures as strings a human can read, never a panic — running a
//! deep scan is optional and on-demand by design (privileged-tier spec §3).

/// Matches the `org.freedesktop.policykit.exec.path` annotation in
/// `packaging/polkit/org.snitchwatch.scanner.policy`. Overridable via
/// `SNITCHWATCH_SCANNER_BIN` for dev/manual runs against a non-packaged
/// build, the same override-env-var convention `bridge_runtime.rs` uses for
/// `SNITCHWATCH_GRPC_BIND`.
const DEFAULT_SCANNER_BIN: &str = "/usr/libexec/snitchwatch-scanner-privileged";

fn scanner_binary_path() -> String {
    std::env::var("SNITCHWATCH_SCANNER_BIN").unwrap_or_else(|_| DEFAULT_SCANNER_BIN.to_string())
}

/// Run one privileged deep scan and return its `--json` stdout as a raw
/// string. Exit code 2 (per the scanner's own contract: "new anomalies
/// found") is treated as success too — it's still well-formed JSON, just
/// carrying a non-empty `new` bucket.
pub fn run_deep_scan() -> Result<String, String> {
    let pkexec = which::which("pkexec").map_err(|e| format!("pkexec not found: {e}"))?;

    let output = std::process::Command::new(pkexec)
        .arg(scanner_binary_path())
        .arg("--json")
        .output()
        .map_err(|e| format!("failed to run scanner via pkexec: {e}"))?;

    match output.status.code() {
        Some(0) | Some(2) => {
            String::from_utf8(output.stdout).map_err(|e| format!("scanner output not UTF-8: {e}"))
        }
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "scanner exited with {}: {}",
                output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr.trim()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both cases live in one test (rather than two `#[test]` fns) because
    // Rust runs tests in parallel within a binary and `SNITCHWATCH_SCANNER_BIN`
    // is process-global state — two tests mutating it concurrently would race.
    #[test]
    fn scanner_binary_path_defaults_and_honors_override() {
        std::env::remove_var("SNITCHWATCH_SCANNER_BIN");
        assert_eq!(scanner_binary_path(), DEFAULT_SCANNER_BIN);

        std::env::set_var("SNITCHWATCH_SCANNER_BIN", "/tmp/fake-scanner");
        assert_eq!(scanner_binary_path(), "/tmp/fake-scanner");
        std::env::remove_var("SNITCHWATCH_SCANNER_BIN");
    }

    #[test]
    fn run_deep_scan_never_panics_when_pkexec_or_binary_absent() {
        // This sandbox has no polkit daemon and (usually) no pkexec at all —
        // exactly the "gracefully degrade" path this function exists for.
        let result = run_deep_scan();
        if let Err(e) = result {
            assert!(!e.is_empty());
        }
    }
}
