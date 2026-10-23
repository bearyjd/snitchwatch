//! The recorder: dedupe, the unsaved part versus the totals, the writer thread
//! and what the banner reads. A double stands in for the file.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use super::*;
use crate::learned::path_key;
use crate::learned::store::{Loaded, WriteReport};

const T0: i64 = 1_000;

fn row(path: &str, host: &str) -> PendingRow {
    PendingRow {
        process: "p".into(),
        process_path: Some(path.into()),
        dst_host: host.into(),
        dst_ip: "93.184.216.34".into(),
        port: 443,
        protocol: "tcp".into(),
    }
}

/// An in-memory file.
#[derive(Default)]
struct Fake {
    stored: Mutex<Aggregate>,
    writes: AtomicUsize,
    fail: AtomicBool,
    degraded: AtomicBool,
    /// While set, `merge_write` waits for this to be taken.
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: Mutex<Option<Sender<()>>>,
    cleared: AtomicBool,
}

impl Backend for Fake {
    fn load(&self, _now: i64) -> Loaded {
        Loaded {
            aggregate: self.stored.lock().unwrap().clone(),
            notice: None,
        }
    }

    fn merge_write(&self, delta: &Aggregate, _now: i64) -> Result<WriteReport, Notice> {
        if let Some(entered) = self.entered.lock().unwrap().as_ref() {
            let _ = entered.send(());
        }
        if let Some(gate) = self.gate.lock().unwrap().as_ref() {
            let _ = gate.recv_timeout(Duration::from_secs(10));
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(Notice::WriteFailed);
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut stored = self.stored.lock().unwrap();
        *stored = std::mem::take(&mut *stored).merge(delta.clone());
        Ok(WriteReport {
            lock_degraded: self.degraded.load(Ordering::SeqCst),
            notice: None,
        })
    }

    fn clear(&self) -> Result<(), Notice> {
        self.cleared.store(true, Ordering::SeqCst);
        *self.stored.lock().unwrap() = Aggregate::default();
        Ok(())
    }
}

/// A recorder with a quick debounce, attached to a fresh double.
fn recorder() -> (Arc<Shared>, Arc<Fake>) {
    let shared = Shared::new(Duration::from_millis(30));
    let fake = Arc::new(Fake::default());
    shared.attach(fake.clone());
    (shared, fake)
}

fn stored_connections(fake: &Fake, key: &str) -> u64 {
    fake.stored
        .lock()
        .unwrap()
        .c
        .get(&path_key(key))
        .map_or(0, |p| p.connections)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// --- dedupe and the totals --------------------------------------------------------

#[test]
fn a_row_is_counted_once_however_often_it_is_recorded() {
    let shared = Shared::new(Duration::from_secs(60));
    let r = row("/usr/bin/curl", "example.com");
    assert!(shared.record("3:ask-1", &r, T0));
    assert!(!shared.record("3:ask-1", &r, T0 + 1), "a repeat");
    assert!(shared.record("3:ask-2", &r, T0 + 2));
    // The session is part of the id: the same wire id in another session is another row.
    assert!(shared.record("4:ask-1", &r, T0 + 3));
    let snapshot = shared.snapshot();
    assert_eq!(snapshot.c[&path_key("/usr/bin/curl")].connections, 3);
    assert_eq!(shared.status().programs_seen, 1);
}

#[test]
fn the_dedupe_set_is_bounded_and_forgets_the_oldest_first() {
    let shared = Shared::new(Duration::from_secs(60));
    let r = row("/usr/bin/curl", "example.com");
    for n in 0..(SEEN_CAP + 10) {
        shared.record(&format!("1:row-{n}"), &r, T0);
    }
    assert!(
        !shared.record(&format!("1:row-{}", SEEN_CAP + 9), &r, T0),
        "recent: still deduped"
    );
    assert!(shared.record("1:row-0", &r, T0), "the oldest was forgotten");
}

#[test]
fn the_status_reports_what_the_banner_shows() {
    let shared = Shared::new(Duration::from_secs(60));
    assert_eq!(shared.status(), Status::default());
    shared.record("1:a", &row("/usr/bin/curl", "example.com"), T0);
    shared.record("1:b", &row("/tmp/x/evil", "example.com"), T0);
    shared.record(
        "1:c",
        &PendingRow {
            dst_host: String::new(),
            ..row("/usr/bin/nc", "")
        },
        T0,
    );
    shared.note_lagged();
    let status = shared.status();
    assert_eq!(status.programs_seen, 3);
    assert!(status.incomplete);
    assert_eq!(status.dropped, 0);
    assert_eq!(status.notice, None);
}

#[test]
fn a_new_window_forgets_the_old_one_and_its_dedupe() {
    let shared = Shared::new(Duration::from_secs(60));
    let r = row("/usr/bin/curl", "example.com");
    shared.record("1:a", &r, T0);
    shared.note_lagged();
    shared.reset();
    assert_eq!(shared.status(), Status::default());
    assert!(shared.snapshot().is_empty());
    assert!(
        shared.record("1:a", &r, T0),
        "counted again in the new window"
    );
}

// --- saving: the unsaved part, never the total ------------------------------------------

#[test]
fn flushing_again_and_again_never_counts_a_connection_twice() {
    let (shared, fake) = recorder();
    let r = row("/usr/bin/curl", "example.com");
    for n in 0..3 {
        shared.record(&format!("1:a{n}"), &r, T0);
    }
    for _ in 0..3 {
        assert!(shared.flush_and_wait(Duration::from_secs(10)));
    }
    assert_eq!(stored_connections(&fake, "/usr/bin/curl"), 3, "x1, not x3");
    shared.record("1:b", &r, T0 + 1);
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert_eq!(stored_connections(&fake, "/usr/bin/curl"), 4);
    // The totals shown are unchanged by saving.
    assert_eq!(
        shared.snapshot().c[&path_key("/usr/bin/curl")].connections,
        4
    );
}

#[test]
fn recording_is_saved_by_itself_after_the_debounce() {
    let (shared, fake) = recorder();
    let r = row("/usr/bin/curl", "example.com");
    for n in 0..20 {
        shared.record(&format!("1:a{n}"), &r, T0);
    }
    wait_for("the debounced write", || {
        stored_connections(&fake, "/usr/bin/curl") == 20
    });
    assert!(
        fake.writes.load(Ordering::SeqCst) <= 3,
        "a burst is saved in a few writes, not one per row: {}",
        fake.writes.load(Ordering::SeqCst)
    );
}

#[test]
fn a_failed_write_keeps_what_was_not_saved_and_says_so_until_it_works() {
    let (shared, fake) = recorder();
    let r = row("/usr/bin/curl", "example.com");
    fake.fail.store(true, Ordering::SeqCst);
    shared.record("1:a", &r, T0);
    shared.record("1:b", &r, T0);
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert_eq!(shared.status().notice, Some(Notice::WriteFailed));
    assert_eq!(stored_connections(&fake, "/usr/bin/curl"), 0);
    assert_eq!(shared.status().programs_seen, 1, "still shown");

    fake.fail.store(false, Ordering::SeqCst);
    shared.record("1:c", &r, T0);
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert_eq!(
        stored_connections(&fake, "/usr/bin/curl"),
        3,
        "nothing was lost"
    );
    assert_eq!(shared.status().notice, None);
}

#[test]
fn a_write_without_the_lock_is_shown() {
    let (shared, fake) = recorder();
    fake.degraded.store(true, Ordering::SeqCst);
    shared.record("1:a", &row("/usr/bin/curl", "example.com"), T0);
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert!(shared.status().lock_degraded);
}

#[test]
fn what_is_stored_is_loaded_into_the_totals_but_not_saved_again() {
    let shared = Shared::new(Duration::from_millis(30));
    let fake = Arc::new(Fake::default());
    {
        let mut earlier = Aggregate::default();
        for p in ["/usr/bin/a", "/usr/bin/b", "/usr/bin/c"] {
            earlier.absorb(Program::observed(&row(p, "example.com"), T0 - 500));
        }
        *fake.stored.lock().unwrap() = earlier;
    }
    shared.attach(fake.clone());
    shared.request_load();
    shared.record("1:x", &row("/usr/bin/a", "example.com"), T0);
    wait_for("the load", || shared.status().programs_seen == 3);
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    // Only the new row was added to the file, not the loaded totals again.
    assert_eq!(stored_connections(&fake, "/usr/bin/a"), 2);
    assert_eq!(stored_connections(&fake, "/usr/bin/b"), 1);
}

#[test]
fn clear_empties_the_totals_the_dedupe_and_the_file() {
    let (shared, fake) = recorder();
    let r = row("/usr/bin/curl", "example.com");
    shared.record("1:a", &r, T0);
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    shared.clear();
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert!(fake.cleared.load(Ordering::SeqCst));
    assert!(fake.stored.lock().unwrap().is_empty());
    assert_eq!(shared.status(), Status::default());
    assert!(shared.record("1:a", &r, T0), "forgotten");
}

// --- off the feed task ---------------------------------------------------------------------

#[test]
fn recording_never_waits_for_the_disk() {
    let (shared, fake) = recorder();
    let (release, gate) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    *fake.gate.lock().unwrap() = Some(gate);
    *fake.entered.lock().unwrap() = Some(entered_tx);
    let r = row("/usr/bin/curl", "example.com");
    shared.record("1:first", &r, T0);
    // The writer is now inside the (blocked) write.
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    let started = Instant::now();
    for n in 0..200 {
        assert!(shared.record(&format!("1:more{n}"), &r, T0));
    }
    let _ = shared.status();
    shared.note_lagged();
    shared.flush(); // asking for a save is not waiting for it either
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "recording waited for the blocked disk: {:?}",
        started.elapsed()
    );
    assert_eq!(
        stored_connections(&fake, "/usr/bin/curl"),
        0,
        "still blocked"
    );

    drop(release);
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert_eq!(stored_connections(&fake, "/usr/bin/curl"), 201);
}

#[test]
fn waiting_for_a_blocked_save_gives_up_after_the_timeout() {
    let (shared, fake) = recorder();
    let (release, gate) = mpsc::channel();
    *fake.gate.lock().unwrap() = Some(gate);
    shared.record("1:a", &row("/usr/bin/curl", "example.com"), T0);
    assert!(!shared.flush_and_wait(Duration::from_millis(100)));
    drop(release);
}

#[test]
fn quitting_saves_what_the_debounce_has_not_yet_written() {
    // A long debounce: only the explicit flush (what `main` does on quit)
    // can have written anything.
    let shared = Shared::new(Duration::from_secs(600));
    let fake = Arc::new(Fake::default());
    shared.attach(fake.clone());
    let r = row("/usr/bin/curl", "example.com");
    shared.record("1:a", &r, T0);
    shared.record("1:b", &r, T0);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(stored_connections(&fake, "/usr/bin/curl"), 0, "nothing yet");
    assert!(shared.flush_and_wait(Duration::from_secs(10)));
    assert_eq!(stored_connections(&fake, "/usr/bin/curl"), 2);
}
