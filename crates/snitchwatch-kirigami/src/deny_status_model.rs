//! `DenyStatusModel`: the Daemon Health page's deny-mode section, from the
//! tower helper's snapshot (`crate::deny_status`). Display only: it reads two
//! small files when QML asks it to refresh and never writes, runs or asks for
//! anything. The paths are properties so tests point it at fixtures; `main.qml`
//! leaves the production defaults.

use core::pin::Pin;
use std::path::Path;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QDateTime, QString};

use crate::deny_status::{load, read_boot_id, texts, BOOT_ID_PATH, COMMANDS, STATUS_PATH};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        /// The snapshot file read on `refresh()`.
        #[qproperty(QString, status_path, cxx_name = "statusPath")]
        /// This boot's id, compared with the snapshot's `bootId`.
        #[qproperty(QString, boot_id_path, cxx_name = "bootIdPath")]
        /// `unknown` / `allow` / `trial` / `overdue` / `confirmed` /
        /// `unconfirmed` / `other`.
        #[qproperty(QString, state)]
        /// Fixed sentence for the state.
        #[qproperty(QString, headline)]
        /// Countdown, overdue or unknown-reason sentence; may be empty.
        #[qproperty(QString, detail)]
        /// "as of HH:MM" from `updatedAt`, empty when unknown.
        #[qproperty(QString, as_of, cxx_name = "asOf")]
        /// The helper's blocker lines, plain text; empty when none.
        #[qproperty(QString, blockers_text, cxx_name = "blockersText")]
        /// The `ujust` commands to copy, as a JSON array of strings.
        #[qproperty(QString, commands_json, cxx_name = "commandsJson")]
        type DenyStatusModel = super::DenyStatusModelRust;

        /// Re-read the snapshot and recompute against the clock.
        #[qinvokable]
        fn refresh(self: Pin<&mut DenyStatusModel>);
    }
}

/// Rust-side state for [`qobject::DenyStatusModel`].
pub struct DenyStatusModelRust {
    status_path: QString,
    boot_id_path: QString,
    state: QString,
    headline: QString,
    detail: QString,
    as_of: QString,
    blockers_text: QString,
    commands_json: QString,
}

impl Default for DenyStatusModelRust {
    fn default() -> Self {
        Self {
            status_path: QString::from(STATUS_PATH),
            boot_id_path: QString::from(BOOT_ID_PATH),
            state: QString::from("unknown"),
            headline: QString::from("Deny mode: unknown"),
            detail: QString::default(),
            as_of: QString::default(),
            blockers_text: QString::default(),
            commands_json: QString::from(
                &serde_json::to_string(&COMMANDS).expect("fixed strings serialize"),
            ),
        }
    }
}

impl qobject::DenyStatusModel {
    fn refresh(mut self: Pin<&mut Self>) {
        let status_path = self.rust().status_path.to_string();
        let boot_id_path = self.rust().boot_id_path.to_string();
        let now = QDateTime::current_date_time();
        let now_ms = now.to_msecs_since_epoch();
        let boot_id = read_boot_id(Path::new(&boot_id_path));
        let view = load(Path::new(&status_path), boot_id.as_deref(), now_ms);
        let shown = texts(&view, |ms| local_time(&now, ms));
        self.as_mut().set_state(QString::from(shown.state));
        self.as_mut().set_headline(QString::from(shown.headline));
        self.as_mut().set_detail(QString::from(&shown.detail));
        self.as_mut().set_as_of(QString::from(&shown.as_of));
        self.as_mut()
            .set_blockers_text(QString::from(&shown.blockers));
    }
}

/// `ms` in Qt's local time (as `tray_controller` reads it): "HH:MM" on
/// today's date, else "yyyy-MM-dd HH:MM".
fn local_time(now: &QDateTime, ms: i64) -> String {
    let mut local = now.clone();
    local.set_msecs_since_epoch(ms);
    let time = local.time().format(&QString::from("HH:mm")).to_string();
    if local.date() == now.date() {
        time
    } else {
        let date = local.date().format(&QString::from("yyyy-MM-dd"));
        format!("{date} {time}")
    }
}
