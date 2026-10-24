//! Unprivileged, narrowly scoped session-bus adapter for the host scanner.
//! The wire method accepts no executable, path, arguments or environment.
//! The scanner reserves stdout for JSON and stderr for diagnostics. A watchdog
//! timeout requires both its exact stderr marker and SIGKILL. The watchdog's
//! marker is best effort: if its pipe is full or unavailable, termination still
//! occurs and the broker reports generic failure because the cause is unknown.
use std::{
    os::unix::process::ExitStatusExt,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

pub const BUS_NAME: &str = "org.snitchwatch.Scanner";
pub const OBJECT_PATH: &str = "/org/snitchwatch/Scanner";
pub const INTERFACE: &str = "org.snitchwatch.Scanner1";
pub const SCANNER: &str = "/usr/libexec/snitchwatch-scanner-privileged";
pub const MAX_REPORT: usize = 4 * 1024 * 1024;
const MAX_ERROR: usize = 64 * 1024;
const SCAN_TIMEOUT: Duration = Duration::from_secs(600);
// Matches the privileged watchdog's single atomic diagnostic write. A signal
// alone cannot distinguish its deadline from an unrelated kill or OOM event.
const WATCHDOG_TIMEOUT_MARKER: &[u8] = b"\nSNITCHWATCH_SCANNER_WATCHDOG_TIMEOUT\n";
pub type Reply = (String, String);

/// Wire schema emitted by scanner-privileged::main::print_json. Keep this
/// typed boundary independent of the scanner's host-reading implementation.
#[derive(serde::Deserialize)]
pub struct Report {
    pub scan_id: i64,
    pub new: Vec<Finding>,
    pub still_outstanding: Vec<Finding>,
    pub resolved: Vec<ResolvedFinding>,
    pub informational: Vec<Finding>,
    pub skipped: Vec<SkippedCheck>,
}
#[derive(serde::Deserialize)]
pub struct Finding {
    pub path: String,
    pub detail: String,
}
#[derive(serde::Deserialize)]
pub struct ResolvedFinding {
    pub path: String,
}
#[derive(serde::Deserialize)]
pub struct SkippedCheck {
    pub check: String,
    pub reason: String,
}

fn reply(code: &str, detail: impl Into<String>) -> Reply {
    (code.into(), detail.into())
}

/// Validate the report before either transport presents it as a successful scan.
pub fn validate_report(text: String) -> Reply {
    let valid = text.len() <= MAX_REPORT && serde_json::from_str::<Report>(&text).is_ok();
    if valid {
        reply("ok", text)
    } else {
        reply(
            "failed",
            "The host scanner returned an invalid or oversized report.",
        )
    }
}

async fn read_limited(reader: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::other(
            "scanner output exceeded its size limit",
        ));
    }
    Ok(bytes)
}

/// Native GUI callers may select a development scanner. The broker never does.
pub async fn run_native(scanner: &Path) -> Reply {
    run_process(Path::new("/usr/bin/pkexec"), scanner, SCAN_TIMEOUT).await
}

async fn run_process(pkexec: &Path, scanner: &Path, timeout: Duration) -> Reply {
    if !pkexec.is_file() || !scanner.is_file() {
        return reply("unavailable", "The host scanner or /usr/bin/pkexec is missing. Install the Snitchwatch scanner host integration (packaging/scanner/README.md).");
    }
    let mut command = Command::new(pkexec);
    command
        .arg("--disable-internal-agent")
        .arg(scanner)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return reply(
                "unavailable",
                format!("Could not start the host scanner: {e}"),
            )
        }
    };
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            read_limited(stdout, MAX_REPORT),
            read_limited(stderr, MAX_ERROR),
            child.wait()
        )
    })
    .await;
    match result {
        Ok(Ok((stdout, stderr, status))) => match status.code() {
            Some(0 | 2) => match String::from_utf8(stdout) {
                Ok(report) => validate_report(report),
                Err(_) => reply("failed", "The host scanner returned non-UTF-8 output."),
            },
            Some(126) => reply(
                "denied",
                "Authentication was cancelled. No scan report was produced.",
            ),
            Some(127) => reply(
                "denied",
                format!(
                    "The host could not authorize or start the scan. {}",
                    String::from_utf8_lossy(&stderr).trim()
                ),
            ),
            None if status.signal() == Some(libc::SIGKILL)
                && stderr
                    .windows(WATCHDOG_TIMEOUT_MARKER.len())
                    .any(|part| part == WATCHDOG_TIMEOUT_MARKER) =>
            {
                reply("timeout", "The host scanner reached its execution limit.")
            }
            _ => reply(
                "failed",
                format!(
                    "The host scanner exited with {status}: {}",
                    String::from_utf8_lossy(&stderr).trim()
                ),
            ),
        },
        failure => {
            // Best effort for unprivileged authentication and test children.
            // The privileged scanner also enforces its own 9-minute watchdog;
            // an unprivileged broker cannot kill a child after it becomes root.
            if let Some(pid) = pid {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            // Do not wait indefinitely for a privileged child or its pipe owners.
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            match failure {
                Err(_) => reply(
                    "timeout",
                    "The deep scan timed out. The host scanner has its own execution limit.",
                ),
                Ok(Err(e)) => reply(
                    "failed",
                    format!("Could not read the host scanner report: {e}"),
                ),
                _ => unreachable!(),
            }
        }
    }
}

#[derive(Clone)]
pub struct Broker {
    state: Arc<Mutex<BrokerState>>,
    #[cfg(test)]
    fake_process: Option<(std::path::PathBuf, std::path::PathBuf)>,
}
enum BrokerState {
    Idle { last_finished: Instant },
    Running,
    Stopping,
}
#[derive(Debug)]
enum AdmissionError {
    Busy,
    Stopping,
}
impl Default for Broker {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(BrokerState::Idle {
                last_finished: Instant::now(),
            })),
            #[cfg(test)]
            fake_process: None,
        }
    }
}
struct ScanGuard<'a>(&'a Broker);
impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        *self.0.state.lock().unwrap() = BrokerState::Idle {
            last_finished: Instant::now(),
        };
    }
}
impl Broker {
    fn begin(&self) -> Result<ScanGuard<'_>, AdmissionError> {
        let mut state = self.state.lock().unwrap();
        match *state {
            BrokerState::Idle { .. } => {
                *state = BrokerState::Running;
                Ok(ScanGuard(self))
            }
            BrokerState::Running => Err(AdmissionError::Busy),
            BrokerState::Stopping => Err(AdmissionError::Stopping),
        }
    }
    /// Claim shutdown under the same lock as admission. Once this succeeds no
    /// later request can start work while the main loop drops the connection.
    pub fn claim_idle_shutdown(&self, duration: Duration) -> bool {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, BrokerState::Idle { last_finished } if last_finished.elapsed() >= duration)
        {
            *state = BrokerState::Stopping;
            true
        } else {
            false
        }
    }
}
#[zbus::interface(name = "org.snitchwatch.Scanner1")]
impl Broker {
    async fn run_deep_scan(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<Reply> {
        if header.signature() != &zbus::zvariant::Signature::Unit {
            return Err(zbus::fdo::Error::InvalidArgs(
                "RunDeepScan takes no arguments".into(),
            ));
        }
        let _guard = match self.begin() {
            Ok(guard) => guard,
            Err(AdmissionError::Busy) => {
                return Ok(reply("busy", "A host deep scan is already running."))
            }
            Err(AdmissionError::Stopping) => {
                return Ok(reply(
                    "failed",
                    "The host scanner broker is shutting down. Retry the scan.",
                ))
            }
        };
        // No environment override or caller-provided values cross this boundary.
        #[cfg(test)]
        if let Some((pkexec, scanner)) = &self.fake_process {
            return Ok(run_process(pkexec, scanner, SCAN_TIMEOUT).await);
        }
        Ok(run_native(Path::new(SCANNER)).await)
    }
}

/// Called off the GUI thread. The connection deadline accommodates authentication.
pub async fn request_scan() -> Reply {
    let result: zbus::Result<Reply> = async {
        let connection = zbus::connection::Builder::session()?
            .method_timeout(Duration::from_secs(605))
            .build()
            .await?;
        let proxy = zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE).await?;
        proxy.call("RunDeepScan", &()).await
    }
    .await;
    match result {
        Ok((code, detail)) if code == "ok" => validate_report(detail),
        Ok(result) => result,
        Err(e) => reply("unavailable", format!("The Snitchwatch host scanner integration is unavailable. Install its broker, scanner and polkit policy on the host (packaging/scanner/README.md). {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    const REPORT: &str = r#"{"scan_id":1,"new":[],"still_outstanding":[],"resolved":[],"informational":[],"skipped":[]}"#;
    // A parallel spawn can inherit another fixture's writable fd until exec,
    // causing ETXTBSY even after std::fs::write closes the original fd.
    static FAKE_PROCESS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    async fn fake(script: &str, timeout: Duration) -> Reply {
        let _fixture_guard = FAKE_PROCESS_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("fake-pkexec");
        std::fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        run_process(&exe, &exe, timeout).await
    }
    #[tokio::test]
    async fn accepts_anomaly_exit_and_keeps_fixed_arguments() {
        let script = format!("[ \"$#\" = 3 ] && [ \"$1\" = --disable-internal-agent ] && [ \"$2\" = \"$0\" ] && [ \"$3\" = --json ] || exit 90\nprintf '%s' '{REPORT}'\nexit 2");
        assert_eq!(
            fake(&script, Duration::from_secs(2)).await,
            reply("ok", REPORT)
        );
    }
    #[tokio::test]
    async fn reports_denial_invalid_output_and_failure() {
        for (script, expected) in [
            ("exit 126", "denied"),
            ("exit 127", "denied"),
            ("printf nope", "failed"),
            ("exit 1", "failed"),
        ] {
            assert_eq!(fake(script, Duration::from_secs(2)).await.0, expected);
        }
        assert_eq!(
            run_process(
                Path::new("/nonexistent/pkexec"),
                Path::new(SCANNER),
                Duration::ZERO
            )
            .await
            .0,
            "unavailable"
        );
    }
    #[tokio::test]
    async fn bounds_execution_and_output() {
        assert_eq!(
            fake("sleep 20", Duration::from_millis(50)).await.0,
            "timeout"
        );
        assert_eq!(
            fake("head -c 4200000 /dev/zero", Duration::from_secs(2))
                .await
                .0,
            "failed"
        );
    }
    #[tokio::test]
    async fn recognizes_only_the_watchdogs_explicit_kill_contract() {
        let marker = "SNITCHWATCH_SCANNER_WATCHDOG_TIMEOUT";
        let marked_kill = format!("printf '\\n{marker}\\n' >&2\nkill -KILL $$");
        assert_eq!(
            fake(&marked_kill, Duration::from_secs(2)).await.0,
            "timeout"
        );
        for script in [
            "kill -KILL $$".to_string(),
            format!("printf '\\n{marker}\\n' >&2\nexit 1"),
            format!("printf '\\n{marker}\\n' >&2\nkill -TERM $$"),
            format!("printf 'prefix-{marker}\\n' >&2\nkill -KILL $$"),
            format!("printf '\\n{marker}' >&2\nkill -KILL $$"),
        ] {
            assert_eq!(fake(&script, Duration::from_secs(2)).await.0, "failed");
        }
    }
    #[test]
    fn rejects_overlap_and_exits_only_when_idle() {
        let broker = Broker::default();
        let first = broker.begin().unwrap();
        assert!(broker.begin().is_err());
        assert!(!broker.claim_idle_shutdown(Duration::ZERO));
        drop(first);
        let retry = broker.begin().expect("finished work must permit retries");
        drop(retry);
        assert!(broker.claim_idle_shutdown(Duration::ZERO));
        assert!(broker.begin().is_err());
    }
    #[test]
    fn idle_shutdown_claim_prevents_later_admission() {
        let broker = Broker::default();
        std::thread::scope(|scope| {
            let (claimed, claim) = std::sync::mpsc::channel();
            let broker = &broker;
            let request = scope.spawn(move || {
                claim.recv().unwrap();
                assert!(
                    broker.begin().is_err(),
                    "shutdown won, so no scan may start"
                );
            });
            assert!(broker.claim_idle_shutdown(Duration::ZERO));
            claimed.send(()).unwrap();
            request.join().unwrap();
        });
    }
    #[test]
    fn admitted_scan_prevents_shutdown_until_its_guard_finishes() {
        let broker = Broker::default();
        std::thread::scope(|scope| {
            let (admitted, admission) = std::sync::mpsc::channel();
            let (finish, finished) = std::sync::mpsc::channel();
            let broker = &broker;
            let request = scope.spawn(move || {
                let _scan = broker.begin().unwrap();
                admitted.send(()).unwrap();
                finished.recv().unwrap();
            });
            admission.recv().unwrap();
            assert!(!broker.claim_idle_shutdown(Duration::ZERO));
            assert!(broker.begin().is_err());
            finish.send(()).unwrap();
            request.join().unwrap();
        });
        assert!(!broker.claim_idle_shutdown(Duration::from_secs(30)));
        let retry = broker
            .begin()
            .expect("a rejected shutdown must permit retries");
        drop(retry);
        assert!(broker.claim_idle_shutdown(Duration::ZERO));
        assert!(broker.begin().is_err());
    }
    #[test]
    fn rejects_truncated_and_unstructured_reports() {
        for report in ["{}", "[]", "{", r#"{"scan_id":"wrong"}"#] {
            assert_eq!(validate_report(report.into()).0, "failed");
        }
        assert_eq!(
            validate_report(
                REPORT.replace("\"new\":[]", "\"new\":[{\"path\":2,\"detail\":false}]")
            )
            .0,
            "failed"
        );
    }

    #[tokio::test]
    #[ignore = "requires a private session: dbus-run-session -- cargo test -p scanner-broker private_bus -- --ignored"]
    async fn private_bus_method_has_no_inputs_and_rejects_concurrent_calls() {
        let _fixture_guard = FAKE_PROCESS_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("fake-pkexec");
        std::fs::write(
            &exe,
            format!("#!/bin/sh\nsleep 0.2\nprintf '%s' '{REPORT}'\nexit 2\n"),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let broker = Broker {
            fake_process: Some((exe.clone(), exe)),
            ..Broker::default()
        };
        let _server = zbus::connection::Builder::session()
            .unwrap()
            .name(BUS_NAME)
            .unwrap()
            .serve_at(OBJECT_PATH, broker.clone())
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::Connection::session().await.unwrap();
        let proxy = zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, INTERFACE)
            .await
            .unwrap();
        let introspect = zbus::fdo::IntrospectableProxy::builder(&client)
            .destination(BUS_NAME)
            .unwrap()
            .path(OBJECT_PATH)
            .unwrap()
            .build()
            .await
            .unwrap()
            .introspect()
            .await
            .unwrap();
        let method = introspect
            .split("<method name=\"RunDeepScan\">")
            .nth(1)
            .unwrap()
            .split("</method>")
            .next()
            .unwrap();
        assert!(!method.contains("direction=\"in\""), "{method}");
        let bad: zbus::Result<Reply> = proxy.call("RunDeepScan", &("/tmp/evil",)).await;
        assert!(bad.is_err());
        let first = proxy.call::<_, _, Reply>("RunDeepScan", &());
        let second = async {
            for _ in 0..100 {
                if matches!(*broker.state.lock().unwrap(), BrokerState::Running) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            proxy.call::<_, _, Reply>("RunDeepScan", &()).await
        };
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap().0, "ok");
        assert_eq!(second.unwrap().0, "busy");
        assert!(broker.claim_idle_shutdown(Duration::ZERO));
    }
}
