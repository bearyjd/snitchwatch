//! Native builds invoke pkexec; the Flatpak uses the constrained host broker.
use std::path::Path;

#[derive(Debug)]
pub struct ScanError {
    pub title: &'static str,
    pub detail: String,
}

fn interpret((code, detail): scanner_broker::Reply) -> Result<String, ScanError> {
    tracing::info!(reply_code = %code, detail_bytes = detail.len(), "deep scan broker reply");
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

/// How a finished scan reads to the user; drives the summary banner's look.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SummaryKind {
    Clean,
    Findings,
    Error,
}

impl SummaryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Findings => "findings",
            Self::Error => "error",
        }
    }
}

/// Current local time as `HH:MM`, via `localtime_r` (no chrono dependency).
pub fn local_hhmm() -> String {
    // SAFETY: `time`/`localtime_r` write only to the locals passed in.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return "--:--".into();
        }
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// One-line outcome of a scan attempt. Counts only: never report contents.
pub fn summarize(result: &Result<String, ScanError>, hhmm: &str) -> (SummaryKind, String) {
    let report = match result {
        Ok(json) => json,
        Err(e) => {
            return (
                SummaryKind::Error,
                format!("Last scan: {hhmm}, did not complete: {}", e.title),
            )
        }
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(report) else {
        return (
            SummaryKind::Error,
            format!("Last scan: {hhmm}, did not complete: the report could not be read."),
        );
    };
    let n = |k: &str| v.get(k).and_then(|a| a.as_array()).map_or(0, Vec::len);
    let (new, outstanding) = (n("new"), n("still_outstanding"));
    let id = v
        .get("scan_id")
        .and_then(|i| i.as_u64())
        .map_or(String::new(), |i| format!(", scan #{i}"));
    let resolved = match n("resolved") {
        0 => String::new(),
        r => format!(", {r} resolved"),
    };
    let text = format!(
        "Last scan: {hhmm}{id}: {new} new, {outstanding} still outstanding{resolved}, {} informational, {} skipped",
        n("informational"),
        n("skipped")
    );
    let kind = if new + outstanding > 0 {
        SummaryKind::Findings
    } else {
        SummaryKind::Clean
    };
    (kind, text)
}

/// How a scan request reaches the host scanner.
#[derive(Debug, Clone)]
pub enum Transport {
    /// The Flatpak path: the host broker on the session bus.
    Broker,
    /// A native build: pkexec running this scanner binary.
    Native(std::path::PathBuf),
}

static FORCED_TRANSPORT: std::sync::OnceLock<Transport> = std::sync::OnceLock::new();

/// Tests drive the real click path over a private bus without a `/.flatpak-info`.
#[doc(hidden)]
pub fn force_transport_for_tests(transport: Transport) {
    let _ = FORCED_TRANSPORT.set(transport);
}

fn detect_transport() -> Transport {
    if let Some(t) = FORCED_TRANSPORT.get() {
        return t.clone();
    }
    if Path::new("/.flatpak-info").exists() {
        return Transport::Broker;
    }
    let scanner =
        std::env::var("SNITCHWATCH_SCANNER_BIN").unwrap_or_else(|_| scanner_broker::SCANNER.into());
    Transport::Native(scanner.into())
}

pub fn run_deep_scan() -> Result<String, ScanError> {
    run_deep_scan_via(detect_transport())
}

pub fn run_deep_scan_via(transport: Transport) -> Result<String, ScanError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ScanError {
            title: "The deep scan failed.",
            detail: e.to_string(),
        })?;
    let result = runtime.block_on(async {
        match transport {
            Transport::Broker => scanner_broker::request_scan().await,
            Transport::Native(scanner) => scanner_broker::run_native(&scanner).await,
        }
    });
    interpret(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ok(json: &str) -> Result<String, ScanError> {
        Ok(json.into())
    }

    #[test]
    fn summary_states_clean_findings_and_failures() {
        let clean = r#"{"scan_id":5,"new":[],"still_outstanding":[],"resolved":[],
            "informational":[{},{}],"skipped":[{},{}]}"#;
        assert_eq!(
            summarize(&ok(clean), "09:05"),
            (
                SummaryKind::Clean,
                "Last scan: 09:05, scan #5: 0 new, 0 still outstanding, 2 informational, 2 skipped"
                    .into()
            )
        );
        let found = r#"{"scan_id":6,"new":[{}],"still_outstanding":[{},{}],"resolved":[{}]}"#;
        assert_eq!(
            summarize(&ok(found), "10:00"),
            (
                SummaryKind::Findings,
                "Last scan: 10:00, scan #6: 1 new, 2 still outstanding, 1 resolved, 0 informational, 0 skipped"
                    .into()
            )
        );
        let err = Err(ScanError {
            title: "Host scanner setup is required.",
            detail: "/secret/path".into(),
        });
        let (kind, text) = summarize(&err, "11:11");
        assert_eq!(kind, SummaryKind::Error);
        assert_eq!(
            text,
            "Last scan: 11:11, did not complete: Host scanner setup is required."
        );
        assert!(!text.contains("secret"));
        assert_eq!(summarize(&ok("not json"), "01:02").0, SummaryKind::Error);
    }

    #[test]
    fn local_time_is_hh_mm() {
        let t = local_hhmm();
        assert!(t.len() == 5 && t.as_bytes()[2] == b':', "{t}");
    }

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
