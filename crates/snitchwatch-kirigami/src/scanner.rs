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
    scanner_binary_path_from(std::env::var("SNITCHWATCH_SCANNER_BIN").ok())
}

/// Pure core of [`scanner_binary_path`]: the override comes in as a
/// parameter, so tests never touch the process environment (issue #97).
fn scanner_binary_path_from(override_bin: Option<String>) -> String {
    override_bin.unwrap_or_else(|| DEFAULT_SCANNER_BIN.to_string())
}

/// Run one privileged deep scan and return its `--json` stdout as a raw
/// string. Exit code 2 (per the scanner's own contract: "new anomalies
/// found") is treated as success too — it's still well-formed JSON, just
/// carrying a non-empty `new` bucket.
pub fn run_deep_scan() -> Result<String, String> {
    run_deep_scan_with(scanner_binary_path())
}

/// [`run_deep_scan`] for the scanner at `scanner_bin`.
fn run_deep_scan_with(scanner_bin: String) -> Result<String, String> {
    let pkexec = which::which("pkexec").map_err(|e| format!("pkexec not found: {e}"))?;

    // Preflight the scanner binary itself. Without this, a dev build (where
    // nothing is installed at the packaged /usr/libexec path) fails with a
    // bare pkexec exit 127 *after* the polkit authentication prompt, and
    // nothing tells the user the override env var exists.
    if !std::path::Path::new(&scanner_bin).is_file() {
        return Err(format!(
            "scanner binary not found at {scanner_bin} — install the \
             snitchwatch-scanner package, or point SNITCHWATCH_SCANNER_BIN \
             at a locally built scanner-privileged binary"
        ));
    }

    let output = std::process::Command::new(pkexec)
        .arg(scanner_bin)
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

    // These never call `std::env::set_var`/`remove_var`: the environment is
    // process-global and tests run on parallel threads (issue #97).
    #[test]
    fn scanner_binary_path_defaults_and_honors_override() {
        assert_eq!(scanner_binary_path_from(None), DEFAULT_SCANNER_BIN);
        assert_eq!(
            scanner_binary_path_from(Some("/opt/dev/scanner-privileged".into())),
            "/opt/dev/scanner-privileged"
        );
    }

    #[test]
    fn a_missing_scanner_is_named_before_any_polkit_prompt() {
        // The error names the path and the override env var, *before* any
        // pkexec/polkit prompt — unless pkexec itself is missing, which
        // legitimately short-circuits first.
        let missing = "/nonexistent/snitchwatch-fake-scanner".to_string();
        let err = run_deep_scan_with(missing).unwrap_err();
        if which::which("pkexec").is_ok() {
            assert!(
                err.contains("/nonexistent/snitchwatch-fake-scanner"),
                "{err}"
            );
            assert!(err.contains("SNITCHWATCH_SCANNER_BIN"), "{err}");
        } else {
            assert!(err.contains("pkexec not found"), "{err}");
        }
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
