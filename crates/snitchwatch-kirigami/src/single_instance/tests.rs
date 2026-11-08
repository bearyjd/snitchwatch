//! The guard on a private bus (`notification_signals::bus_tests::PrivateBus`,
//! its own `dbus-daemon`), reached through an explicit address: no test sets
//! `DBUS_SESSION_BUS_ADDRESS` (unit tests share one process and run in
//! parallel, so a changed variable could send another test to the real
//! bus), and no code here resolves the session bus on its own. Plain
//! `#[test]`s: the blocking zbus API must not run inside a tokio runtime.
//! Readiness is a handshake: a method call returns only after the owner's
//! handler ran, and `Guard`'s drop returns only after the bus answered.

use super::*;
use crate::notification_signals::bus_tests::PrivateBus;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

/// Short timeouts, for the wedged-owner test only.
const FAST: ForwardPolicy = ForwardPolicy {
    timeout: Duration::from_millis(300),
    retry_delay: Duration::from_millis(50),
    bound: Duration::from_secs(20),
};

/// Happy-path tests: `method_timeout` covers every call on the connection
/// (Hello, RequestName, Activate), so a loaded CI runner needs room.
const ROOMY: ForwardPolicy = ForwardPolicy {
    timeout: Duration::from_secs(5),
    retry_delay: Duration::from_millis(50),
    bound: Duration::from_secs(20),
};

fn counting_sink() -> (Arc<RaiseSink>, Arc<AtomicU32>) {
    let sink = Arc::new(RaiseSink::new());
    let count = Arc::new(AtomicU32::new(0));
    let seen = count.clone();
    sink.register(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    (sink, count)
}

fn env(demo: bool, allow_multiple: bool, bus_address: Option<&str>) -> EnvSnapshot {
    EnvSnapshot {
        demo,
        allow_multiple,
        bus_address: bus_address.map(str::to_string),
    }
}

#[test]
fn the_production_policy_waits_two_seconds_then_retries_once_after_half_a_second() {
    assert_eq!(ForwardPolicy::DEFAULT.timeout, Duration::from_secs(2));
    assert_eq!(ForwardPolicy::DEFAULT.bound, Duration::from_secs(6));
    assert_eq!(
        ForwardPolicy::DEFAULT.retry_delay,
        Duration::from_millis(500)
    );
}

#[test]
fn demo_allow_multiple_and_no_bus_skip_before_any_bus_is_touched() {
    let bus = Some("unix:path=/nonexistent/bus");
    assert_eq!(skip_reason(&env(true, false, bus)), Some(SkipReason::Demo));
    assert_eq!(
        skip_reason(&env(false, true, bus)),
        Some(SkipReason::AllowMultiple)
    );
    assert_eq!(
        skip_reason(&env(false, false, None)),
        Some(SkipReason::NoSessionBus)
    );
    assert_eq!(skip_reason(&env(false, false, bus)), None);
    let outcome = acquire_with(&env(false, false, None), ROOMY, Arc::new(RaiseSink::new()));
    assert!(matches!(
        outcome,
        Outcome::Skipped(SkipReason::NoSessionBus)
    ));
}

#[test]
fn only_exactly_one_allows_multiple_instances() {
    assert!(allow_multiple_flag(Some("1")));
    for off in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("yes"),
        Some(" 1"),
    ] {
        assert!(!allow_multiple_flag(off), "{off:?}");
    }
}

#[test]
fn an_unreachable_bus_runs_anyway() {
    let dir = tempfile::tempdir().unwrap();
    let address = format!("unix:path={}", dir.path().join("bus").display());
    let outcome = acquire_at(&address, ROOMY, Arc::new(RaiseSink::new()));
    assert!(matches!(outcome, Outcome::Skipped(SkipReason::BusError)));
}

#[test]
fn the_session_bus_is_the_variable_else_a_socket_in_the_runtime_dir() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        session_bus_address(Some("unix:path=/x"), None),
        Some("unix:path=/x".into())
    );
    assert_eq!(session_bus_address(Some(""), None), None);
    assert_eq!(session_bus_address(None, None), None);
    assert_eq!(session_bus_address(None, Some(dir.path())), None);
    std::fs::write(dir.path().join("bus"), b"").unwrap();
    assert_eq!(
        session_bus_address(None, Some(dir.path())),
        None,
        "a plain file is no bus"
    );
    std::fs::remove_file(dir.path().join("bus")).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(dir.path().join("bus")).unwrap();
    assert_eq!(
        session_bus_address(None, Some(dir.path())),
        Some(format!("unix:path={}", dir.path().join("bus").display()))
    );
}

#[test]
fn the_first_guard_is_primary_and_a_second_raises_it_exactly_once() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let (sink, raised) = counting_sink();
    let primary = acquire_at(bus.address(), ROOMY, sink);
    assert!(matches!(primary, Outcome::Primary(_)), "{primary:?}");
    let second = acquire_at(bus.address(), ROOMY, Arc::new(RaiseSink::new()));
    assert!(matches!(second, Outcome::Forwarded), "{second:?}");
    assert_eq!(raised.load(Ordering::SeqCst), 1);
}

#[test]
fn an_activate_before_the_window_exists_is_delivered_once_it_does() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let sink = Arc::new(RaiseSink::new());
    let _primary = acquire_at(bus.address(), ROOMY, sink.clone());
    let second = acquire_at(bus.address(), ROOMY, Arc::new(RaiseSink::new()));
    assert!(matches!(second, Outcome::Forwarded), "{second:?}");
    let raised = Arc::new(AtomicU32::new(0));
    let seen = raised.clone();
    sink.register(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(raised.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_the_primary_releases_the_name_for_the_next_launch() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let first = acquire_at(bus.address(), ROOMY, Arc::new(RaiseSink::new()));
    let Outcome::Primary(guard) = first else {
        panic!("not primary: {first:?}");
    };
    // The connection stays open, so only the explicit release can free the name.
    let keep_open = guard.conn.clone();
    drop(guard);
    let next = acquire_at(bus.address(), ROOMY, Arc::new(RaiseSink::new()));
    drop(keep_open);
    assert!(matches!(next, Outcome::Primary(_)), "{next:?}");
}

#[test]
fn retaking_takes_a_name_that_became_free_and_never_steals_one() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let free = connect_serving(bus.address(), ROOMY, Arc::new(RaiseSink::new())).unwrap();
    let outcome = retake(free);
    let Outcome::Primary(holder) = outcome else {
        panic!("a free name was not taken: {outcome:?}");
    };
    let rival = connect_serving(bus.address(), ROOMY, Arc::new(RaiseSink::new())).unwrap();
    let stolen = retake(rival);
    drop(holder);
    assert!(
        matches!(stolen, Outcome::Skipped(SkipReason::OwnerUnresponsive)),
        "{stolen:?}"
    );
}

#[test]
fn a_bus_that_never_completes_the_handshake_cannot_hold_up_start() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("silent");
    // Bound and listening, but nothing ever accepts or answers.
    let _listener = UnixListener::bind(&path).unwrap();
    let policy = ForwardPolicy {
        bound: Duration::from_millis(600),
        ..ROOMY
    };
    let started = Instant::now();
    let outcome = acquire_with(
        &env(false, false, Some(&format!("unix:path={}", path.display()))),
        policy,
        Arc::new(RaiseSink::new()),
    );
    let took = started.elapsed();
    assert!(
        matches!(outcome, Outcome::Skipped(SkipReason::BusError)),
        "{outcome:?}"
    );
    assert!(took >= policy.bound, "returned before the bound: {took:?}");
    assert!(took < Duration::from_secs(5), "not bounded: {took:?}");
}

/// Holds the name and receives `Activate`, but never answers it until told.
struct Wedged {
    calls: Arc<AtomicU32>,
    release: Arc<tokio::sync::Notify>,
}

#[zbus::interface(name = "org.freedesktop.Application")]
impl Wedged {
    async fn activate(&self, _platform_data: HashMap<String, OwnedValue>) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.release.notified().await;
    }
}

#[test]
fn an_owner_that_never_answers_times_out_and_the_second_instance_runs() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let calls = Arc::new(AtomicU32::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let wedged = Wedged {
        calls: calls.clone(),
        release: release.clone(),
    };
    let owner = connection::Builder::address(bus.address())
        .unwrap()
        .serve_at(OBJECT_PATH, wedged)
        .unwrap()
        .build()
        .unwrap();
    let reply = owner
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .unwrap();
    assert_eq!(reply, RequestNameReply::PrimaryOwner);

    let started = Instant::now();
    let outcome = acquire_at(bus.address(), FAST, Arc::new(RaiseSink::new()));
    let took = started.elapsed();
    release.notify_waiters();
    assert!(
        matches!(outcome, Outcome::Skipped(SkipReason::OwnerUnresponsive)),
        "{outcome:?}"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "the owner never saw Activate"
    );
    // Two timeouts and the retry delay, with room for a loaded machine.
    assert!(took >= FAST.timeout * 2, "gave up too early: {took:?}");
    assert!(took < Duration::from_secs(10), "not bounded: {took:?}");
}

/// Holds the name, and on `Activate` gives it up (as an exiting instance
/// would) without ever answering.
struct Departing {
    released: Arc<AtomicU32>,
    hold: Arc<tokio::sync::Notify>,
}

#[zbus::interface(name = "org.freedesktop.Application")]
impl Departing {
    async fn activate(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        _platform_data: HashMap<String, OwnedValue>,
    ) {
        // The release reply arrives before the handler parks, so the name is
        // free well inside the caller's `Activate` timeout. The retry then finds
        // no owner and fails at once, so `retake` can take the name.
        if conn.release_name(BUS_NAME).await.unwrap_or(false) {
            self.released.fetch_add(1, Ordering::SeqCst);
        }
        self.hold.notified().await;
    }
}

#[test]
fn an_owner_that_leaves_without_answering_hands_the_name_to_the_second_instance() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let released = Arc::new(AtomicU32::new(0));
    let hold = Arc::new(tokio::sync::Notify::new());
    let departing = Departing {
        released: released.clone(),
        hold: hold.clone(),
    };
    let owner = connection::Builder::address(bus.address())
        .unwrap()
        .serve_at(OBJECT_PATH, departing)
        .unwrap()
        .build()
        .unwrap();
    let reply = owner
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .unwrap();
    assert_eq!(reply, RequestNameReply::PrimaryOwner);

    // A timeout roomy enough for the owner's release to land inside it.
    let policy = ForwardPolicy {
        timeout: Duration::from_secs(2),
        ..FAST
    };
    let outcome = acquire_at(bus.address(), policy, Arc::new(RaiseSink::new()));
    // A permit, not `notify_waiters`: it also wakes a handler that has not yet
    // polled `notified()`; the bus is torn down with the test either way.
    hold.notify_one();
    assert_eq!(released.load(Ordering::SeqCst), 1, "the owner never let go");
    assert!(
        matches!(outcome, Outcome::Primary(_)),
        "a freed name was not retaken: {outcome:?}"
    );
}

#[test]
fn a_forward_reports_the_unique_name_of_the_owner_that_answered() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let (sink, _raised) = counting_sink();
    let _primary = acquire_at(bus.address(), ROOMY, sink);
    let caller = connect_serving(bus.address(), ROOMY, Arc::new(RaiseSink::new())).unwrap();
    let owner = activate_owner(&caller).unwrap();
    let owner = owner.expect("the Activate reply names its sender");
    assert!(owner.starts_with(':'), "not a unique bus name: {owner}");
}
