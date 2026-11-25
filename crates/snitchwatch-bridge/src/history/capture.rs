//! PR 5 (design D1, D4, D5): capture of the events the daemon reports on its
//! `Ping`, into the history store.
//!
//! # Where it runs
//!
//! [`Capture::observe`] is called from the `Ping` handler **after** the
//! connection-cache lock and the rules lock are dropped, never under either.
//! It does bounded in-memory work only (map, coalesce, admit, pin, push) and
//! never waits on the writer or the disk: the queue is a bounded channel
//! entered with `try_send` (plus an atomic byte reservation), a full queue
//! drops the record and counts it, and no database I/O happens on the caller.
//! It does briefly take two ordinary mutexes, the admission state and the
//! pin registry's; the writer takes the registry's only for in-memory work
//! before and after a transaction, never across database I/O, and nothing
//! takes the admission state but the caller.
//! The writer thread owns the [`Engine`] and commits through
//! [`Engine::commit_pinned`], so every accepted record holds a pin from its
//! arrival until the transaction that made it durable (W1).
//!
//! # What is captured (D4)
//!
//! Only the allowlisted fields of [`EventRecord`]: the process path, the
//! destination, the protocol, the action and who decided it, the rule's name
//! and class, and its hash. `process_args`, the environment, the working
//! directory, checksums, the process tree, `uid`, pid, and byte counters are
//! never read. Source address and port support prompt-to-ping correlation.
//!
//! # Rules
//!
//! * A matched rule gets its [`identify`] hash and, with the record, its
//!   redacted snapshot. A rule `identify` refuses (an operator type this build
//!   does not model, an over-long name, ...) is **still recorded**, with no
//!   `rule_hash` and no snapshot: the hash is never invented, and nothing about
//!   the rule beyond its name and class is stored, logged or put in an error.
//! * A default-action event has no rule name, hash or class (`default-allow`
//!   or `default-deny`, answered by `default`); the synthetic rule is not
//!   identified.
//! * A rule with `nolog` is not recorded (D1: "not observable"), should the
//!   daemon ever report one. Traffic the daemon reports nothing for (`nolog`
//!   rules, default-applied traffic on a stock daemon) leaves no row and no
//!   fabricated gap here; [`super::gaps::StandingLabels`] says so instead.
//!
//! # Coalescing
//!
//! Consecutive events with the same D4 fields and rule identity, each within
//! 1 s of the first, become one record whose `repeat_count` is their exact
//! number. This is duplicate compression within one `Ping`'s events, not a
//! rate cap: each daemon ping carries events the daemon then forgets, so
//! nothing is held between pings (no timer, no unpinned buffer). Admission
//! (D5, [`super::admission`]) is charged per record.
//!
//! # Losses
//!
//! Counted in memory ([`CaptureStats`]) and tallied per cause, each tally
//! holding a pin; [`super::gaps`] turns the tallies into aggregated gap rows
//! on the writer's heartbeat, at disable and at shutdown. A dropped record's
//! pin is released at once. A batch that fails to commit is tallied as
//! `write-failed` under its oldest pin, so the watermark can never pass the
//! first lost event.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use snitchwatch_proto::protocol::{Connection, Event, Rule};

use super::admission::{Admission, AdmissionConfig, Refusal};
use super::budget::Budget;
use super::caps::{DstHost, ProcessPath, Protocol, RuleName};
pub use super::capture_stats::CaptureStats;
use super::capture_stats::{add, Counters};
use super::control_files::StoreDir;
use super::control_lock::WriterLock;
use super::gaps::{Losses, StandingLabels};
use super::guard::{DecisionGuard, DecisionOutcome};
use super::ledger::{LedgerLimits, LossCounter};
use super::lifecycle::{AcceptanceSeq, Generation};
use super::measure::estimate_bytes;
use super::pending::{PendingGuards, PendingKey};
use super::pinned_queue::PinnedQueue;
use super::pins::{PinId, PinRegistry};
use super::record::{Action, AnsweredBy, EventRecord};
use super::rule_identity::{identify, RuleClass, RuleIdentity};
use super::settings::{Settings, DEFAULT_RETENTION_DAYS};
use super::snapshots::snapshot_for;
use super::writer::{Engine, Item, QUEUE_MAX_BYTES, QUEUE_MAX_COUNT};
use super::{load_state, locate, open_store, Consent, Mode, State};

/// Events whose first is no more than this many ms before them coalesce.
const COALESCE_WINDOW_MS: i64 = 1000;

/// One event, mapped to the allowlist.
pub struct Mapped {
    pub record: EventRecord,
    /// The matched rule's identity; `None` for a default event or a rule
    /// `identify` refused.
    pub identity: Option<RuleIdentity>,
    pub nolog: bool,
}

/// `None` when the event carries no connection or no rule.
pub(crate) fn map_event(event: &Event) -> Option<Mapped> {
    let conn = event.connection.as_ref()?;
    let rule = event.rule.as_ref()?;
    let by_default = crate::daemon_contract::is_default_action_rule(rule);
    let mut record = EventRecord::new(
        event.unixnano / 1_000_000,
        Action::Deny,
        if by_default {
            AnsweredBy::Default
        } else {
            AnsweredBy::Rule
        },
    );
    map_connection(&mut record, conn);
    if by_default {
        record.action = if rule.action == "allow" {
            Action::DefaultAllow
        } else {
            Action::DefaultDeny
        };
        return Some(Mapped {
            record,
            identity: None,
            nolog: rule.nolog,
        });
    }
    Some(map_rule(record, rule))
}

fn map_connection(record: &mut EventRecord, conn: &Connection) {
    record.src_ip = conn.src_ip.parse().ok();
    record.src_port = u16::try_from(conn.src_port).ok().filter(|port| *port != 0);
    record.process_path = nonempty(&conn.process_path).map(ProcessPath::new);
    record.dst_host = nonempty(&conn.dst_host).map(DstHost::new);
    record.dst_ip = conn.dst_ip.parse().ok();
    record.dst_port = u16::try_from(conn.dst_port).ok();
    record.protocol = nonempty(&conn.protocol).map(Protocol::new);
}

pub(super) fn map_rule(mut record: EventRecord, rule: &Rule) -> Mapped {
    let allow = rule.action == "allow";
    let class = RuleClass::of(&rule.name);
    record.action = match (allow, class) {
        (true, _) => Action::Allow,
        (false, RuleClass::Blocklist) => Action::Blocklist,
        (false, _) => Action::Deny,
    };
    record.rule_name = nonempty(&rule.name).map(RuleName::new);
    // `Err` carries no rule text; it only means there is no identity.
    let identity = identify(rule).ok();
    record.rule_hash = identity.as_ref().map(|i| i.hash);
    record.rule_class = Some(super::caps::RuleClass::new(class.as_str()));
    Mapped {
        record,
        identity,
        nolog: rule.nolog,
    }
}

fn nonempty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

fn same_event(a: &EventRecord, b: &EventRecord) -> bool {
    a.src_ip == b.src_ip
        && a.src_port == b.src_port
        && a.process_path == b.process_path
        && a.dst_host == b.dst_host
        && a.dst_ip == b.dst_ip
        && a.dst_port == b.dst_port
        && a.protocol == b.protocol
        && a.action == b.action
        && a.answered_by == b.answered_by
        && a.rule_name == b.rule_name
        && a.rule_hash == b.rule_hash
        && a.rule_class == b.rule_class
}

/// Merge consecutive identical events within the window of the first.
pub(crate) fn coalesce(mapped: Vec<Mapped>) -> Vec<Mapped> {
    let mut out: Vec<Mapped> = Vec::with_capacity(mapped.len());
    for next in mapped {
        match out.last_mut() {
            Some(last)
                if same_event(&last.record, &next.record)
                    && (next.record.ts_ms - last.record.ts_ms).abs() <= COALESCE_WINDOW_MS =>
            {
                last.record.repeat_count = last
                    .record
                    .repeat_count
                    .saturating_add(next.record.repeat_count);
            }
            _ => out.push(next),
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    pub queue_max_bytes: usize,
    pub queue_max_count: usize,
    pub admission: AdmissionConfig,
    /// How often the writer commits a heartbeat and flushes its loss tallies.
    pub heartbeat_every: Duration,
    /// The first pause after eviction could not make room, doubling up to
    /// [`pause_max`](Self::pause_max) (D5).
    pub pause_min: Duration,
    pub pause_max: Duration,
    /// How often the writer runs an age-expiry pass.
    pub expiry_every: Duration,
    /// Age retention, in days (D5).
    pub retention_days: u16,
}

impl CaptureConfig {
    /// The defaults for a store of `_budget` (nothing depends on its size
    /// since the byte share moved to retention).
    pub fn for_budget(_budget: &Budget) -> Self {
        Self {
            queue_max_bytes: QUEUE_MAX_BYTES,
            queue_max_count: QUEUE_MAX_COUNT,
            admission: AdmissionConfig::standard(),
            heartbeat_every: HEARTBEAT_EVERY,
            pause_min: PAUSE_MIN,
            pause_max: PAUSE_MAX,
            expiry_every: EXPIRY_EVERY,
            retention_days: DEFAULT_RETENTION_DAYS,
        }
    }

    /// [`for_budget`](Self::for_budget) with the user's retention.
    pub fn for_settings(budget: &Budget, settings: &Settings) -> Self {
        Self {
            retention_days: settings.retention_days,
            ..Self::for_budget(budget)
        }
    }
}

/// How many queues this process has allocated: stays 0 while history is off.
pub fn queues_allocated() -> usize {
    super::pinned_queue::allocated()
}

/// How often the writer commits a heartbeat and flushes its loss tallies.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
/// Recording pauses this long after eviction could not make room, doubling
/// per failed attempt up to the maximum (D5).
const PAUSE_MIN: Duration = Duration::from_millis(500);
const PAUSE_MAX: Duration = Duration::from_secs(60);
/// Age expiry runs this often: a pass takes at most a tenth of the rows, so
/// a wall clock stepped far forward erodes history slowly, not at once.
const EXPIRY_EVERY: Duration = Duration::from_secs(600);

pub(super) struct Shared {
    // In-memory only. A control operation must change eligibility and/or
    // advance the generation under this gate before clearing queued work.
    control: Mutex<Option<Arc<Consent>>>,
    generation: Arc<Generation>,
    acceptance: Arc<AcceptanceSeq>,
    registry: Arc<PinRegistry>,
    /// Prompts accepted and not yet settled (#420).
    pending: PendingGuards,
    queue: PinnedQueue,
    admission: Mutex<Admission>,
    losses: Mutex<Losses>,
    heartbeat_every: Duration,
    pause_min: Duration,
    pause_max: Duration,
    expiry_every: Duration,
    /// A fork default-action event was seen this session (D1's capability).
    fork_marker_seen: AtomicBool,
    no_gui_prompts: AtomicBool,
    /// The writer ends its session as `disabled`, not `clean`.
    disabling: AtomicBool,
    /// When the shutdown drain ends: set once, by the writer, on the first
    /// batch it takes after the queue closed.
    drain_deadline: OnceLock<Instant>,
    counters: Counters,
    #[cfg(test)]
    probe: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Test seam: run an expiry pass at the writer's next turn.
    #[cfg(test)]
    force_expiry: AtomicBool,
}

impl Shared {
    fn new(
        registry: Arc<PinRegistry>,
        config: CaptureConfig,
        consent: Option<Consent>,
        generation: Arc<Generation>,
        acceptance: Arc<AcceptanceSeq>,
    ) -> Self {
        Self {
            control: Mutex::new(consent.map(Arc::new)),
            generation,
            acceptance,
            registry,
            pending: PendingGuards::default(),
            queue: PinnedQueue::new(config.queue_max_bytes, config.queue_max_count),
            admission: Mutex::new(Admission::new(config.admission)),
            losses: Mutex::new(Losses::new()),
            heartbeat_every: config.heartbeat_every,
            pause_min: config.pause_min,
            pause_max: config.pause_max,
            expiry_every: config.expiry_every,
            fork_marker_seen: AtomicBool::new(false),
            no_gui_prompts: AtomicBool::new(false),
            disabling: AtomicBool::new(false),
            drain_deadline: OnceLock::new(),
            counters: Counters::default(),
            #[cfg(test)]
            probe: Mutex::new(None),
            #[cfg(test)]
            force_expiry: AtomicBool::new(false),
        }
    }

    fn stats(&self) -> CaptureStats {
        self.counters.snapshot()
    }

    /// The shutdown drain's deadline has passed: a pressure pass ends
    /// between its chunks, so the drain is not overrun. While drain time
    /// remains the queue being closed changes nothing: batches are still
    /// evicted-for and written.
    pub(super) fn arm_stop(self: &Arc<Self>, engine: &mut Engine) {
        let shared = self.clone();
        engine.set_stop(Box::new(move || {
            shared
                .drain_deadline
                .get()
                .is_some_and(|deadline| Instant::now() >= *deadline)
        }));
    }

    #[cfg(test)]
    pub(super) fn pending_add_for_test(&self, pin: PinId, arrival_ms: i64) -> PendingKey {
        self.pending.add(pin, arrival_ms)
    }

    #[cfg(test)]
    pub(super) fn pending_len_for_test(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub(super) fn generation_for_test(&self) -> Arc<Generation> {
        self.generation.clone()
    }

    #[cfg(test)]
    pub(super) fn set_drain_deadline_for_test(&self, at: Instant) {
        let _ = self.drain_deadline.set(at);
    }

    #[cfg(test)]
    pub(super) fn close_queue_for_test(&self) {
        self.queue.close();
    }

    fn admit(&self, record: &EventRecord, now_ms: i64) -> Result<u64, Refusal> {
        let bytes = estimate_bytes(record);
        let process = record.process_path.as_ref().map(|p| p.as_str());
        self.admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(process, bytes, now_ms)
            .map(|()| bytes)
    }

    fn refund(&self, record: &EventRecord, bytes: u64) {
        let process = record.process_path.as_ref().map(|p| p.as_str());
        self.admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_bytes(process, bytes);
    }

    /// Tallies `n` events that will never be recorded, holding a pin for the
    /// cause until a gap row is durable. When even that pin cannot be taken
    /// (the registry is exhausted) only the counters know.
    fn lose(&self, pick: impl FnOnce(&mut Losses) -> &mut LossCounter, n: u64, pin: Option<PinId>) {
        let mut losses = self.losses.lock().unwrap_or_else(|e| e.into_inner());
        let tally = pick(&mut losses);
        if let Some(pin) = pin {
            tally.record_pinned(&self.registry, n, pin, None);
        } else {
            let _ = tally.record(&self.registry, n);
        }
    }

    pub(super) fn with_losses(&self, f: impl FnOnce(&mut Losses, &PinRegistry)) {
        f(
            &mut self.losses.lock().unwrap_or_else(|e| e.into_inner()),
            &self.registry,
        );
    }

    /// Admit, pin and queue one record.
    fn enqueue(&self, mapped: Mapped, now_ms: i64, arrival_pin: Option<PinId>) {
        if arrival_pin.is_none() {
            self.number_acceptance();
        }
        let n = u64::from(mapped.record.repeat_count);
        let counters = &self.counters;
        let bytes = match self.admit(&mapped.record, now_ms) {
            Ok(bytes) => bytes,
            Err(refusal) => {
                let counter = match refusal {
                    Refusal::ProcessRate => &counters.refused_process_rate,
                    Refusal::GlobalRate => &counters.refused_global_rate,
                };
                add(counter, n);
                self.lose(|l| &mut l.admission, n, arrival_pin);
                return;
            }
        };
        let Mapped {
            record, identity, ..
        } = mapped;
        let snapshot = identity
            .as_ref()
            .and_then(|i| match snapshot_for(i, record.ts_ms) {
                Ok(snapshot) => Some(snapshot),
                Err(_) => {
                    add(&counters.snapshot_errors, 1);
                    None
                }
            });
        let item = Item {
            event: record,
            snapshot,
        };
        let pin = match arrival_pin.map_or_else(|| self.registry.accept(), Ok) {
            Ok(pin) => pin,
            Err(_) => {
                self.refund(&item.event, bytes);
                add(&counters.dropped_pin, n);
                return self.lose(|l| &mut l.queue, n, None);
            }
        };
        if let Err(entry) = self.queue.try_push(item, pin) {
            let (item, pin) = *entry;
            // Nothing will ever commit it; the loss tally holds a pin for it.
            self.lose(|l| &mut l.queue, n, Some(pin));
            self.refund(&item.event, bytes);
            add(&counters.dropped_queue, n);
        }
    }

    /// Gives one accepted item its number, under the lock a control
    /// operation takes to read the boundary, so the two agree.
    pub(super) fn number_acceptance(&self) {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.acceptance.assign();
    }

    pub(super) fn settle(
        &self,
        record: EventRecord,
        consent: &Consent,
        generation: u64,
        key: PendingKey,
        outcome: DecisionOutcome<'_>,
    ) {
        let now = self.registry.now();
        let mapped = outcome.map(record, now);
        let control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if control.as_deref() != Some(consent) || self.generation.current() != generation {
            // The pin stays pending: the end of this capture, or the control
            // operation that invalidated it, turns it into a gap (#420).
            return;
        }
        // A gap that took the pin already covers this prompt.
        let Some(pin) = self.pending.take(key) else {
            return;
        };
        add(&self.counters.seen, 1);
        if mapped.record.rule_name.is_some() && mapped.identity.is_none() {
            add(&self.counters.unidentified_rules, 1);
        }
        self.enqueue(mapped, now, Some(pin));
    }

    fn observe(&self, events: &[&Event]) {
        #[cfg(test)]
        if let Some(probe) = self
            .probe
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            probe();
        }
        let now_ms = self.registry.now();
        let mut mapped = Vec::with_capacity(events.len());
        for event in events {
            let Some(m) = map_event(event) else { continue };
            add(&self.counters.seen, 1);
            if m.record.answered_by == AnsweredBy::Default {
                self.fork_marker_seen.store(true, Ordering::Relaxed);
            }
            if m.nolog {
                add(&self.counters.skipped_nolog, 1);
            } else {
                if m.record.rule_name.is_some() && m.identity.is_none() {
                    add(&self.counters.unidentified_rules, 1);
                }
                mapped.push(m);
            }
        }
        for record in coalesce(mapped) {
            self.enqueue(record, now_ms, None);
        }
    }
}

/// History capture: the admission state, the pinned queue and the writer
/// thread. Exists only while history is enabled.
pub struct Capture {
    pub(super) shared: Arc<Shared>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The single-writer lock of a capture that [`start`] made; released
    /// only after the writer has been joined (fields drop after `Drop`).
    _lock: Option<WriterLock>,
}

impl Capture {
    /// Starts the writer thread on `engine`. Prefer [`start`], which only
    /// does so when history is enabled.
    pub fn spawn(
        engine: Engine,
        registry: Arc<PinRegistry>,
        config: CaptureConfig,
    ) -> io::Result<Self> {
        Self::spawn_shared(
            engine,
            registry,
            config,
            Arc::new(Generation::random()),
            Arc::new(AcceptanceSeq::default()),
        )
    }

    /// [`spawn`](Self::spawn) on the bridge-wide generation and acceptance
    /// sequence, which outlive this capture (PR 10: a clear or a re-enable
    /// respawns the capture, never the counters).
    pub fn spawn_shared(
        engine: Engine,
        registry: Arc<PinRegistry>,
        config: CaptureConfig,
        generation: Arc<Generation>,
        acceptance: Arc<AcceptanceSeq>,
    ) -> io::Result<Self> {
        let mut engine = engine;
        engine.set_retention_days(config.retention_days);
        let limits = LedgerLimits::from_budget(engine.budget());
        // The previous session's end becomes a `bridge-down` gap now. A
        // failure is retried by the writer before it commits anything: until
        // the session began, a heartbeat or an end would overwrite what the
        // crashed session left.
        let began = match engine.begin_session(&registry, &limits) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(error = %e, "history could not start its session; retrying");
                false
            }
        };
        let shared = Arc::new(Shared::new(
            registry,
            config,
            engine.consent.clone(),
            generation,
            acceptance,
        ));
        let rx = shared
            .queue
            .take_receiver()
            .expect("a new queue has its receiver");
        let worker = shared.clone();
        let handle = std::thread::Builder::new()
            .name("history-writer".into())
            .spawn(move || {
                let mut engine = engine;
                worker.arm_stop(&mut engine);
                worker.run_writer(engine, rx, began)
            })?;
        Ok(Self {
            shared,
            writer: Mutex::new(Some(handle)),
            _lock: None,
        })
    }

    /// Capture eligibility, generation and the arrival pin at prompt acceptance.
    pub fn begin_decision(&self, connection: &Connection) -> Option<DecisionGuard> {
        let control = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let consent = Arc::clone(control.as_ref()?);
        let generation = self.shared.generation.current();
        let _ = self.shared.acceptance.assign();
        let (pin, arrival) = match self.shared.registry.accept_timed() {
            Ok(accepted) => accepted,
            Err(_) => {
                add(&self.shared.counters.dropped_pin, 1);
                self.shared.lose(|l| &mut l.queue, 1, None);
                return None;
            }
        };
        let key = self.shared.pending.add(pin, arrival);
        drop(control);
        let mut record = EventRecord::new(arrival, Action::PendingUnanswered, AnsweredBy::Unknown);
        map_connection(&mut record, connection);
        Some(DecisionGuard::new(
            self.shared.clone(),
            record,
            consent,
            generation,
            key,
        ))
    }

    /// The decided events of one `Ping`. Call with no lock held; never waits.
    pub fn observe(&self, events: &[&Event]) {
        self.shared.observe(events);
    }

    pub fn stats(&self) -> CaptureStats {
        self.shared.stats()
    }

    /// A prompt arrived with no authenticated GUI: no row can exist for it.
    pub fn note_no_gui_prompt(&self) {
        self.shared.no_gui_prompts.store(true, Ordering::Relaxed);
    }

    /// What history cannot observe today (standing notes, not gaps).
    pub fn standing_labels(&self) -> StandingLabels {
        StandingLabels {
            // The daemon never reports `nolog` traffic: stated while history is on.
            nolog: true,
            stock_default_unobservable: !self.shared.fork_marker_seen.load(Ordering::Relaxed),
            no_gui_prompts: self.shared.no_gui_prompts.load(Ordering::Relaxed),
        }
    }

    /// History is switched off: like [`Self::shutdown`], but the session ends
    /// `disabled`, so the next start records a `disabled` gap.
    pub fn disable(self) -> CaptureStats {
        self.shared.disabling.store(true, Ordering::Release);
        self.shutdown()
    }

    /// Writes what is queued, stops the writer and returns the final counts.
    pub fn shutdown(self) -> CaptureStats {
        self.stop();
        self.shared.stats()
    }

    fn stop(&self) {
        {
            let mut control = self
                .shared
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *control = None;
            self.shared.queue.close();
        }
        let handle = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    #[cfg(test)]
    pub(crate) fn new_unstarted_for_test(
        registry: Arc<PinRegistry>,
        config: CaptureConfig,
    ) -> Self {
        Self {
            _lock: None,
            shared: Arc::new(Shared::new(
                registry,
                config,
                None,
                Arc::new(Generation::random()),
                Arc::new(AcceptanceSeq::default()),
            )),
            writer: Mutex::new(None),
        }
    }

    /// What admission's ledger holds for `process`.
    #[cfg(test)]
    pub(super) fn retained_for_test(&self, process: &str) -> u64 {
        self.shared
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retained_for(Some(process))
    }

    #[cfg(test)]
    pub(super) fn close_queue_for_test(&self) {
        self.shared.close_queue_for_test();
    }

    /// Runs an age-expiry pass at the writer's next turn.
    #[cfg(test)]
    pub(super) fn expire_now_for_test(&self) {
        self.shared.force_expiry.store(true, Ordering::SeqCst);
    }

    /// The shared state, for a test's observer on the writer thread.
    #[cfg(test)]
    pub(super) fn shared_for_test(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    #[cfg(test)]
    pub(super) fn with_losses_for_test<R>(&self, f: impl FnOnce(&Losses) -> R) -> R {
        f(&self.shared.losses.lock().unwrap())
    }

    #[cfg(test)]
    pub(crate) fn daemon_gap_open_for_test(&self) -> bool {
        self.with_losses_for_test(|l| l.disconnect.is_pending())
    }

    #[cfg(test)]
    pub(crate) fn queued_for_test(&self) -> usize {
        self.shared.queue.len()
    }

    #[cfg(test)]
    pub(crate) fn set_probe_for_test(&self, probe: Box<dyn Fn() + Send + Sync>) {
        *self.shared.probe.lock().unwrap() = Some(probe);
    }
}

/// Dropping a capture stops the writer the same way: what is queued is
/// written first.
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The bridge's one entry point. Reads the history state (which creates
/// nothing) and returns a running capture **only if history is enabled**;
/// off, unavailable, or a state directory the mode refuses gives `None`: no
/// thread, no directory, no queue.
pub fn start(mode: Mode, state_dir: Option<&Path>) -> Option<Arc<Capture>> {
    let location = locate(mode, state_dir).ok()?;
    super::control_files::recover(&location);
    let State::Enabled(settings) = load_state(&location) else {
        return None;
    };
    let budget = Budget::new(u64::from(settings.budget_mib) * 1024 * 1024).ok()?;
    let lock = match StoreDir::open(location.dir(), super::open::effective_uid())
        .map_err(Into::into)
        .and_then(|dir| dir.lock_writer())
    {
        Ok(lock) => lock,
        Err(reason) => {
            tracing::warn!(%reason, "history is enabled but another bridge holds it");
            return None;
        }
    };
    let store = match open_store(&location) {
        Ok(store) => store,
        Err(reason) => {
            tracing::warn!(%reason, "history is enabled but could not be opened");
            return None;
        }
    };
    let engine = match Engine::new(store, &location, budget) {
        Ok(engine) => engine,
        Err(e) => {
            tracing::warn!(error = %e, "history is enabled but its writer could not start");
            return None;
        }
    };
    let capture = Capture::spawn(
        engine,
        Arc::new(PinRegistry::new()),
        CaptureConfig::for_settings(&budget, &settings),
    );
    match capture {
        Ok(mut capture) => {
            capture._lock = Some(lock);
            Some(Arc::new(capture))
        }
        Err(e) => {
            tracing::warn!(error = %e, "history writer thread could not start");
            None
        }
    }
}

#[path = "capture_control.rs"]
mod control_ops;
#[path = "capture_writer.rs"]
mod writer_loop;

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
