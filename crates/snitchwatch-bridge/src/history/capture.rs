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
//! directory, checksums, the process tree, `uid`, pid, byte counters and the
//! source address and port are never read. (`src_ip` and `src_port` are PR 6.)
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
//!   fabricated gap here; the standing labels are PR 7.
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
//! Counted in memory ([`CaptureStats`]); turning the counters into aggregated
//! gap rows is PR 7. A dropped record's pin is released at once. A batch that
//! fails to commit is lost, its pins are released **except the oldest**, which
//! is kept so the watermark can never pass the first lost event.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use snitchwatch_proto::protocol::Event;

use super::admission::{Admission, AdmissionConfig, Refusal};
use super::budget::Budget;
use super::caps::{DstHost, ProcessPath, Protocol, RuleName};
use super::durable::PinnedBatch;
use super::pins::{PinId, PinRegistry};
use super::record::{Action, AnsweredBy, EventRecord};
use super::rule_identity::{identify, RuleClass, RuleIdentity};
use super::snapshots::snapshot_for;
use super::writer::{Engine, Item, QUEUE_MAX_BYTES, QUEUE_MAX_COUNT};
use super::{load_state, locate, open_store, Mode, State};

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
    record.process_path = nonempty(&conn.process_path).map(ProcessPath::new);
    record.dst_host = nonempty(&conn.dst_host).map(DstHost::new);
    record.dst_ip = conn.dst_ip.parse().ok();
    record.dst_port = u16::try_from(conn.dst_port).ok();
    record.protocol = nonempty(&conn.protocol).map(Protocol::new);
    let allow = rule.action == "allow";
    if by_default {
        record.action = if allow {
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
    Some(Mapped {
        record,
        identity,
        nolog: rule.nolog,
    })
}

fn nonempty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

fn same_event(a: &EventRecord, b: &EventRecord) -> bool {
    a.process_path == b.process_path
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

/// Events, not records: each count is the sum of `repeat_count`s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureStats {
    pub seen: u64,
    pub skipped_nolog: u64,
    pub refused_process_rate: u64,
    pub refused_byte_share: u64,
    pub refused_global_rate: u64,
    pub dropped_queue: u64,
    pub dropped_pin: u64,
    pub written: u64,
    pub lost_write: u64,
    /// Matched-rule events whose rule has no identity (recorded without a
    /// hash).
    pub unidentified_rules: u64,
}

#[derive(Default)]
struct Counters {
    seen: AtomicU64,
    skipped_nolog: AtomicU64,
    refused_process_rate: AtomicU64,
    refused_byte_share: AtomicU64,
    refused_global_rate: AtomicU64,
    dropped_queue: AtomicU64,
    dropped_pin: AtomicU64,
    written: AtomicU64,
    lost_write: AtomicU64,
    unidentified_rules: AtomicU64,
}

fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    pub queue_max_bytes: usize,
    pub queue_max_count: usize,
    pub admission: AdmissionConfig,
}

impl CaptureConfig {
    pub fn for_budget(budget: &Budget) -> Self {
        Self {
            queue_max_bytes: QUEUE_MAX_BYTES,
            queue_max_count: QUEUE_MAX_COUNT,
            admission: AdmissionConfig::for_retained_bytes(budget.main_bytes),
        }
    }
}

/// How many queues this process has allocated: stays 0 while history is off.
static QUEUES_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

pub fn queues_allocated() -> usize {
    QUEUES_ALLOCATED.load(Ordering::Relaxed)
}

type Entry = (Item, PinId);

/// [`super::writer::Queue`] with a pin riding along with each item. A
/// bounded channel, so a producer never takes a lock the writer holds and
/// never waits: `try_send` either queues or says why not. The byte bound is
/// an atomic reservation made before the send.
struct PinnedQueue {
    tx: SyncSender<Entry>,
    rx: Mutex<Option<Receiver<Entry>>>,
    bytes: AtomicUsize,
    queued: AtomicUsize,
    max_bytes: usize,
    closed: AtomicBool,
}

impl PinnedQueue {
    fn new(max_bytes: usize, max_count: usize) -> Self {
        QUEUES_ALLOCATED.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::sync_channel(max_count);
        Self {
            tx,
            rx: Mutex::new(Some(rx)),
            bytes: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            max_bytes,
            closed: AtomicBool::new(false),
        }
    }

    /// Never waits. Gives the item and its pin back when it cannot queue.
    fn try_push(&self, item: Item, pin: PinId) -> Result<(), Box<Entry>> {
        let size = item.queue_bytes();
        if self.closed.load(Ordering::Acquire) {
            return Err(Box::new((item, pin)));
        }
        if self.bytes.fetch_add(size, Ordering::AcqRel) + size > self.max_bytes {
            self.bytes.fetch_sub(size, Ordering::AcqRel);
            return Err(Box::new((item, pin)));
        }
        self.queued.fetch_add(1, Ordering::AcqRel);
        self.tx.try_send((item, pin)).map_err(|e| {
            self.bytes.fetch_sub(size, Ordering::AcqRel);
            self.queued.fetch_sub(1, Ordering::AcqRel);
            match e {
                TrySendError::Full(entry) | TrySendError::Disconnected(entry) => Box::new(entry),
            }
        })
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    fn len(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    /// The writer's end, once.
    fn take_receiver(&self) -> Option<Receiver<Entry>> {
        self.rx.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Up to `max` entries, waiting at most `wait` for the first. `None`
    /// once closed and drained.
    fn pop_batch(&self, rx: &Receiver<Entry>, max: usize, wait: Duration) -> Option<PinnedBatch> {
        let mut entries: Vec<Entry> = Vec::new();
        match rx.recv_timeout(wait) {
            Ok(first) => entries.push(first),
            Err(RecvTimeoutError::Timeout) => {
                // `close` precedes the last drain: nothing queues after it.
                return if self.closed.load(Ordering::Acquire) && self.len() == 0 {
                    None
                } else {
                    Some(PinnedBatch {
                        items: Vec::new(),
                        pins: Vec::new(),
                    })
                };
            }
            Err(RecvTimeoutError::Disconnected) => return None,
        }
        while entries.len() < max {
            match rx.try_recv() {
                Ok(next) => entries.push(next),
                Err(_) => break,
            }
        }
        let (items, pins): (Vec<Item>, Vec<PinId>) = entries.into_iter().unzip();
        let freed: usize = items.iter().map(Item::queue_bytes).sum();
        self.bytes.fetch_sub(freed, Ordering::AcqRel);
        self.queued.fetch_sub(items.len(), Ordering::AcqRel);
        Some(PinnedBatch { items, pins })
    }
}

struct Shared {
    registry: Arc<PinRegistry>,
    queue: PinnedQueue,
    admission: Mutex<Admission>,
    counters: Counters,
    #[cfg(test)]
    probe: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    fn new(registry: Arc<PinRegistry>, config: CaptureConfig) -> Self {
        Self {
            registry,
            queue: PinnedQueue::new(config.queue_max_bytes, config.queue_max_count),
            admission: Mutex::new(Admission::new(config.admission)),
            counters: Counters::default(),
            #[cfg(test)]
            probe: Mutex::new(None),
        }
    }

    fn stats(&self) -> CaptureStats {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        CaptureStats {
            seen: get(&c.seen),
            skipped_nolog: get(&c.skipped_nolog),
            refused_process_rate: get(&c.refused_process_rate),
            refused_byte_share: get(&c.refused_byte_share),
            refused_global_rate: get(&c.refused_global_rate),
            dropped_queue: get(&c.dropped_queue),
            dropped_pin: get(&c.dropped_pin),
            written: get(&c.written),
            lost_write: get(&c.lost_write),
            unidentified_rules: get(&c.unidentified_rules),
        }
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

    /// Admit, pin and queue one record.
    fn enqueue(&self, mapped: Mapped, now_ms: i64) {
        let n = u64::from(mapped.record.repeat_count);
        let counters = &self.counters;
        let bytes = match self.admit(&mapped.record, now_ms) {
            Ok(bytes) => bytes,
            Err(Refusal::ProcessRate) => return add(&counters.refused_process_rate, n),
            Err(Refusal::ByteShare) => return add(&counters.refused_byte_share, n),
            Err(Refusal::GlobalRate) => return add(&counters.refused_global_rate, n),
        };
        let Mapped {
            record, identity, ..
        } = mapped;
        let snapshot = identity
            .as_ref()
            .and_then(|i| snapshot_for(i, record.ts_ms).ok());
        let item = Item {
            event: record,
            snapshot,
        };
        let pin = match self.registry.accept() {
            Ok(pin) => pin,
            Err(_) => {
                self.refund(&item.event, bytes);
                return add(&counters.dropped_pin, n);
            }
        };
        if let Err(entry) = self.queue.try_push(item, pin) {
            let (item, pin) = *entry;
            // Nothing will ever commit it: its pin must not hold the
            // watermark back.
            let _ = self.registry.release(&pin);
            self.refund(&item.event, bytes);
            add(&counters.dropped_queue, n);
        }
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
            self.enqueue(record, now_ms);
        }
    }

    /// The writer thread: commit pinned batches until the queue is closed and
    /// empty.
    fn run_writer(&self, mut engine: Engine, rx: Receiver<Entry>) {
        let rows = engine.budget().rows_per_txn as usize;
        // The oldest pin of the first batch that failed, held for good.
        let mut held: Option<PinId> = None;
        while let Some(batch) = self.queue.pop_batch(&rx, rows, Duration::from_millis(200)) {
            if batch.items.is_empty() {
                continue;
            }
            let events: u64 = batch
                .items
                .iter()
                .map(|i| u64::from(i.event.repeat_count))
                .sum();
            match engine.commit_pinned(&batch, &self.registry) {
                Ok(_) => add(&self.counters.written, events),
                Err(e) => {
                    add(&self.counters.lost_write, events);
                    tracing::error!(error = %e, lost = batch.items.len(), "history write failed");
                    self.keep_oldest_pin(batch.pins, &mut held);
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    /// Releases `pins` but the oldest, unless one is already held (it is
    /// older: pins arrive in order).
    fn keep_oldest_pin(&self, mut pins: Vec<PinId>, held: &mut Option<PinId>) {
        if held.is_none() && !pins.is_empty() {
            *held = Some(pins.remove(0));
        }
        let refs: Vec<&PinId> = pins.iter().collect();
        let _ = self.registry.release_all(&refs);
    }
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
        let shared = Arc::new(Shared::new(registry, config));
        let rx = shared
            .queue
            .take_receiver()
            .expect("a new queue has its receiver");
        let worker = shared.clone();
        let handle = std::thread::Builder::new()
            .name("history-writer".into())
            .spawn(move || worker.run_writer(engine, rx))?;
        Ok(Self {
            shared,
            writer: Mutex::new(Some(handle)),
        })
    }

    /// The decided events of one `Ping`. Call with no lock held; never waits.
    pub fn observe(&self, events: &[&Event]) {
        self.shared.observe(events);
    }

    pub fn stats(&self) -> CaptureStats {
        self.shared.stats()
    }

    /// Writes what is queued, stops the writer and returns the final counts.
    pub fn shutdown(self) -> CaptureStats {
        self.stop();
        self.shared.stats()
    }

    fn stop(&self) {
        self.shared.queue.close();
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
            shared: Arc::new(Shared::new(registry, config)),
            writer: Mutex::new(None),
        }
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
