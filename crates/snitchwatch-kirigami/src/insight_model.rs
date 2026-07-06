//! `PendingInsight` — the cxx-qt `QObject` backing the pending-decision
//! dialog's insight panel (Parity 2). Thin wrapper over
//! [`crate::insight::client`]'s Qt-free fetch/cache logic, following the same
//! async-dispatch-then-`qt_thread.queue` pattern as
//! `TrafficModel`/`BlocklistsModel`.
//!
//! Lives outside the `insight` module directory (rather than as
//! `insight::model`) because `cxx-qt-build`'s `QmlModule` only supports
//! registering Rust bridge files from a single directory per module (Qt bug
//! QTBUG-93443) — every other cxx-qt bridge file in this crate is a direct
//! child of `src/`, so this one is too; the Qt-free logic it wraps stays in
//! `insight::client`.
//!
//! **Never on the safety-critical path**: `lookup()` always returns
//! immediately — it either applies a cached result synchronously or spawns
//! the network work and returns. Nothing here is ever awaited by the verdict
//! submission path (`PendingDecision::submit`), so a hung/offline lookup can
//! never delay or block an Allow/Deny decision.

use core::pin::Pin;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cxx_qt::Threading;
use cxx_qt_lib::QString;

use crate::insight::client::{resolve, InsightResult, InsightSource, RealInsightSource};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Reverse-DNS + RDAP insight panel, bound by `PendingDecisionSheet.qml`.
        #[qobject]
        #[qml_element]
        /// Whether a lookup is currently in flight for the most recent `lookup()` call.
        #[qproperty(bool, loading)]
        /// Whether there is anything at all to show (drives the QML
        /// "unavailable (offline?)" fallback once loading has finished).
        #[qproperty(bool, available)]
        #[qproperty(QString, hostname)]
        #[qproperty(QString, org)]
        #[qproperty(QString, registrar)]
        #[qproperty(QString, country)]
        type PendingInsight = super::PendingInsightRust;

        /// Look up `ip` (reverse DNS + RDAP), applying a cached result
        /// synchronously or spawning the (timeout-bounded) network work.
        /// A no-op for an empty string. Never blocks.
        #[qinvokable]
        fn lookup(self: Pin<&mut PendingInsight>, ip: &QString);
    }

    impl cxx_qt::Threading for PendingInsight {}
}

/// Rust-side state for [`qobject::PendingInsight`].
pub struct PendingInsightRust {
    loading: bool,
    available: bool,
    hostname: QString,
    org: QString,
    registrar: QString,
    country: QString,
    /// Per-IP session cache, shared with the spawned lookup tasks via `Arc`
    /// so a `lookup()` call for an IP already in flight/cached elsewhere
    /// doesn't need to bounce through the Qt thread to consult it.
    cache: Arc<Mutex<HashMap<String, InsightResult>>>,
    source: Arc<dyn InsightSource>,
}

impl Default for PendingInsightRust {
    fn default() -> Self {
        Self {
            loading: false,
            available: false,
            hostname: QString::default(),
            org: QString::default(),
            registrar: QString::default(),
            country: QString::default(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            source: Arc::new(RealInsightSource::new()),
        }
    }
}

impl qobject::PendingInsight {
    fn lookup(mut self: Pin<&mut Self>, ip: &QString) {
        let ip = ip.to_string();
        if ip.is_empty() {
            return;
        }

        // Fast synchronous path: already cached (e.g. re-opening the
        // inspector for the same remote host) — no spinner flash. The lock
        // guard is dropped before `apply()` so the mutable borrow below is
        // never live at the same time as the immutable one.
        let cached = self.cache.lock().unwrap().get(&ip).cloned();
        if let Some(cached) = cached {
            self.as_mut().apply(&cached);
            return;
        }

        self.as_mut().set_loading(true);
        self.as_mut().set_available(false);

        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("PendingInsight: bridge not running; lookup unavailable");
            self.as_mut().set_loading(false);
            return;
        };

        let qt_thread = self.as_mut().qt_thread();
        let source = self.source.clone();
        let cache = self.cache.clone();
        handles.runtime().spawn(async move {
            let result = resolve(source.as_ref(), &cache, &ip).await;
            let _ = qt_thread.queue(move |qobject| {
                qobject.apply(&result);
            });
        });
    }
}

impl qobject::PendingInsight {
    fn apply(mut self: Pin<&mut Self>, result: &InsightResult) {
        self.as_mut().set_loading(false);
        self.as_mut().set_available(result.has_any_data());
        self.as_mut()
            .set_hostname(QString::from(result.hostname.as_deref().unwrap_or("")));
        self.as_mut()
            .set_org(QString::from(result.org.as_deref().unwrap_or("")));
        self.as_mut()
            .set_registrar(QString::from(result.registrar.as_deref().unwrap_or("")));
        self.as_mut()
            .set_country(QString::from(result.country.as_deref().unwrap_or("")));
    }
}
