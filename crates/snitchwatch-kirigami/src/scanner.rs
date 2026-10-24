//! Native builds invoke pkexec; the Flatpak uses the constrained host broker.
use std::path::Path;

#[derive(Debug)]
pub struct ScanError {
    pub title: &'static str,
    pub detail: String,
}

fn interpret((code, detail): scanner_broker::Reply) -> Result<String, ScanError> {
    if code == "ok" {
        return Ok(detail);
    }
    let title = match code.as_str() {
        "unavailable" => "Host scanner setup is required.",
        "denied" => "The deep scan was not authorized.",
        "busy" => "A deep scan is already running.",
        "timeout" => "The deep scan timed out.",
        _ => "The deep scan failed.",
    };
    Err(ScanError { title, detail })
}

pub fn run_deep_scan() -> Result<String, ScanError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ScanError {
            title: "The deep scan failed.",
            detail: e.to_string(),
        })?;
    let result = runtime.block_on(async {
        if Path::new("/.flatpak-info").exists() {
            scanner_broker::request_scan().await
        } else {
            let scanner = std::env::var("SNITCHWATCH_SCANNER_BIN")
                .unwrap_or_else(|_| scanner_broker::SCANNER.into());
            scanner_broker::run_native(Path::new(&scanner)).await
        }
    });
    interpret(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_denial_busy_timeout_and_execution_errors_have_distinct_titles() {
        for (code, title) in [
            ("unavailable", "Host scanner setup is required."),
            ("denied", "The deep scan was not authorized."),
            ("busy", "A deep scan is already running."),
            ("timeout", "The deep scan timed out."),
            ("failed", "The deep scan failed."),
            ("unknown-future-status", "The deep scan failed."),
        ] {
            let err = interpret((code.into(), "detail".into())).unwrap_err();
            assert_eq!(err.title, title);
            assert_eq!(err.detail, "detail");
        }
        assert_eq!(interpret(("ok".into(), "report".into())).unwrap(), "report");
    }
}
