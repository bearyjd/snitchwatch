//! Registered controls and authenticated fixture WebSocket; no live daemon.
use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use futures_util::{SinkExt, StreamExt};
use snitchwatch_bridge::{
    analysis_export::AppSelector,
    ws_messages::{ClientMessage, ServerMessage},
};
#[allow(unused_imports)]
use snitchwatch_kirigami::analysis_export_controller as _;
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::Message;
mod common;

const SELECTOR: &str = "/private/selector-CANARY/bin/curl";
const CANARY: &str = "PAYLOAD-CANARY-DO-NOT-LOG";
const ARTIFACT: &str = include_str!("support/analysis_export_artifact.json");
#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);
impl Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn registered_controls_use_capable_authenticated_session_and_save_exact_bytes_privately() {
    common::init_headless_qt_env();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::Builder::new()
        .prefix("export-location-CANARY-")
        .tempdir()
        .unwrap();
    let socket = socket_dir.path().join("bridge.sock");
    let token = socket_dir.path().join("token");
    std::fs::write(&token, "fixture-token-CANARY").unwrap();
    std::env::set_var("SNITCHWATCH_WS_SOCKET", &socket);
    std::env::set_var("SNITCHWATCH_WS_TOKEN_PATH", &token);
    std::env::set_var("XDG_DATA_HOME", data_dir.path());
    let buffer = LogBuffer(Arc::new(Mutex::new(Vec::new())));
    let capture = buffer.clone();
    // Only app-owned diagnostics: logging.rs separately tests production's
    // dependency frame cap, including hostile environment directives.
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter("snitchwatch_kirigami=trace")
            .with_writer(move || capture.clone())
            .finish(),
    )
    .unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let cancels = Arc::new(AtomicUsize::new(0));
    let listener = runtime.block_on(async { tokio::net::UnixListener::bind(&socket).unwrap() });
    let server = runtime.spawn({
        let requests = requests.clone(); let cancels = cancels.clone();
        async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert!(matches!(ws.next().await, Some(Ok(Message::Text(t))) if t == "fixture-token-CANARY"));
            ws.send(Message::Text(format!("{{\"action\":\"authenticated\",\"capabilities\":\"{CANARY}\"}}"))).await.unwrap();
            drop(ws);
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            ws.send(Message::Text(r#"{"action":"authenticated","capabilities":["curatedBroadOptInV1","aiCacheSummaryExportV10"]}"#.into())).await.unwrap();
            let mut snapshots = 0;
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let message = serde_json::from_str::<ClientMessage>(&text).unwrap();
                if matches!(message, ClientMessage::RequestSnapshot) {
                    snapshots += 1;
                    if snapshots == 2 { break; }
                } else { panic!("older bridge received an export action"); }
            }
            ws.close(None).await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            ws.send(Message::Text(r#"{"action":"authenticated","capabilities":["aiCacheSummaryExportV1"]}"#.into())).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                match serde_json::from_str::<ClientMessage>(&text).unwrap() {
                    ClientMessage::RequestSnapshot => {},
                    ClientMessage::CancelCacheSummaryExport { schema_version, .. } => {
                        assert_eq!(schema_version, 1); cancels.fetch_add(1, Ordering::SeqCst);
                    }
                    ClientMessage::RequestCacheSummaryExport { request_id, schema_version, filters, .. } => {
                        assert_eq!(schema_version, 1);
                        let request = requests.fetch_add(1, Ordering::SeqCst);
                        let window = filters.window.expect("UTC window missing");
                        assert!(window.from_ms == 1792803600000 && window.to_ms == 1792807200000, "wrong independent UTC window");
                        if request == 0 {
                            assert!(matches!(filters.app, Some(AppSelector::Path { value }) if value == SELECTOR), "exact path changed");
                            assert!(filters.verdicts.is_some_and(|v| v == [snitchwatch_bridge::analysis_export::ExportVerdict::Deny]), "verdict selector changed");
                        } else {
                            assert!(matches!(filters.app, Some(AppSelector::MissingPath)), "missing-path category changed");
                            assert!(filters.verdicts.is_some_and(|v| v.is_empty()), "empty verdict set was widened");
                        }
                        ws.send(Message::Text(format!("{{\"action\":\"cacheSummaryExportRefused\",\"requestId\":\"{request_id}\",\"reason\":\"{CANARY}\"}}"))).await.unwrap();
                        ws.send(Message::Text(serde_json::to_string(&ServerMessage::CacheSummaryExport { request_id, artifact: ARTIFACT.into() }).unwrap())).await.unwrap();
                    }
                    _ => panic!("export probe sent a policy or unrelated action"),
                }
            }
        }
    });
    snitchwatch_kirigami::bridge_runtime::ensure_started();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !snitchwatch_kirigami::bridge_runtime::handles().is_some_and(|h| h.is_connected()) {
        assert!(Instant::now() < deadline, "old bridge did not authenticate");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(snitchwatch_kirigami::bridge_runtime::handles()
        .unwrap()
        .analysis_export_session()
        .is_none());
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let created = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let created = created.clone();
        engine.on_object_created(move |_, obj, _| {
            created.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let qml = include_str!("probes/analysis_export_probe.qml")
        .replace(
            "\"EXPECTED_ARTIFACT\"",
            &serde_json::to_string(ARTIFACT).unwrap(),
        )
        .replace(
            "\"EXACT_SELECTOR\"",
            &serde_json::to_string(SELECTOR).unwrap(),
        );
    let captured = common::capture_stderr(|| {
        engine.as_mut().unwrap().load_data(
            &QByteArray::from(&qml),
            &QUrl::from("qrc:/analysis_export_probe.qml"),
        );
        if created.load(Ordering::SeqCst) {
            app.as_mut().unwrap().exec();
        }
    });
    drop(guard);
    assert!(
        created.load(Ordering::SeqCst),
        "export controller not registered: {captured}"
    );
    assert!(
        captured.contains("ANALYSIS_EXPORT_PROBE_COMPLETE"),
        "{captured}"
    );
    assert!(
        !captured.contains("Error:") && !captured.contains("TIMEOUT"),
        "{captured}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert!(cancels.load(Ordering::SeqCst) >= 1);
    let exports = data_dir.path().join("snitchwatch/analysis-exports");
    let entries = std::fs::read_dir(&exports)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        std::fs::read(entries[0].path()).unwrap(),
        ARTIFACT.as_bytes()
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&exports).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        entries[0].metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    for canary in [
        CANARY,
        SELECTOR,
        "fixture-token-CANARY",
        "curl-é",
        "export-location-CANARY",
        "analysis-exports/",
    ] {
        assert!(
            !logs.contains(canary) && !captured.contains(canary),
            "sensitive export value reached diagnostics"
        );
    }
    assert!(logs.contains("invalid bridge authentication acknowledgement"));
    assert!(logs.contains("skipping an unrecognized bridge message"));
    assert!(logs.contains("invalid client message dropped"));
    server.abort();
}
