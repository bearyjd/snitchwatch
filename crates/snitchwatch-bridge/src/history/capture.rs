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
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use snitchwatch_proto::protocol::{Connection, Event, Rule};

use super::admission::{Admission, AdmissionConfig, Refusal};
use super::budget::Budget;
use super::caps::{DstHost, ProcessPath, Protocol, RuleName};
pub use super::capture_stats::CaptureStats;
use super::capture_stats::{add, Counters};
use super::durable::PinnedBatch;
use super::gaps::{self, Losses, StandingLabels};
use super::guard::{DecisionGuard, DecisionOutcome};
use super::ledger::{LedgerLimits, LossCounter};
use super::lifecycle::Generation;
use super::pinned_queue::{Entry, PinnedQueue};
use super::pins::{PinId, PinRegistry};
use super::record::{Action, AnsweredBy, EventRecord};
use super::rule_identity::{identify, RuleClass, RuleIdentity};
use super::session::SessionEnd;
use super::snapshots::snapshot_for;
use super::writer::{Engine, Item, QUEUE_MAX_BYTES, QUEUE_MAX_COUNT};
use super::{load_state, locate, open_store, Consent, Mode, State};
use crate::cache::rule_hits::UpstreamLoss;

/// Bytes a record counts for against its process's share, before its strings.
const ROW_OVERHEAD: u64 = 160;
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

fn estimate_bytes(r: &EventRecord) -> u64 {
    let len = |n: Option<usize>| n.unwrap_or(0) as u64;
    ROW_OVERHEAD
        + len(r.process_path.as_ref().map(|v| v.as_str().len()))
        + len(r.dst_host.as_ref().map(|v| v.as_str().len()))
        + len(r.rule_name.as_ref().map(|v| v.as_str().len()))
        + len(r.protocol.as_ref().map(|v| v.as_str().len()))
        + len(r.rule_class.as_ref().map(|v| v.as_str().len()))
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    pub queue_max_bytes: usize,
    pub queue_max_count: usize,
    pub admission: AdmissionConfig,
    /// How often the writer commits a heartbeat and flushes its loss tallies.
    pub heartbeat_every: Duration,
}

impl CaptureConfig {
    pub fn for_budget(budget: &Budget) -> Self {
        Self {
            queue_max_bytes: QUEUE_MAX_BYTES,
            queue_max_count: QUEUE_MAX_COUNT,
            admission: AdmissionConfig::for_retained_bytes(budget.main_bytes),
            heartbeat_every: HEARTBEAT_EVERY,
        }
    }
}

/// How many queues this process has allocated: stays 0 while history is off.
pub fn queues_allocated() -> usize {
    super::pinned_queue::allocated()
}

/// How long the writer waits on a lock held by another connection.
const WRITER_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
/// Wait before the one retry of a failed batch, doubling per lost batch.
const RETRY_BACKOFF_MIN: Duration = Duration::from_millis(100);
const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(2);
/// A failing writer logs at most this often.
const ERROR_LOG_EVERY: Duration = Duration::from_secs(30);
/// How often the writer commits a heartbeat and flushes its loss tallies.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
/// After close the writer drains for at most this long; the rest is lost.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

pub(super) struct Shared {
    // In-memory only. A control operation must change eligibility and/or
    // advance the generation under this gate before clearing queued work.
    control: Mutex<Option<Arc<Consent>>>,
    generation: Generation,
    registry: Arc<PinRegistry>,
    queue: PinnedQueue,
    admission: Mutex<Admission>,
    losses: Mutex<Losses>,
    heartbeat_every: Duration,
    /// A fork default-action event was seen this session (D1's capability).
    fork_marker_seen: AtomicBool,
    no_gui_prompts: AtomicBool,
    /// The writer ends its session as `disabled`, not `clean`.
    disabling: AtomicBool,
    counters: Counters,
    #[cfg(test)]
    probe: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    fn new(registry: Arc<PinRegistry>, config: CaptureConfig, consent: Option<Consent>) -> Self {
        Self {
            control: Mutex::new(consent.map(Arc::new)),
            generation: Generation::new(0),
            registry,
            queue: PinnedQueue::new(config.queue_max_bytes, config.queue_max_count),
            admission: Mutex::new(Admission::new(config.admission)),
            losses: Mutex::new(Losses::new()),
            heartbeat_every: config.heartbeat_every,
            fork_marker_seen: AtomicBool::new(false),
            no_gui_prompts: AtomicBool::new(false),
            disabling: AtomicBool::new(false),
            counters: Counters::default(),
            #[cfg(test)]
            probe: Mutex::new(None),
        }
    }

    fn stats(&self) -> CaptureStats {
        self.counters.snapshot()
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

    fn with_losses(&self, f: impl FnOnce(&mut Losses, &PinRegistry)) {
        f(
            &mut self.losses.lock().unwrap_or_else(|e| e.into_inner()),
            &self.registry,
        );
    }

    /// Admit, pin and queue one record.
    fn enqueue(&self, mapped: Mapped, now_ms: i64, arrival_pin: Option<PinId>) {
        let n = u64::from(mapped.record.repeat_count);
        let counters = &self.counters;
        let bytes = match self.admit(&mapped.record, now_ms) {
            Ok(bytes) => bytes,
            Err(refusal) => {
                let counter = match refusal {
                    Refusal::ProcessRate => &counters.refused_process_rate,
                    Refusal::ByteShare => &counters.refused_byte_share,
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

    pub(super) fn settle(
        &self,
        record: EventRecord,
        consent: &Consent,
        generation: u64,
        pin: PinId,
        outcome: DecisionOutcome<'_>,
    ) {
        let now = self.registry.now();
        let mapped = outcome.map(record, now);
        let control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if control.as_deref() != Some(consent) || self.generation.current() != generation {
            let _ = self.registry.release(&pin);
            return;
        }
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

    /// The writer thread: commit pinned batches until the queue is closed and
    /// empty, or, once closed, until [`SHUTDOWN_DRAIN`] has passed; beat every
    /// [`CaptureConfig::heartbeat_every`]; then flush the loss tallies and end
    /// the session.
    fn run_writer(&self, mut engine: Engine, rx: Receiver<Entry>, mut began: bool) {
        let rows = engine.budget().rows_per_txn as usize;
        // A short lock wait: a locked database costs a batch, not the thread.
        let _ = engine.connection().busy_timeout(WRITER_BUSY_TIMEOUT);
        let limits = LedgerLimits::from_budget(engine.budget());
        let mut pulse = gaps::Pulse::new(self.heartbeat_every);
        let mut backoff = RETRY_BACKOFF_MIN;
        let mut last_logged: Option<Instant> = None;
        let mut deadline: Option<Instant> = None;
        while let Some(batch) = self.queue.pop_batch(&rx, rows, Duration::from_millis(200)) {
            if self.queue.is_closed() {
                let limit = *deadline.get_or_insert_with(|| Instant::now() + SHUTDOWN_DRAIN);
                if Instant::now() >= limit {
                    self.abandon(batch, &rx);
                    break;
                }
            }
            if !began {
                match engine.begin_session(&self.registry, &limits) {
                    Ok(_) => began = true,
                    Err(e) => {
                        // Nothing is committed (not even a heartbeat) over the
                        // previous session; what is queued is lost, loudly.
                        log_rate_limited(&mut last_logged, &e, batch_events(&batch));
                        self.lost_batch(batch);
                        continue;
                    }
                }
            }
            if pulse.due() {
                if let Err(e) = pulse.beat(&mut engine, &self.registry, &limits, &self.losses) {
                    log_rate_limited(&mut last_logged, &e, 0);
                }
            }
            if batch.items.is_empty() {
                continue;
            }
            let mut result = engine.commit_pinned(&batch, &self.registry);
            if result.is_err() {
                // Once closed there is no time to wait: the retry is immediate.
                if !self.queue.is_closed() {
                    std::thread::sleep(backoff);
                }
                result = engine.commit_pinned(&batch, &self.registry);
            }
            let events = batch_events(&batch);
            match result {
                Ok(_) => {
                    add(&self.counters.written, events);
                    backoff = RETRY_BACKOFF_MIN;
                }
                Err(e) => {
                    self.lost_batch(batch);
                    log_rate_limited(&mut last_logged, &e, events);
                    backoff = (backoff * 2).min(RETRY_BACKOFF_MAX);
                }
            }
        }
        self.finish(&mut engine, &limits, began);
    }

    /// The queue is closed and drained: flush every tally (an open daemon
    /// gap ends now) and end the session. A failure leaves the pins held, so
    /// the next start still reconstructs a gap from the watermark.
    fn finish(&self, engine: &mut Engine, limits: &LedgerLimits, began: bool) {
        // Never began: ending would close the previous session as this one's.
        if !began && engine.begin_session(&self.registry, limits).is_err() {
            tracing::error!("history never started its session; the previous one stays open");
            return;
        }
        let kind = if self.disabling.load(Ordering::Acquire) {
            SessionEnd::Disabled
        } else {
            SessionEnd::Clean
        };
        if let Err(e) = gaps::close(engine, &self.registry, limits, &self.losses, kind) {
            tracing::error!(error = %e, "history shutdown could not record its gaps");
        }
    }

    /// The batch could not be written: count it, give its admission bytes
    /// back, and tally it as `write-failed` under its oldest pin, which holds
    /// the watermark until the tally is a gap row (W1).
    fn lost_batch(&self, batch: PinnedBatch) {
        let lost = batch_events(&batch);
        add(&self.counters.lost_write, lost);
        for item in &batch.items {
            self.refund(&item.event, estimate_bytes(&item.event));
        }
        if let Some(oldest) = self.release_all_but_oldest(batch.pins) {
            self.lose(|l| &mut l.write_failed, lost, Some(oldest));
        }
    }

    /// Shutdown ran out of time: everything still queued is lost.
    fn abandon(&self, batch: PinnedBatch, rx: &Receiver<Entry>) {
        let (mut items, mut pins) = (batch.items, batch.pins);
        for (item, pin) in rx.try_iter() {
            items.push(item);
            pins.push(pin);
        }
        self.lost_batch(PinnedBatch { items, pins });
    }

    /// Releases `pins` but the one that arrived first, which it returns.
    fn release_all_but_oldest(&self, mut pins: Vec<PinId>) -> Option<PinId> {
        let arrival = |p: &PinId| self.registry.arrival_of(p).unwrap_or(i64::MAX);
        let i = (0..pins.len()).min_by_key(|&i| arrival(&pins[i]))?;
        let oldest = pins.swap_remove(i);
        let refs: Vec<&PinId> = pins.iter().collect();
        let _ = self.registry.release_all(&refs);
        Some(oldest)
    }
}

/// A failing writer logs at most once per [`ERROR_LOG_EVERY`].
fn log_rate_limited(last: &mut Option<Instant>, error: &dyn std::fmt::Display, lost: u64) {
    if last.is_none_or(|t| t.elapsed() >= ERROR_LOG_EVERY) {
        *last = Some(Instant::now());
        tracing::error!(%error, lost, "history write failed");
    }
}

fn batch_events(batch: &PinnedBatch) -> u64 {
    batch
        .items
        .iter()
        .map(|i| u64::from(i.event.repeat_count))
        .sum()
}

/// History capture: the admission state, the pinned queue and the writer
/// thread. Exists only while history is enabled.
pub struct Capture {
    shared: Arc<Shared>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Capture {
    /// Starts the writer thread on `engine`. Prefer [`start`], which only
    /// does so when history is enabled.
    pub fn spawn(
        engine: Engine,
        registry: Arc<PinRegistry>,
        config: CaptureConfig,
    ) -> io::Result<Self> {
        let mut engine = engine;
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
        let shared = Arc::new(Shared::new(registry, config, engine.consent.clone()));
        let rx = shared
            .queue
            .take_receiver()
            .expect("a new queue has its receiver");
        let worker = shared.clone();
        let handle = std::thread::Builder::new()
            .name("history-writer".into())
            .spawn(move || worker.run_writer(engine, rx, began))?;
        Ok(Self {
            shared,
            writer: Mutex::new(Some(handle)),
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
        let (pin, arrival) = match self.shared.registry.accept_timed() {
            Ok(accepted) => accepted,
            Err(_) => {
                add(&self.shared.counters.dropped_pin, 1);
                self.shared.lose(|l| &mut l.queue, 1, None);
                return None;
            }
        };
        drop(control);
        let mut record = EventRecord::new(arrival, Action::PendingUnanswered, AnsweredBy::Unknown);
        map_connection(&mut record, connection);
        Some(DecisionGuard::new(
            self.shared.clone(),
            record,
            consent,
            generation,
            pin,
        ))
    }

    /// The decided events of one `Ping`. Call with no lock held; never waits.
    pub fn observe(&self, events: &[&Event]) {
        self.shared.observe(events);
    }

    pub fn stats(&self) -> CaptureStats {
        self.shared.stats()
    }

    /// The daemon went away: a `daemon-disconnected` gap opens now. In
    /// memory only; the watchdog calls it on its down edge.
    pub fn daemon_down(&self) {
        self.shared
            .with_losses(|l, registry| l.daemon_down(registry));
    }

    /// The daemon is back: the gap closes now.
    pub fn daemon_up(&self) {
        let now = self.shared.registry.now();
        self.shared.with_losses(|l, _| l.daemon_up(now));
    }

    /// What one ping showed was never received (`upstream-loss`). In memory
    /// only.
    pub fn observe_upstream_loss(&self, loss: UpstreamLoss) {
        self.shared
            .with_losses(|l, registry| l.upstream_loss(registry, loss));
    }

    /// A prompt arrived with no authenticated GUI: no row can exist for it.
    pub fn note_no_gui_prompt(&self) {
        self.shared.no_gui_prompts.store(true, Ordering::Relaxed);
    }

    /// What history cannot observe today (standing notes, not gaps).
    pub fn standing_labels(&self) -> StandingLabels {
        StandingLabels {
            nolog: self.shared.counters.skipped_nolog.load(Ordering::Relaxed) > 0,
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
            shared: Arc::new(Shared::new(registry, config, None)),
            writer: Mutex::new(None),
        }
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
    let State::Enabled(settings) = load_state(&location) else {
        return None;
    };
    let budget = Budget::new(u64::from(settings.budget_mib) * 1024 * 1024).ok()?;
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
        CaptureConfig::for_budget(&budget),
    );
    match capture {
        Ok(capture) => Some(Arc::new(capture)),
        Err(e) => {
            tracing::warn!(error = %e, "history writer thread could not start");
            None
        }
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
