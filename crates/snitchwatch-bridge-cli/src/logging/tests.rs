use super::*;

const CHILD: &str = "SNITCHWATCH_CLI_FRAME_CAP_TEST_CHILD";

#[test]
fn subscriber_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    init();
    log::trace!(target: "tungstenite::protocol", "Received message EXPORT-BODY-CANARY");
    log::debug!(target: "tokio_tungstenite::compat", "AUTH-CANARY");
    tracing::trace!(target: "tungstenite::protocol::frame", "EXPORT-BODY-CANARY");
    tracing::debug!(target: "tokio_tungstenite", "AUTH-CANARY");
    // Positives prove both the log adapter and direct tracing paths are live.
    log::info!(target: "tungstenite::protocol", "BRIDGED-INFO-POSITIVE");
    log::trace!(target: "snitchwatch_bridge_cli::logging", "BRIDGED-APP-TRACE-POSITIVE");
    tracing::warn!(target: "tokio_tungstenite", "DIRECT-WARN-POSITIVE");
    tracing::trace!(target: "snitchwatch_bridge_cli::logging", "DIRECT-APP-TRACE-POSITIVE");
    tracing::trace!(target: "tungstenite_extra", "UNKNOWN-LIBRARY-TRACE-POSITIVE");
}

#[test]
fn rust_log_trace_cannot_enable_frame_bodies_or_disable_application_trace() {
    for filter in ["trace", "trace,tungstenite::protocol=trace,tungstenite::protocol::frame=trace,tokio_tungstenite=trace,tokio_tungstenite::compat=trace"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "logging::tests::subscriber_child", "--nocapture"])
            .env(CHILD, "1").env("RUST_LOG", filter).output().unwrap();
        assert!(output.status.success());
        let captured = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        for positive in ["BRIDGED-INFO-POSITIVE", "BRIDGED-APP-TRACE-POSITIVE", "DIRECT-WARN-POSITIVE", "DIRECT-APP-TRACE-POSITIVE", "UNKNOWN-LIBRARY-TRACE-POSITIVE"] {
            assert!(captured.contains(positive), "missing {positive}: {captured}");
        }
        assert!(!captured.contains("EXPORT-BODY-CANARY"), "frame body escaped subscriber cap");
        assert!(!captured.contains("AUTH-CANARY"), "authentication escaped subscriber cap");
    }
}
