//! PR 10: the control gate. Every destructive or mode-changing operation on
//! history passes through here, in one fixed order:
//!
//! 1. close read admission and wait for the readers to stop
//!    ([`ReaderRegistry::quiesce`], which fails closed: a reader that will
//!    not stop means **nothing** changes);
//! 2. persist the new mode. This is the only step before the point of no
//!    return that can be refused, so it comes first: a refusal then changes
//!    nothing, and a prompt that was being answered is not stranded;
//! 3. in one short step under `Shared::control` (never held across anything
//!    slow): advance the generation, which only the quiesced registry's
//!    guard can do, and stop accepting. Nothing after this is refused for
//!    the capture that was running;
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
    /// More events were accepted after the preview than a clear carries
    /// into the new store.
    #[error("too many events were recorded since the preview to keep; preview again")]
    TooManyCarried,
    /// The operation was refused after the writer had stopped, and putting
    /// recording back failed too: history may now be off.
    #[error("{refused}; recording could not be put back: {reason}")]
    RollbackFailed { refused: String, reason: String },
    /// A delete passed the point of no return (the store was taken out of
    /// place) and then failed. History is off and stays off; what could not
    /// be finished is named, so a client can say "history was removed;
    /// settings could not be removed".
    #[error("history was removed but the delete did not finish: {0}; recording stays off")]
    Incomplete(String),
    /// A removal passed the rename but the tombstone could not be unlinked
    /// (or a retry could not unlink it): the rows in it are still on disk
    /// and retrying the delete finishes the job. The text says whether
    /// recording stays off; a clear resumes it on a new store.
    #[error("deleted history is still on disk; retry delete: {0}")]
    DataLeft(String),
    /// A clear replaced the store and then could not record on it. `kept`
    /// rows accepted after the preview are in the new store.
    #[error(
        "history was cleared; {kept} rows accepted since the preview were kept; \
         recording could not be resumed: {reason}"
    )]
    ClearedNotResumed { kept: usize, reason: String },
    /// A clear replaced the store but the directory could not be synced, so
    /// a power loss may bring the old store back.
    #[error("history was cleared, but the change may not survive a power loss: {0}")]
    ClearedNotDurable(String),
    /// A clear's rename failed and what is under the store's name could not be
    /// told: the history may or may not be cleared. Never "nothing changed".
    #[error("cannot tell whether the history was cleared: {0}")]
    ClearOutcomeUnknown(String),
}

/// How long a control operation waits for readers: one wait and one
/// extension (owner decision 8), then it fails closed.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub quiesce: Duration,
    pub extension: Duration,
    /// How long a preview token stays valid.
    pub token_ttl: Duration,
    /// How long one statement of the advisory preview scan may run. The
    /// authoritative recheck at confirm is not bounded by it.
    pub statement: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            quiesce: Duration::from_secs(3),
            extension: Duration::from_secs(3),
            token_ttl: Duration::from_secs(60),
            statement: Duration::from_millis(100),
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
    writer_lock: Mutex<Option<Arc<WriterLock>>>,
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
    .map(|capture| capture.holding(inner.shared_writer_lock()))
    .map_err(|e| e.to_string())
}

impl Inner {
    /// The writer lock, shared with a capture so that it is held for as long
    /// as the capture's writer can write, whatever happens to the gate.
    fn shared_writer_lock(&self) -> Option<Arc<WriterLock>> {
        self.writer_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Takes the writer lock if the history directory exists (it is never
/// created here), and only **then** finishes what a crashed removal left:
/// another bridge's half-done clear is not ours to finish. With
/// `needs_history` a directory that holds no store, settings or tombstone
/// yet (an enabling bridge has just made it) is not taken either.
pub(super) fn lock_and_recover(
    location: &Location,
    needs_history: bool,
) -> Result<Option<Arc<WriterLock>>, String> {
    if !matches!(super::open::dir_exists(location.dir()), Ok(true)) {
        return Ok(None);
    }
    let dir =
        StoreDir::open(location.dir(), super::open::effective_uid()).map_err(|e| e.to_string())?;
    if needs_history && !dir.has_history().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let held = dir.lock_writer().map_err(|e| e.to_string())?;
    control_files::recover(location);
    Ok(Some(Arc::new(held)))
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
            match lock_and_recover(location, false) {
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
        match self.0.op.try_lock() {
            Ok(held) => Ok(held),
            // A panicked operation holds nothing else: the poison is a flag.
            Err(std::sync::TryLockError::Poisoned(p)) => Ok(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => Err(Refused::InProgress),
        }
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
        consent
            .validate()
            .map_err(|e| Refused::Settings(e.to_string()))?;
        // The directory and the lock first: consent is only written by the
        // bridge that owns the store.
        super::open::ensure_dir(location.dir(), super::open::effective_uid())
            .map_err(|e| Refused::Unavailable(e.to_string()))?;
        self.hold_writer_lock(&location, false)?;
        super::enable(&location, consent).map_err(|e| Refused::Settings(e.to_string()))?;
        if self.0.slot.get().is_none() {
            self.resume();
        }
        match self.0.slot.get() {
            Some(_) => Ok(()),
            None => Err(Refused::Unavailable("history could not start".into())),
        }
    }

    /// Takes the writer lock if this bridge does not hold it yet, and
    /// finishes what a crashed removal left, once, as the lock is first
    /// taken. No directory means history was never enabled here: nothing to
    /// hold, so nothing may be changed.
    /// A bridge that takes the lock here, lazily, also starts recording if
    /// history is enabled: nobody else would. Only an operation that owns
    /// existing history takes it (`needs_history`), never one that finds an
    /// empty directory.
    fn hold_writer_lock(&self, location: &Location, needs_history: bool) -> Result<(), Refused> {
        let took = {
            let mut held = self.0.writer_lock.lock().unwrap_or_else(|e| e.into_inner());
            if held.is_some() {
                false
            } else {
                match lock_and_recover(location, needs_history) {
                    Ok(Some(lock)) => *held = Some(lock),
                    Ok(None) => {
                        return Err(Refused::Unavailable(
                            "history has never been enabled; nothing to change".into(),
                        ))
                    }
                    Err(reason) => return Err(Refused::Unavailable(reason)),
                }
                true
            }
        };
        if took && self.0.slot.get().is_none() {
            self.resume();
        }
        Ok(())
    }

    /// The location, for an operation other than `enable`: only the bridge
    /// that holds the writer lock may preview, clear, delete or disable.
    pub(super) fn owned_location(&self) -> Result<Location, Refused> {
        let location = self.location()?.clone();
        self.hold_writer_lock(&location, true)?;
        Ok(location)
    }

    /// Ownership is re-checked after every wait inside an operation. The lock
    /// is held for the life of the bridge once taken, so this fails only if
    /// that invariant is ever broken.
    pub(super) fn check_owner(&self) -> Result<(), Refused> {
        let held = self.0.writer_lock.lock().unwrap_or_else(|e| e.into_inner());
        match *held {
            Some(_) => Ok(()),
            None => Err(Refused::Unavailable(
                "the writer lock is not held by this bridge".into(),
            )),
        }
    }

    /// Step 3: advance the generation and stop accepting, through the
    /// capture if there is one.
    pub(super) fn close(
        &self,
        guard: &ControlGuard,
        capture: Option<&Arc<Capture>>,
    ) -> Result<(), Refused> {
        match capture {
            Some(capture) => capture.close_admission(guard, &self.0.generation),
            None => guard.advance(&self.0.generation).map(|_| ()),
        }
        .map_err(|_| Refused::Unavailable("generation counter exhausted".into()))
    }

    /// Stops recording and keeps history (owner decision 10): prompts still
    /// pending become one `disabled` gap, the queue is written, the session
    /// ends `disabled`, and `settings.json` keeps its consent with
    /// `enabled = false`. The settings are saved before anything is closed,
    /// so a refusal changes nothing, and before the writer stops, so a crash
    /// between the two never restarts recording on a user who disabled it.
    pub fn disable(&self) -> Result<(), Refused> {
        let _op = self.begin()?;
        let location = self.owned_location()?;
        let guard = self.quiesce()?;
        self.check_owner()?;
        let capture = self.0.slot.get();
        let prior = persist_off(&location, capture.is_none(), false)?;
        if let Err(refused) = self.close(&guard, capture.as_ref()) {
            restore_settings(&location, prior);
            return Err(refused);
        }
        if let Some(capture) = capture {
            self.0.slot.set(None);
            capture.quiesce_writer(true);
        }
        drop(guard);
        Ok(())
    }
}

/// Saves `enabled = false`, everything else kept, and returns the settings
/// that were there (to put back). Creates nothing: no settings file means
/// nothing to turn off. With `tolerate_unreadable`, settings that cannot be
/// read are left as they are instead of refusing: there is nothing to
/// preserve, and what the user wants (off, or gone) does not depend on them.
/// With `revoke_consent` (a delete) the consent record goes in the same write.
pub(super) fn persist_off(
    location: &Location,
    tolerate_unreadable: bool,
    revoke_consent: bool,
) -> Result<Option<Settings>, Refused> {
    let prior = match settings::load(location) {
        Ok(Some(prior)) => prior,
        Ok(None) => return Ok(None),
        Err(e) if tolerate_unreadable => {
            tracing::warn!(error = %e, "history settings are unreadable; leaving them");
            return Ok(None);
        }
        Err(e) => return Err(Refused::Settings(e.to_string())),
    };
    let off = Settings {
        enabled: false,
        consent: if revoke_consent {
            None
        } else {
            prior.consent.clone()
        },
        ..prior.clone()
    };
    settings::save(location, &off).map_err(|e| Refused::Settings(e.to_string()))?;
    Ok(Some(prior))
}

/// Puts back what [`persist_off`] replaced, when nothing else has changed.
pub(super) fn restore_settings(location: &Location, prior: Option<Settings>) {
    if let Some(prior) = prior {
        if let Err(e) = settings::save(location, &prior) {
            tracing::error!(error = %e, "history settings could not be put back");
        }
    }
}
