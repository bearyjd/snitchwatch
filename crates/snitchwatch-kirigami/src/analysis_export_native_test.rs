// Referenced by cfg(test) only. These helpers inspect real registered controls;
// there are no additional QML actions or production configuration switches.
#[cxx::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qqmlapplicationengine.h");
        type QQmlApplicationEngine = cxx_qt_lib::QQmlApplicationEngine;
        include!("analysis_export_test.h");
        type AnalysisExportController = super::super::super::qobject::AnalysisExportController;
        #[namespace = "snitchwatch::test"]
        #[rust_name = "find_controller"]
        fn findAnalysisExportController(
            engine: Pin<&mut QQmlApplicationEngine>,
        ) -> *mut AnalysisExportController;
        #[namespace = "snitchwatch::test"]
        #[rust_name = "process_events"]
        fn processAnalysisEvents();
        #[namespace = "snitchwatch::test"]
        #[rust_name = "probe"]
        fn analysisProbe(engine: Pin<&mut QQmlApplicationEngine>, method: &QString) -> QString;
        #[namespace = "snitchwatch::test"]
        #[rust_name = "heartbeat"]
        fn analysisHeartbeat(engine: Pin<&mut QQmlApplicationEngine>) -> i32;
    }
}
