use super::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
fn prepared() -> PreparedExport {
    PreparedExport {
        artifact: Arc::from("{}"),
        wire: Arc::from("{}"),
    }
}
#[tokio::test(start_paused = true)]
async fn expired_private_slot() {
    let session = ExportSession::new();
    let run = session
        .admit("r", Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(run.publish(prepared()));
    drop(run);
    assert!(matches!(
        session.admit("next", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    let pending = session.next_reply().await.unwrap();
    assert!(matches!(
        session.admit("next", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(!pending.is_current());
    // Even an expired in-flight send owns its slot until completion/drop.
    assert!(matches!(
        session.admit("next", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    drop(pending);
    let next = session
        .admit("next", Instant::now() + Duration::from_secs(10))
        .unwrap();
    session.cancel("next");
    assert!(matches!(
        next.control().check(),
        Err(ExportError::Cancelled)
    ));
    assert!(!next.publish(prepared()));
    assert!(matches!(
        session.admit("replacement", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    drop(next);
    assert!(session
        .admit("replacement", Instant::now() + Duration::from_secs(10))
        .is_ok());
}
#[tokio::test]
async fn cancellation_and_disconnect_hold_admission_until_worker_exit() {
    let session = ExportSession::new();
    let run = session
        .admit("r", Instant::now() + Duration::from_secs(10))
        .unwrap();
    session.cancel("other");
    assert!(run.control().check().is_ok());
    let guard = session.drop_guard();
    drop(guard);
    assert!(matches!(run.control().check(), Err(ExportError::Cancelled)));
    assert!(!run.publish(prepared()));
    assert!(session.next_reply().await.is_none());
    drop(run);
    assert!(matches!(
        session.admit("new", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Unavailable)
    ));
}

#[tokio::test]
async fn completed_delivery_does_not_allow_a_second_publish_from_same_worker() {
    let session = ExportSession::new();
    let run = session
        .admit("r", Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(run.publish(prepared()));
    let pending = session.next_reply().await.unwrap();
    drop(pending);
    assert!(!run.publish(prepared()));
    assert!(matches!(
        session.admit("next", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    drop(run);
    assert!(session
        .admit("next", Instant::now() + Duration::from_secs(10))
        .is_ok());
}

#[tokio::test(start_paused = true)]
async fn queued_expiry_removes_bytes_and_stale_generations_cannot_publish() {
    let session = ExportSession::new();
    let expiry_session = session.clone();
    let expiry = tokio::spawn(async move { expiry_session.expire_queued_replies().await });
    let run = session
        .admit("r", Instant::now() + Duration::from_secs(10))
        .unwrap();
    let reply = prepared();
    let bytes = Arc::downgrade(&reply.wire);
    assert!(run.publish(reply));
    tokio::task::yield_now().await;
    assert!(bytes.upgrade().is_some());
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert!(bytes.upgrade().is_none());
    assert!(matches!(
        session.admit("new", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    assert!(!run.publish(prepared()));
    drop(run);
    let next = session
        .admit("next", Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(next.publish(prepared()));
    session.cancel("r");
    let pending = session.next_reply().await.unwrap();
    assert!(pending.is_current());
    session.cancel("next");
    assert!(!pending.is_current());
    drop(next);
    assert!(matches!(
        session.admit("new", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    drop(pending);
    assert!(session
        .admit("new", Instant::now() + Duration::from_secs(10))
        .is_ok());
    session.close();
    expiry.await.unwrap();
}

#[tokio::test]
async fn expired_admission_and_cancel_never_release_an_existing_worker() {
    let session = ExportSession::new();
    let run = session
        .admit("r", Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(matches!(
        ExportSession::new().admit("expired", Instant::now() - Duration::from_secs(1)),
        Err(ExportError::TimedOut)
    ));
    session.cancel("r");
    assert!(!run.publish(prepared()));
    assert!(matches!(
        session.admit("next", Instant::now() + Duration::from_secs(10)),
        Err(ExportError::Busy)
    ));
    drop(run);
    assert!(session
        .admit("next", Instant::now() + Duration::from_secs(10))
        .is_ok());
}

#[tokio::test]
async fn identical_request_ids_still_have_distinct_private_slots() {
    let a = ExportSession::new();
    let b = ExportSession::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let run_a = a.admit("same-id", deadline).unwrap();
    let run_b = b.admit("same-id", deadline).unwrap();
    let wire_a: Arc<str> = Arc::from("private-wire-A");
    let wire_b: Arc<str> = Arc::from("private-wire-B");
    assert!(run_a.publish(PreparedExport {
        artifact: Arc::from("artifact-A"),
        wire: wire_a.clone()
    }));
    assert!(Arc::ptr_eq(&a.queued_wire().unwrap(), &wire_a));
    assert!(b.queued_wire().is_none());
    assert!(run_b.publish(PreparedExport {
        artifact: Arc::from("artifact-B"),
        wire: wire_b.clone()
    }));
    assert!(Arc::ptr_eq(&a.queued_wire().unwrap(), &wire_a));
    assert!(Arc::ptr_eq(&b.queued_wire().unwrap(), &wire_b));
    a.cancel("same-id");
    assert!(a.queued_wire().is_none());
    assert!(Arc::ptr_eq(&b.queued_wire().unwrap(), &wire_b));
    let pending_b = b.next_reply().await.unwrap();
    assert!(pending_b.is_current());
    assert_eq!(&*pending_b.wire, "private-wire-B");
}
