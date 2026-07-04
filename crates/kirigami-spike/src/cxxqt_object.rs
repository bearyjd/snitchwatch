// THROWAWAY SPIKE — cxx-qt bridge proving the four Phase 3a proof points:
//   1. Async signal emission from a Tokio context into QML via CxxQtThread.
//   2. A QAbstractListModel-backed QML list (not just a scalar property).
//   (3 and 4 are proven by the build system + tests/, not this file.)
use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};
use std::time::Duration;

/// Role ids exposed to the QML delegate.
const ROLE_PROCESS: i32 = 0;
const ROLE_DESTINATION: i32 = 1;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;

        include!(<QtCore/QAbstractListModel>);
        /// Opaque C++ base class we subclass via `#[base = QAbstractListModel]`.
        type QAbstractListModel;
    }

    extern "RustQt" {
        /// A QAbstractListModel subclass registered as a QML element. Rows are
        /// appended from a background Tokio task, exercising proof points 1 & 2.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        #[qproperty(bool, running)]
        type ConnectionModel = super::ConnectionModelRust;

        /// Proof point 1: emitted from the Qt thread after a Tokio task queues
        /// the append. QML connects to this to prove async signal delivery.
        #[qsignal]
        #[cxx_name = "connectionAdded"]
        fn connection_added(
            self: Pin<&mut ConnectionModel>,
            process: QString,
            destination: QString,
        );

        /// Kicks off the background Tokio feed.
        #[qinvokable]
        #[cxx_name = "startFeed"]
        fn start_feed(self: Pin<&mut ConnectionModel>);

        /// Appends one row (called on the Qt thread, either from QML or from
        /// the queued Tokio closure).
        #[qinvokable]
        #[cxx_name = "appendRow"]
        fn append_row(self: Pin<&mut ConnectionModel>, process: QString, destination: QString);

        // --- QAbstractListModel overrides (proof point 2) ---
        // cxx_name maps to the exact Qt virtual so C++ `override` resolves.
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &ConnectionModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &ConnectionModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &ConnectionModel) -> QHash_i32_QByteArray;
    }

    // Protected base-class helpers inherited from QAbstractListModel.
    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginInsertRows"]
        unsafe fn begin_insert_rows(
            self: Pin<&mut ConnectionModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );

        #[inherit]
        #[cxx_name = "endInsertRows"]
        unsafe fn end_insert_rows(self: Pin<&mut ConnectionModel>);
    }

    // Proof point 1 enabler: gives us `qt_thread()` + `CxxQtThread::queue`.
    impl cxx_qt::Threading for ConnectionModel {}
}

/// Rust-side state for the model.
#[derive(Default)]
pub struct ConnectionModelRust {
    entries: Vec<(String, String)>,
    count: i32,
    running: bool,
}

impl qobject::ConnectionModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.entries.len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        if let Some((process, destination)) = self.entries.get(index.row() as usize) {
            return match role {
                ROLE_PROCESS => QVariant::from(&QString::from(process)),
                ROLE_DESTINATION => QVariant::from(&QString::from(destination)),
                _ => QVariant::default(),
            };
        }
        QVariant::default()
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_PROCESS, QByteArray::from("process"));
        roles.insert(ROLE_DESTINATION, QByteArray::from("destination"));
        roles
    }

    fn append_row(mut self: Pin<&mut Self>, process: QString, destination: QString) {
        let row = self.entries.len() as i32;
        // begin/endInsertRows keep QML views in sync.
        unsafe {
            self.as_mut()
                .begin_insert_rows(&QModelIndex::default(), row, row);
        }
        self.as_mut()
            .rust_mut()
            .entries
            .push((process.to_string(), destination.to_string()));
        unsafe {
            self.as_mut().end_insert_rows();
        }

        let new_count = self.entries.len() as i32;
        self.as_mut().set_count(new_count);
        // Spike evidence on stdout (throwaway instrumentation).
        println!(
            "[spike] append_row on Qt thread: row {new_count} = {} -> {}",
            process.to_string(),
            destination.to_string()
        );
        // Proof point 1: emit the Qt signal from the Qt thread.
        self.connection_added(process, destination);
    }

    fn start_feed(mut self: Pin<&mut Self>) {
        if self.running {
            return;
        }
        self.as_mut().set_running(true);

        // Proof point 1: a real Tokio runtime on a background OS thread queues
        // work back onto the Qt/GUI thread via CxxQtThread.
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("tokio runtime");
            rt.block_on(async move {
                for i in 0..5 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let process = format!("proc-{i}");
                    let destination = format!("10.0.0.{}:443", i + 1);
                    let _ = qt_thread.queue(move |mut qobject| {
                        qobject
                            .as_mut()
                            .append_row(QString::from(&process), QString::from(&destination));
                    });
                }
            });
        });
    }
}
