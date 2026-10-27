use super::*;
use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use std::{
    sync::{atomic::AtomicUsize, mpsc},
    time::{Duration, Instant},
};
#[path = "../../tests/common/mod.rs"]
mod common;
#[path = "../analysis_export_native_test.rs"]
mod native;

const ARTIFACT: &str = include_str!("../../tests/support/analysis_export_artifact.json");
struct Harness {
    engine: cxx::UniquePtr<QQmlApplicationEngine>,
}
impl Harness {
    fn with<T>(&mut self, f: impl FnOnce(Pin<&mut qobject::AnalysisExportController>) -> T) -> T {
        let pointer = native::ffi::find_controller(self.engine.pin_mut());
        assert!(!pointer.is_null(), "registered controller is absent");
        // SAFETY: the engine owns this QObject for the harness lifetime; this
        // borrow ends before any event pumping or engine destruction.
        f(unsafe { Pin::new_unchecked(&mut *pointer) })
    }
    fn probe(&mut self, method: &str) {
        assert_eq!(
            native::ffi::probe(self.engine.pin_mut(), &QString::from(method)).to_string(),
            "ok"
        );
    }
    fn pump_until(&mut self, mut condition: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self) {
            assert!(Instant::now() < deadline, "Qt condition timed out");
            native::ffi::process_events();
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    /// The claimed delivery is queued to the Qt thread after the claim, so one
    /// `process_events` can run first; wait for the request to finish instead.
    fn pump_until_idle(&mut self) {
        self.pump_until(|h| !h.with(|c| *c.busy()));
    }
    fn request(&mut self) -> String {
        self.with(|mut c| {
            c.as_mut().request_export(&QString::from("{}"));
            c.pending.as_ref().unwrap().1.clone()
        })
    }
}
fn wait_claimed(gate: &DeliveryGate) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while gate.lock().unwrap().is_some() {
        assert!(Instant::now() < deadline, "feed did not queue delivery");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn reply(link: &crate::bridge_runtime::test_link::TestLink, session: u64, id: String) {
    link.feed
        .send(crate::bridge_runtime::ReceivedServerMessage {
            connection_id: session,
            message: ServerMessage::CacheSummaryExport {
                request_id: id,
                artifact: ARTIFACT.into(),
            },
        })
        .unwrap();
}

#[test]
fn stale_delivery_and_off_qt_save_keep_independent_lifetimes() {
    common::init_headless_qt_env();
    let _app = QGuiApplication::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let link = crate::bridge_runtime::test_link::TestLink::new(runtime.handle().clone(), 32, 32);
    link.set_analysis_export(true);
    let directory = tempfile::tempdir().unwrap();
    let mut harness = Harness {
        engine: QQmlApplicationEngine::new(),
    };
    let captured = common::capture_stderr(|| {
        harness.engine.pin_mut().load_data(
            &QByteArray::from(include_str!(
                "../../tests/probes/analysis_export_lifecycle_probe.qml"
            )),
            &QUrl::from("qrc:/analysis_export_lifecycle_probe.qml"),
        );
        native::ffi::process_events();
        harness.with(|mut c| {
            c.as_mut().rust_mut().test_handles = Some(link.handles.clone());
            c.as_mut().rust_mut().test_directory = Some(directory.path().join("exports"));
            c.start_bridge_feed();
        });
        let gate = harness.with(|c| c.delivery.clone());
        let malformed = harness.request();
        link.feed
            .send(crate::bridge_runtime::ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::CacheSummaryExport {
                    request_id: malformed,
                    artifact: "{PAYLOAD-CANARY-malformed".into(),
                },
            })
            .unwrap();
        wait_claimed(&gate);
        harness.pump_until_idle();
        assert!(
            !harness.with(|c| *c.preview_ready()),
            "malformed artifact enabled preview and Save"
        );
        assert_eq!(harness.with(|c| c.status_text().to_string()), BAD_ARTIFACT);
        let oversized = harness.request();
        link.feed
            .send(crate::bridge_runtime::ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::CacheSummaryExport {
                    request_id: oversized,
                    artifact: "x".repeat(MAX_ARTIFACT + 1),
                },
            })
            .unwrap();
        wait_claimed(&gate);
        harness.pump_until_idle();
        assert!(!harness.with(|c| *c.preview_ready()));
        assert_eq!(
            harness.with(|c| c.status_text().to_string()),
            refusal(ExportError::TooLarge)
        );
        // Delivered straight to on_delivery with no event pump in between, so
        // the sheet's periodic poll cannot have expired the request first.
        let direct = harness.request();
        harness.with(|mut c| {
            c.as_mut().rust_mut().view.begin(
                1,
                direct.clone(),
                Instant::now() - Duration::from_secs(16),
            );
            c.as_mut()
                .on_delivery(1, Delivery::Artifact(direct, ARTIFACT.into()));
        });
        assert!(
            !harness.with(|c| *c.busy()),
            "on_delivery left an expired request busy"
        );
        assert!(!harness.with(|c| *c.preview_ready()));
        assert_eq!(
            harness.with(|c| c.status_text().to_string()),
            refusal(ExportError::TimedOut)
        );
        let expired = harness.request();
        harness.with(|mut c| {
            c.as_mut().rust_mut().view.begin(
                1,
                expired.clone(),
                Instant::now() - Duration::from_secs(16),
            )
        });
        reply(&link, 1, expired);
        wait_claimed(&gate);
        harness.pump_until_idle();
        assert!(
            !harness.with(|c| *c.busy()),
            "late matching reply left the request permanently busy"
        );
        assert!(!harness.with(|c| *c.preview_ready()));
        assert_eq!(
            harness.with(|c| c.status_text().to_string()),
            refusal(ExportError::TimedOut)
        );
        let refused = harness.request();
        link.feed
            .send(crate::bridge_runtime::ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::CacheSummaryExportRefused {
                    request_id: refused.clone(),
                    reason: ExportError::Busy,
                },
            })
            .unwrap();
        wait_claimed(&gate);
        harness.pump_until_idle();
        assert!(!harness.with(|c| *c.busy() || *c.preview_ready()));
        reply(&link, 1, refused);
        std::thread::sleep(Duration::from_millis(10));
        native::ffi::process_events();
        assert!(
            !harness.with(|c| *c.preview_ready()),
            "success replaced a first matching refusal"
        );
        let id = harness.request();
        reply(&link, 1, id);
        wait_claimed(&gate);
        harness.with(|c| c.cancel());
        native::ffi::process_events();
        assert!(!harness.with(|c| *c.preview_ready()));

        let old = harness.request();
        reply(&link, 1, old);
        wait_claimed(&gate);
        let new = harness.request();
        native::ffi::process_events();
        assert!(!harness.with(|c| *c.preview_ready()));
        reply(&link, 1, new.clone());
        harness.pump_until(|h| h.with(|c| *c.preview_ready()));
        link.feed
            .send(crate::bridge_runtime::ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::CacheSummaryExportRefused {
                    request_id: new,
                    reason: ExportError::Busy,
                },
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        native::ffi::process_events();
        assert!(
            harness.with(|c| *c.preview_ready()),
            "duplicate refusal cleared accepted preview"
        );
        assert_eq!(
            harness.with(|c| c.preview_text().to_string()).as_bytes(),
            ARTIFACT.as_bytes()
        );

        let old = harness.request();
        reply(&link, 1, old);
        wait_claimed(&gate);
        link.set_session(false, 1);
        link.set_session(true, 2);
        native::ffi::process_events();
        assert!(!harness.with(|c| *c.preview_ready()));
        let current = harness.request();
        reply(&link, 2, current);
        harness.pump_until(|h| h.with(|c| *c.preview_ready()));
        harness.probe("openPreview");
        assert!(
            !directory.path().join("exports").exists(),
            "preview/cancel created a file"
        );

        let saves = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let count = saves.clone();
        harness.with(|mut c| {
            c.as_mut().rust_mut().writer = Arc::new(move |path, bytes| {
                count.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                let mut saved = analysis_export::save_artifact(path, bytes)?;
                saved.durability = Durability::Unconfirmed;
                Ok(saved)
            })
        });
        let before = native::ffi::heartbeat(harness.engine.pin_mut());
        assert!(harness.with(|c| c.save()));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!harness.with(|c| c.save()));
        harness.with(|c| c.cancel());
        harness.with(|c| c.request_export(&QString::from("{}")));
        harness.probe("checkSavingLocks");
        harness.probe("navigateAway");
        native::ffi::process_events();
        harness.probe("checkSavingLocks");
        harness.probe("escape");
        harness.probe("windowClose");
        harness.probe("trayHide");
        harness.pump_until(|h| native::ffi::heartbeat(h.engine.pin_mut()) >= before + 3);
        harness.probe("checkSavingLocks");
        link.set_session(false, 2);
        harness.pump_until(|h| !h.with(|c| *c.preview_ready()));
        harness.probe("checkSessionLost");
        release_tx.send(()).unwrap();
        harness.pump_until(|h| !h.with(|c| *c.saving()));
        harness.probe("checkOutcome");
        assert_eq!(saves.load(Ordering::SeqCst), 1);
        let saved = harness.with(|c| c.saved_location().to_string());
        assert_eq!(std::fs::read(&saved).unwrap(), ARTIFACT.as_bytes());
        assert_eq!(
            std::fs::read_dir(directory.path().join("exports"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(harness.with(|c| c.status_text().to_string()), UNCERTAIN);
        let completed_at = native::ffi::heartbeat(harness.engine.pin_mut());
        harness.pump_until(|h| native::ffi::heartbeat(h.engine.pin_mut()) >= completed_at + 30);
        assert_eq!(harness.with(|c| c.saved_location().to_string()), saved);
        assert_eq!(harness.with(|c| c.status_text().to_string()), UNCERTAIN);
        harness.probe("checkOutcome");
        assert!(!harness.with(|c| c.save()));
        harness.probe("cleanup");
        native::ffi::process_events();
        drop(runtime);
        native::ffi::process_events();
    });
    assert!(
        !captured.contains("Error:") && !captured.contains("failed to load"),
        "{captured}"
    );
    assert!(
        !captured.contains("curl-é") && !captured.contains(directory.path().to_str().unwrap()),
        "diagnostic canary leaked"
    );
}
