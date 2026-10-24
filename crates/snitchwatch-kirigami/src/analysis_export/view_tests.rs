use super::*;
use std::time::Duration;

#[test]
fn preview_save_bytes_are_identical() {
    let now = Instant::now();
    let mut view = ExportView::default();
    let artifact: Arc<str> = Arc::from("{\"note\":\"雪\\n\\\"\"}\n");
    view.begin(7, "request-1".into(), now);
    assert!(!view.accept(8, "request-1", artifact.clone(), now));
    assert!(!view.accept(7, "stale", artifact.clone(), now));
    assert!(view.accept(7, "request-1", artifact.clone(), now));
    let saved = view.artifact_for_save(7).unwrap();
    assert!(Arc::ptr_eq(&saved, &artifact));
    assert_eq!(saved.as_bytes(), artifact.as_bytes());
    assert!(view.artifact_for_save(8).is_none());
    assert!(!view.expire(now + Duration::from_secs(60)));
    assert!(Arc::ptr_eq(&view.artifact_for_save(7).unwrap(), &artifact));
    assert!(!view.accept(7, "request-1", Arc::from("replacement"), now));
    view.begin(7, "request-2".into(), now);
    assert!(view.artifact_for_save(7).is_none());
    assert!(!view.accept(7, "request-1", artifact.clone(), now));
    assert!(view.expire(now + Duration::from_secs(15)));
    assert!(!view.accept(7, "request-2", artifact.clone(), now));
    view.begin(7, "request-3".into(), now);
    view.cancel();
    assert!(!view.accept(7, "request-3", artifact.clone(), now));
    view.begin(7, "request-4".into(), now);
    assert!(view.accept(7, "request-4", artifact, now));
    view.disconnect();
    assert!(view.artifact_for_save(7).is_none());
}

#[test]
fn deadline_rejects_reply_without_poll_and_cancel_creates_no_files() {
    let root = tempfile::tempdir().unwrap();
    let now = Instant::now();
    let mut view = ExportView::default();
    view.begin(1, "req".into(), now);
    assert!(!view.accept(1, "req", Arc::from("late"), now + Duration::from_secs(15)));
    assert!(view.artifact_for_save(1).is_none());
    view.cancel();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
