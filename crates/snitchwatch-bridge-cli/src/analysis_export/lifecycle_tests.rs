use super::*;

#[tokio::test]
async fn cancel_timeout_holds_permits_until_actual_worker_exit() {
    let hooks = Arc::new(Hooks::default());
    hooks.stall.store(true, Ordering::Release);
    hooks.force_deadline.store(true, Ordering::Release);
    let _release = Release(hooks.clone());
    let service = AnalysisExports::with_hooks(cache(), hooks.clone());
    let mut a = Peer::new();
    let b = Peer::new();
    let mut c = Peer::new();
    service.try_route(a.request("a"));
    service.try_route(b.request("b"));
    hooks.wait_entered(2).await;
    service.try_route(a.request("duplicate"));
    assert_eq!(a.reason(), Some(ExportError::Busy));
    service.try_route(c.request("third"));
    assert_eq!(c.reason(), Some(ExportError::Busy));
    service.try_route(a.cancel("a"));
    service.try_route(c.request("after-cancel"));
    assert_eq!(c.reason(), Some(ExportError::Busy));
    hooks.deadline_gate.fire();
    let refusal = tokio::time::timeout(Duration::from_secs(1), a.replies.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        refusal,
        ServerMessage::CacheSummaryExportRefused {
            reason: ExportError::TimedOut,
            ..
        }
    ));
    for _ in 0..100 {
        service.try_route(c.request("flood"));
        assert_eq!(c.reason(), Some(ExportError::Busy));
    }
    assert_eq!(service.inner.state.lock().unwrap().jobs.len(), 2);
    assert_eq!(hooks.entered.load(Ordering::Acquire), 2);
    hooks.release();
    finish(&service).await;
    assert_eq!(service.inner.permits.available_permits(), 2);
    assert!(a
        .session
        .admit(
            "after-exit",
            std::time::Instant::now() + Duration::from_secs(1)
        )
        .is_ok());
    assert!(a.replies.try_recv().is_err(), "timeout refused twice");
    hooks.force_deadline.store(false, Ordering::Release);
    service.try_route(c.request("after-exit"));
    finish(&service).await;
    assert!(c.session.next_reply().await.is_some());
}
#[tokio::test]
async fn lock_wait_counts_toward_deadline_without_blocking_the_pump() {
    let cache = cache();
    let locked = cache.lock().await;
    let hooks = Arc::new(Hooks::default());
    let service = AnalysisExports::with_hooks(cache.clone(), hooks.clone());
    let mut peer = Peer::new();
    hooks.force_deadline.store(true, Ordering::Release);
    let before = std::time::Instant::now();
    service.try_route(peer.request("locked"));
    let after = std::time::Instant::now();
    hooks.wait_entered(1).await;
    assert!(service.try_route(ClientMessage::Undo).is_some());
    hooks.deadline_gate.fire();
    let refusal = tokio::time::timeout(Duration::from_secs(1), peer.replies.recv())
        .await
        .unwrap()
        .unwrap();
    let deadline = hooks.deadlines.lock().unwrap()[0];
    assert!(
        deadline >= before + Duration::from_secs(10) && deadline <= after + Duration::from_secs(10)
    );
    assert!(matches!(
        refusal,
        ServerMessage::CacheSummaryExportRefused {
            reason: ExportError::TimedOut,
            ..
        }
    ));
    drop(locked);
    finish(&service).await;
    assert_eq!(hooks.attempts.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn disconnect_and_stop_invalidate_workers_and_pending_artifacts() {
    let hooks = Arc::new(Hooks::default());
    hooks.stall.store(true, Ordering::Release);
    let _release = Release(hooks.clone());
    let service = AnalysisExports::with_hooks(cache(), hooks.clone());
    let peer = Peer::new();
    service.try_route(peer.request("disconnect"));
    hooks.wait_entered(1).await;
    drop(peer.session.drop_guard());
    assert!(peer.session.is_closed());
    hooks.release();
    finish(&service).await;
    assert!(peer.session.next_reply().await.is_none());
    let mut pending = Peer::new();
    service.try_route(pending.request("pending"));
    finish(&service).await;
    let artifact = tokio::time::timeout(Duration::from_secs(1), pending.session.next_reply())
        .await
        .unwrap()
        .unwrap();
    service.try_route(pending.request("pending-duplicate"));
    assert_eq!(pending.reason(), Some(ExportError::Busy));
    service.stop();
    assert!(!artifact.is_current());
    assert!(pending.session.next_reply().await.is_none());
    let mut later = Peer::new();
    service.try_route(later.request("stopped"));
    assert_eq!(later.reason(), Some(ExportError::Unavailable));
}
#[tokio::test]
async fn shutdown_signals_active_jobs_without_releasing_their_admission() {
    let hooks = Arc::new(Hooks::default());
    hooks.stall.store(true, Ordering::Release);
    let _release = Release(hooks.clone());
    let service = AnalysisExports::with_hooks(cache(), hooks.clone());
    let a = Peer::new();
    let b = Peer::new();
    service.try_route(a.request("a"));
    service.try_route(b.request("b"));
    hooks.wait_entered(2).await;
    service.stop();
    assert!(a.session.is_closed() && b.session.is_closed());
    assert_eq!(service.inner.permits.available_permits(), 0);
    assert_eq!(service.inner.state.lock().unwrap().jobs.len(), 2);
    let mut later = Peer::new();
    service.try_route(later.request("later"));
    assert_eq!(later.reason(), Some(ExportError::Unavailable));
    hooks.release();
    finish(&service).await;
    assert!(a.session.next_reply().await.is_none());
    assert!(b.session.next_reply().await.is_none());
    assert_eq!(hooks.active.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn cancellation_interrupts_cache_wait_and_fast_flood_never_accumulates_supervisors() {
    let cache = cache();
    let lock = cache.lock().await;
    let hooks = Arc::new(Hooks::default());
    let service = AnalysisExports::with_hooks(cache.clone(), hooks.clone());
    let mut peer = Peer::new();
    service.try_route(peer.request("lock-cancel"));
    hooks.wait_entered(1).await;
    service.try_route(peer.cancel("lock-cancel"));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), finish(&service))
            .await
            .is_ok()
    );
    assert_eq!(peer.reason(), Some(ExportError::Cancelled));
    drop(lock);
    let mut peers = Vec::new();
    for n in 0..200 {
        let peer = Peer::new();
        service.try_route(peer.request(&format!("flood-{n}")));
        assert!(service.inner.state.lock().unwrap().jobs.len() <= 2);
        peers.push(peer);
        tokio::task::yield_now().await;
    }
    finish(&service).await;
    assert!(hooks.peak.load(Ordering::Acquire) <= 2);
    assert_eq!(hooks.active.load(Ordering::Acquire), 0);
    service.stop();
}

#[tokio::test]
async fn deadline_invalidates_an_already_published_slot_before_refusing() {
    let hooks = Arc::new(Hooks::default());
    hooks.stall_after_publish.store(true, Ordering::Release);
    hooks.force_deadline.store(true, Ordering::Release);
    let _release = Release(hooks.clone());
    let service = AnalysisExports::with_hooks(cache(), hooks.clone());
    let mut peer = Peer::new();
    service.try_route(peer.request("publication-race"));
    let artifact = tokio::time::timeout(Duration::from_secs(1), peer.session.next_reply())
        .await
        .unwrap()
        .unwrap();
    assert!(artifact.is_current());
    hooks.deadline_gate.fire();
    let refusal = tokio::time::timeout(Duration::from_secs(1), peer.replies.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        refusal,
        ServerMessage::CacheSummaryExportRefused {
            reason: ExportError::TimedOut,
            ..
        }
    ));
    assert!(
        !artifact.is_current(),
        "timeout left a published artifact eligible"
    );
    assert_eq!(service.inner.permits.available_permits(), 1);
    hooks.release();
    finish(&service).await;
    assert!(peer.replies.try_recv().is_err());
}

#[tokio::test]
async fn production_deadline_waiter_uses_real_time() {
    let deadline = std::time::Instant::now() + Duration::from_millis(20);
    assert!(tokio::time::timeout(
        Duration::from_secs(1),
        Production.wait_for_deadline(deadline)
    )
    .await
    .is_ok());
    assert!(std::time::Instant::now() >= deadline);
}

#[tokio::test]
async fn same_id_cannot_replace_an_exited_worker_before_its_supervisor_is_reaped() {
    let hooks = Arc::new(Hooks::default());
    hooks.hold_reap.store(true, Ordering::Release);
    hooks.force_deadline.store(true, Ordering::Release);
    let _release = Release(hooks.clone());
    let service = AnalysisExports::with_hooks(cache(), hooks.clone());
    let mut peer = Peer::new();
    service.try_route(peer.request("reused-id"));
    let old = tokio::time::timeout(Duration::from_secs(1), peer.session.next_reply())
        .await
        .unwrap()
        .unwrap();
    drop(old);
    let exited = async {
        loop {
            let notified = hooks.notify.notified();
            if hooks.reaped.load(Ordering::Acquire) >= 1 {
                break;
            }
            notified.await;
        }
    };
    assert!(tokio::time::timeout(Duration::from_secs(1), exited)
        .await
        .is_ok());
    assert_eq!(
        hooks.active.load(Ordering::Acquire),
        0,
        "actual blocking worker must have exited"
    );
    service.try_route(peer.request("reused-id"));
    assert_eq!(peer.reason(), Some(ExportError::Busy));
    // A different service registry cannot bypass the core session guard either.
    let other_service = AnalysisExports::new(cache());
    other_service.try_route(peer.request("reused-id"));
    assert_eq!(peer.reason(), Some(ExportError::Busy));
    hooks.deadline_gate.fire();
    let refusal = tokio::time::timeout(Duration::from_secs(1), peer.replies.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        refusal,
        ServerMessage::CacheSummaryExportRefused {
            reason: ExportError::TimedOut,
            ..
        }
    ));
    hooks.reap_gate.fire();
    finish(&service).await;
    hooks.hold_reap.store(false, Ordering::Release);
    hooks.force_deadline.store(false, Ordering::Release);
    other_service.try_route(peer.request("reused-id"));
    finish(&other_service).await;
    let replacement = tokio::time::timeout(Duration::from_secs(1), peer.session.next_reply())
        .await
        .unwrap()
        .unwrap();
    hooks.deadline_gate.fire();
    assert!(replacement.is_current());
    assert!(peer.replies.try_recv().is_err());
}
