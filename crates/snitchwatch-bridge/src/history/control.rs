//! PR 10: the control gate. Every destructive or mode-changing operation on
//! history passes through here, in one fixed order:
//!
//! 1. close read admission and wait for the readers to stop
//!    ([`ReaderRegistry::quiesce`], which fails closed: a reader that will
//!    not stop means **nothing** changes);
//! 2. in one short step under `Shared::control` (never held across anything
//!    slow): advance the generation, which only the quiesced registry's
//!    guard can do, and stop accepting;
//! 3. persist the new mode;
//! 4. stop the writer and wait for it;
//! 5. file work (clear and delete, see `control_clear`).
//!
//! The order is not the spec's (generation first): a reader admitted between
//! the bump and the close would read the old database under the new
//! generation. One operation runs at a time; a second is refused at once.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use super::budget::Budget;
use super::capture::{Capture, CaptureConfig};
use super::control_files::{self, StoreDir};
use super::control_lock::WriterLock;
use super::lifecycle::{AcceptanceSeq, Generation};
use super::pins::PinRegistry;
use super::reader_registry::{ControlGuard, ReaderRegistry};
use super::writer::Engine;
use super::{load_state, locate, open_store, settings, Location, Mode, Settings, State};

/// Why a control operation did nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("control operation in progress")]
    InProgress,
    #[error("a reader did not stop in time; nothing was changed")]
    ReaderBusy,
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("settings could not be saved: {0}")]
    Settings(String),
    /// The token is unknown, expired, or from an older generation.
    #[error("the preview is no longer valid; preview again")]
    StaleToken,
    /// What the preview counted is not what is stored now.
    #[error("history changed since the preview; preview again")]
    Changed,
    #[error("files: {0}")]
    Files(String),
    /// A reader slot or the scan itself was not available; try again.
    #[error("busy")]
    Busy,
}

/// How long a control operation waits for readers: one wait and one
/// extension (owner decision 8), then it fails closed.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub quiesce: Duration,
    pub extension: Duration,
    /// How long a preview token stays valid.
    pub token_ttl: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            quiesce: Duration::from_secs(3),
            extension: Duration::from_secs(3),
            token_ttl: Duration::from_secs(60),
        }
    }
}

/// The capture callers use. A control operation swaps it; callers take a
/// clone per use and never hold the lock across anything.
#[derive(Clone, Default)]
pub struct CaptureSlot(Arc<RwLock<Option<Arc<Capture>>>>);

impl From<Arc<Capture>> for CaptureSlot {
    /// A slot that never changes.
    fn from(capture: Arc<Capture>) -> Self {
        let slot = Self::default();
        slot.set(Some(capture));
        slot
    }
}

impl CaptureSlot {
    pub fn get(&self) -> Option<Arc<Capture>> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(super) fn set(&self, capture: Option<Arc<Capture>>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = capture;
    }
}

pub(super) struct Inner {
    pub(super) location: Result<Location, String>,
    pub(super) slot: CaptureSlot,
    pub(super) registry: ReaderRegistry,
    pub(super) generation: Arc<Generation>,
    pub(super) acceptance: Arc<AcceptanceSeq>,
    pub(super) limits: Limits,
    /// Serialises operations; never held by anything else.
    op: Mutex<()>,
    /// Held for as long as history lives here, also while it is disabled.
    writer_lock: Mutex<Option<WriterLock>>,
    /// Why history is unavailable (another bridge holds it), if it is.
    pub(super) blocked: Option<String>,
    pub(super) tokens: super::control_preview::Tokens,
}

#[derive(Clone)]
pub struct ControlGate(pub(super) Arc<Inner>);

/// Builds a capture on the existing store, on the bridge-wide counters.
pub(super) fn spawn_capture(
    location: &Location,
    settings: &Settings,
    inner: &Inner,
) -> Result<Capture, String> {
    let budget =
        Budget::new(u64::from(settings.budget_mib) * 1024 * 1024).map_err(|e| e.to_string())?;
    let store = open_store(location).map_err(|e| e.to_string())?;
    let engine = Engine::new(store, location, budget).map_err(|e| e.to_string())?;
    Capture::spawn_shared(
        engine,
        Arc::new(PinRegistry::new()),
        CaptureConfig::for_settings(&budget, settings),
        inner.generation.clone(),
        inner.acceptance.clone(),
    )
    .map_err(|e| e.to_string())
}

impl ControlGate {
    /// The bridge's history: finishes what a crashed removal left, takes the
    /// writer lock, and starts recording if history is enabled. Never fails:
    /// a history that cannot run is `unavailable` and says why.
    pub fn start(mode: Mode, state_dir: Option<&Path>, limits: Limits) -> Self {
        let location = locate(mode, state_dir).map_err(|e| e.to_string());
        let mut blocked = None;
        let mut lock = None;
        if let Ok(location) = &location {
            control_files::recover(location);
            match Self::lock_existing(location) {
                Ok(held) => lock = held,
                Err(reason) => blocked = Some(reason),
            }
        }
        let inner = Inner {
            location,
            slot: CaptureSlot::default(),
            registry: ReaderRegistry::new(),
            generation: Arc::new(Generation::random()),
            acceptance: Arc::new(AcceptanceSeq::default()),
            limits,
            op: Mutex::new(()),
            writer_lock: Mutex::new(lock),
            blocked,
            tokens: Default::default(),
        };
        let gate = Self(Arc::new(inner));
        gate.resume();
        gate
    }

    /// The lock, if the history directory exists (it is never created here).
    fn lock_existing(location: &Location) -> Result<Option<WriterLock>, String> {
        if !matches!(super::open::dir_exists(location.dir()), Ok(true)) {
            return Ok(None);
        }
        StoreDir::open(location.dir(), super::open::effective_uid())
            .map_err(|e| e.to_string())?
            .lock_writer()
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// Starts recording on the existing settings, if history is enabled.
    fn resume(&self) {
        let (Ok(location), None) = (&self.0.location, &self.0.blocked) else {
            return;
        };
        let State::Enabled(settings) = load_state(location) else {
            return;
        };
        match spawn_capture(location, &settings, &self.0) {
            Ok(capture) => self.0.slot.set(Some(Arc::new(capture))),
            Err(reason) => tracing::warn!(%reason, "history is enabled but could not start"),
        }
    }

    pub fn slot(&self) -> CaptureSlot {
        self.0.slot.clone()
    }

    pub fn readers(&self) -> ReaderRegistry {
        self.0.registry.clone()
    }

    pub fn generation(&self) -> Arc<Generation> {
        self.0.generation.clone()
    }

    pub(super) fn location(&self) -> Result<&Location, Refused> {
        if let Some(reason) = &self.0.blocked {
            return Err(Refused::Unavailable(reason.clone()));
        }
        self.0
            .location
            .as_ref()
            .map_err(|reason| Refused::Unavailable(reason.clone()))
    }

    /// The one operation at a time; a second is refused, never queued.
    pub(super) fn begin(&self) -> Result<MutexGuard<'_, ()>, Refused> {
        self.0.op.try_lock().map_err(|e| match e {
            std::sync::TryLockError::WouldBlock => Refused::InProgress,
            std::sync::TryLockError::Poisoned(p) => {
                tracing::error!("a control operation panicked; continuing");
                // The operation holds nothing else: the poison is only a flag.
                let _ = p;
                Refused::InProgress
            }
        })
    }

    /// Step 1: close read admission and wait for the readers, once more if
    /// the first wait ran out. A reader that still does not stop reopens the
    /// registry and refuses the operation with nothing changed.
    pub(super) fn quiesce(&self) -> Result<ControlGuard, Refused> {
        let first = self.0.registry.quiesce(self.0.limits.quiesce);
        let stuck = match first {
            Ok(guard) => return Ok(guard),
            Err(stuck) => stuck,
        };
        match stuck.wait_again(self.0.limits.extension) {
            Ok(guard) => Ok(guard),
            Err(stuck) => {
                stuck.give_up();
                Err(Refused::ReaderBusy)
            }
        }
    }

    /// The user's enable action through the gate: writes the settings with
    /// the consent record (creating the history directory), takes the
    /// writer lock if it is not held yet, and starts recording on the
    /// bridge-wide counters.
    pub fn enable(&self, consent: super::Consent) -> Result<(), Refused> {
        let _op = self.begin()?;
        let location = self.location()?.clone();
        super::enable(&location, consent).map_err(|e| Refused::Settings(e.to_string()))?;
        self.holdwriter_lock(&location)?;
        if self.0.slot.get().is_none() {
            self.resume();
        }
        match self.0.slot.get() {
            Some(_) => Ok(()),
            None => Err(Refused::Unavailable("history could not start".into())),
        }
    }

    fn holdwriter_lock(&self, location: &Location) -> Result<(), Refused> {
        let mut held = self.0.writer_lock.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_none() {
            let dir = StoreDir::open(location.dir(), super::open::effective_uid())
                .map_err(|e| Refused::Unavailable(e.to_string()))?;
            *held = Some(
                dir.lock_writer()
                    .map_err(|e| Refused::Unavailable(e.to_string()))?,
            );
        }
        Ok(())
    }

    /// Stops recording and keeps history (owner decision 10): prompts still
    /// pending become one `disabled` gap, the queue is written, the session
    /// ends `disabled`, and `settings.json` keeps its consent with
    /// `enabled = false`. The settings are saved before the writer stops, so
    /// a crash between the two never restarts recording on a user who
    /// disabled it.
    pub fn disable(&self) -> Result<(), Refused> {
        let _op = self.begin()?;
        let location = self.location()?.clone();
        let guard = self.quiesce()?;
        let capture = self.0.slot.get();
        let saved = match &capture {
            Some(capture) => capture
                .close_admission(&guard, &self.0.generation)
                .map_err(|_| Refused::Unavailable("generation counter exhausted".into()))?,
            None => {
                guard
                    .advance(&self.0.generation)
                    .map_err(|_| Refused::Unavailable("generation counter exhausted".into()))?;
                None
            }
        };
        if let Err(reason) = save_disabled(&location) {
            if let Some(capture) = &capture {
                capture.restore_consent(saved);
            }
            return Err(reason);
        }
        if let Some(capture) = capture {
            self.0.slot.set(None);
            capture.quiesce_writer(true);
        }
        drop(guard);
        Ok(())
    }
}

/// `enabled = false`, everything else kept.
pub(super) fn save_disabled(location: &Location) -> Result<(), Refused> {
    let current = settings::load(location)
        .map_err(|e| Refused::Settings(e.to_string()))?
        .unwrap_or_default();
    let next = Settings {
        enabled: false,
        ..current
    };
    settings::save(location, &next).map_err(|e| Refused::Settings(e.to_string()))
}
