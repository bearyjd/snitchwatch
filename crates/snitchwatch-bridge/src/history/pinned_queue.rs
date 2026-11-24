//! The history writer's input: a bounded queue whose every item carries the
//! pin that holds the watermark until the item is durable (W1).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Mutex;
use std::time::Duration;

use super::durable::PinnedBatch;
use super::pins::PinId;
use super::writer::Item;

/// How many queues this process has allocated.
static QUEUES_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

pub(super) fn allocated() -> usize {
    QUEUES_ALLOCATED.load(Ordering::Relaxed)
}

pub(super) type Entry = (Item, PinId);

/// [`super::writer::Queue`] with a pin riding along with each item. A
/// bounded channel, so a producer never takes a lock the writer holds and
/// never waits: `try_send` either queues or says why not. The byte bound is
/// an atomic reservation made before the send.
pub(super) struct PinnedQueue {
    tx: SyncSender<Entry>,
    rx: Mutex<Option<Receiver<Entry>>>,
    bytes: AtomicUsize,
    queued: AtomicUsize,
    max_bytes: usize,
    closed: AtomicBool,
}

impl PinnedQueue {
    pub(super) fn new(max_bytes: usize, max_count: usize) -> Self {
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
    pub(super) fn try_push(&self, item: Item, pin: PinId) -> Result<(), Box<Entry>> {
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

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(super) fn len(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// The writer's end, once.
    pub(super) fn take_receiver(&self) -> Option<Receiver<Entry>> {
        self.rx.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Up to `max` entries, waiting at most `wait` for the first. `None`
    /// once closed and drained.
    pub(super) fn pop_batch(
        &self,
        rx: &Receiver<Entry>,
        max: usize,
        wait: Duration,
    ) -> Option<PinnedBatch> {
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
