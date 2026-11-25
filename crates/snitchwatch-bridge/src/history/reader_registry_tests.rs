use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

const LONG: Duration = Duration::from_secs(10);

/// A worker that holds `ticket` until told to let go.
fn hold(ticket: ReaderTicket) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let _ticket = ticket;
        let _ = rx.recv();
    });
    (tx, handle)
}

#[test]
fn two_queries_run_and_the_third_is_refused_at_once() {
    let registry = ReaderRegistry::new();
    let a = registry.try_acquire().unwrap();
    let b = registry.try_acquire().unwrap();
    // Never queued: this returns here and now, with nobody to release a slot.
    assert_eq!(registry.try_acquire().err(), Some(NotAdmitted::Busy));
    assert_eq!(registry.in_use(), 2);
    drop(a);
    let c = registry.try_acquire().unwrap();
    drop((b, c));
    assert_eq!(registry.in_use(), 0);
}

#[test]
fn a_cancelled_query_keeps_its_slot_until_its_worker_has_exited() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let token = ticket.cancel_token();
    let (release, worker) = hold(ticket);
    let stuck = registry.quiesce(Duration::from_millis(30));
    assert!(token.is_cancelled());
    assert_eq!(registry.in_use(), 1, "cancelled, not yet stopped");
    let Err(stuck) = stuck else {
        panic!("idle only when the worker has returned")
    };
    stuck.give_up();
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(registry.wait_idle(LONG));
    assert_eq!(registry.in_use(), 0);
}

#[test]
fn the_slot_returns_when_the_worker_panics() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let worker = std::thread::spawn(move || {
        let _ticket = ticket;
        panic!("worker panic");
    });
    assert!(worker.join().is_err());
    assert!(registry.wait_idle(LONG));
    assert_eq!(registry.in_use(), 0);
    registry.try_acquire().unwrap();
}

#[test]
fn closing_refuses_new_readers_with_its_own_reason_until_reopened() {
    let registry = ReaderRegistry::new();
    let guard = registry.quiesce(LONG).ok().unwrap();
    assert!(registry.is_closed());
    assert_eq!(
        registry.try_acquire().err(),
        Some(NotAdmitted::ControlOperation),
        "not Busy: the client must not retry-spin during a clear"
    );
    drop(guard);
    assert!(!registry.is_closed());
    registry.try_acquire().unwrap();
}

#[test]
fn the_quiesce_order_stops_an_in_flight_reader_before_wait_idle_returns() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let token = ticket.cancel_token();
    let worker = std::thread::spawn(move || {
        let _ticket = ticket;
        // A reader that only stops because it was cancelled.
        let deadline = Instant::now() + LONG;
        while !token.is_cancelled() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        token.is_cancelled()
    });
    let guard = registry.quiesce(LONG);
    assert!(guard.is_ok(), "the worker stopped");
    assert!(
        worker.join().unwrap(),
        "it stopped because it was cancelled"
    );
}

#[test]
fn wait_idle_times_out_on_a_stuck_worker() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let (release, worker) = hold(ticket);
    let started = Instant::now();
    assert!(!registry.wait_idle(Duration::from_millis(50)));
    assert!(started.elapsed() >= Duration::from_millis(50));
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(registry.wait_idle(LONG));
}

#[test]
fn cancelling_interrupts_a_running_statement() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let token = ticket.cancel_token();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ticket = ticket;
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        token.attach(conn.get_interrupt_handle());
        // Tells the test the statement is running (SQLite clears an interrupt
        // that arrives while no statement is).
        let mut told = false;
        conn.progress_handler(
            1000,
            Some(move || {
                if !told {
                    told = true;
                    let _ = started_tx.send(());
                }
                false
            }),
        )
        .unwrap();
        // Runs for minutes unless interrupted.
        conn.query_row(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) \
             SELECT count(*) FROM c",
            [],
            |r| r.get::<_, i64>(0),
        )
    });
    started_rx.recv().unwrap();
    assert!(registry.quiesce(LONG).is_ok());
    let result = worker.join().unwrap();
    assert!(result.is_err(), "interrupted, not completed");
    assert!(registry.wait_idle(LONG));
}

#[test]
fn a_new_query_and_a_disconnect_each_cancel_the_previous_one() {
    let registry = ReaderRegistry::new();
    let connection = registry.connection();
    let first = connection.begin().unwrap();
    let first_token = first.cancel_token();
    let second = connection.begin().unwrap();
    let second_token = second.cancel_token();
    assert!(first_token.is_cancelled(), "replaced");
    assert!(!second_token.is_cancelled());
    drop(connection);
    assert!(second_token.is_cancelled(), "the connection went away");
    // The cancelled queries still hold their slots until they are dropped.
    assert_eq!(registry.in_use(), 2);
    drop((first, second));
    assert_eq!(registry.in_use(), 0);
}

#[test]
fn another_connections_query_is_not_cancelled() {
    let registry = ReaderRegistry::new();
    let one = registry.connection();
    let other = registry.connection();
    let a = one.begin().unwrap();
    let b = other.begin().unwrap();
    drop(one);
    assert!(a.cancel_token().is_cancelled());
    assert!(!b.cancel_token().is_cancelled());
}

#[test]
fn two_response_slots_then_none_until_one_is_released() {
    let slots = ResponseSlots::new();
    let a = slots.acquire().unwrap();
    let b = slots.acquire().unwrap();
    assert!(slots.acquire().is_none(), "memory is bounded");
    drop(a);
    let c = slots.acquire().unwrap();
    assert!(!b.reclaimed() && !c.reclaimed());
}

#[test]
fn a_slot_nobody_reads_is_reclaimed_at_the_send_deadline() {
    let slots = ResponseSlots::<()>::with_deadline(2, Duration::from_millis(20));
    let stuck_a = slots.acquire().unwrap();
    let stuck_b = slots.acquire().unwrap();
    let give_up = Instant::now() + LONG;
    let fresh = loop {
        if let Some(slot) = slots.acquire() {
            break slot;
        }
        assert!(Instant::now() < give_up, "the slot was never reclaimed");
        std::thread::yield_now();
    };
    assert!(stuck_a.reclaimed(), "the sender is told to drop its page");
    assert!(!fresh.reclaimed());
    // Late drops of reclaimed slots do not free a slot twice.
    drop((stuck_a, stuck_b));
    let one = slots.acquire().unwrap();
    assert!(slots.acquire().is_none(), "still at most two outstanding");
    drop((fresh, one));
}

/// A page whose drops are counted: reclaiming a slot must free the memory.
struct Counted(Arc<AtomicUsize>);
impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn reclaiming_a_slot_drops_its_page_not_just_a_counter() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let slots = ResponseSlots::<Counted>::with_deadline(1, Duration::from_millis(20));
    let stuck = slots.acquire().unwrap();
    assert!(stuck.fill(Counted(Arc::clone(&dropped))).is_ok());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    let give_up = Instant::now() + LONG;
    let fresh = loop {
        if let Some(slot) = slots.acquire() {
            break slot;
        }
        assert!(Instant::now() < give_up, "never reclaimed");
        std::thread::yield_now();
    };
    // The sender still holds its slot handle, and the page is already gone.
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(stuck.reclaimed());
    assert!(stuck.take().is_none());
    assert!(stuck.fill(Counted(Arc::clone(&dropped))).is_err());
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        2,
        "the refused page came back and dropped"
    );
    drop((stuck, fresh));
}

#[test]
fn a_filled_slot_hands_its_page_to_the_sender_once() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let slots = ResponseSlots::<Counted>::with_deadline(1, LONG);
    let slot = slots.acquire().unwrap();
    assert!(slot.fill(Counted(Arc::clone(&dropped))).is_ok());
    let page = slot.take().expect("the page");
    assert!(slot.take().is_none());
    drop(page);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn a_cancel_before_attach_leaves_the_flag_for_the_reader_to_see() {
    // SQLite forgets an interrupt that arrives while no statement runs, so
    // the flag is what the reader checks before each statement.
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let token = ticket.cancel_token();
    token.cancel();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    token.attach(conn.get_interrupt_handle());
    assert!(token.is_cancelled());
}
