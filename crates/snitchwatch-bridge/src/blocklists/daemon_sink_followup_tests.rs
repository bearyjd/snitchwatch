//! Follow-ups to the daemon sink (issue #73), against the same scripted
//! daemon as `daemon_sink_tests`.

use super::tests::{change, delete, hosts, kind_of, Daemon, Harness, ADS};
use super::*;

fn domains_rule() -> String {
    format!("z00-blocklist:{ADS}:domains")
}

fn ips_rule() -> String {
    format!("z00-blocklist:{ADS}:ips")
}

/// A list had domains and ips; a refresh dropped the ips, but the daemon
/// refused the domains rule, so the ips rule and file were left (the install
/// fails before the other kinds are removed). The retry has to finish the job,
/// or the stale `ips.list` keeps blocking hosts the list no longer has.
#[tokio::test]
async fn the_retry_after_a_refused_install_removes_the_kind_the_list_no_longer_has() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    h.sink()
        .replace_blocklist_rules(ADS, hosts(&["ads.example", "203.0.113.7"]))
        .await
        .unwrap();
    assert!(h.dir.has_list(&IdComponent::from_id(ADS), ListKind::Ips));

    // A new run: the daemon holds the ips rule but not the domains rule, and
    // refuses to install it.
    let ips = h.bridge_rule(ADS, ListKind::Ips);
    let h = h.restart().connect(Daemon::RefuseChange("busy"), vec![ips]);
    let sink = h.sink();
    let refused = sink
        .replace_blocklist_rules(ADS, hosts(&["other.example"]))
        .await;
    assert!(refused.is_err(), "the daemon refused the domains rule");
    let list = IdComponent::from_id(ADS);
    assert!(
        h.dir.has_list(&list, ListKind::Ips),
        "still there, as the issue says"
    );

    // The daemon recovers; the reconcile resends the rule over the verified
    // files.
    h.set_daemon(Daemon::Accept);
    sink.reinstall_blocklist_rules(ADS).await.unwrap();

    let sent: Vec<_> = h.seen().iter().map(|s| kind_of(&s.command)).collect();
    assert_eq!(
        sent.last(),
        Some(&delete(&ips_rule())),
        "the ips rule is deleted: {sent:?}"
    );
    assert!(sent.contains(&change(&domains_rule())));
    assert!(
        !h.dir.has_list(&list, ListKind::Ips),
        "and its file with it"
    );
    assert!(h.dir.has_list(&list, ListKind::Domains));
}

// --- Quick unsubscribe, then resubscribe (issue #73) ---------------------------
//
// opensnitchd reloads a list path at most every 30 s and clears a list whose
// file is missing, so deleting the files with the rule left a resubscribed list
// empty for up to 30 s. Unsubscribing now deletes the rules and keeps the
// files for a while; a resubscribe finds them in place.

use std::os::unix::fs::MetadataExt;
use std::time::Duration;

fn inode(h: &Harness, kind: ListKind) -> u64 {
    let list = IdComponent::from_id(ADS);
    let path = h.dir.kind_dir(&list, kind).join(kind.file_name());
    std::fs::metadata(path).unwrap().ino()
}

#[tokio::test]
async fn unsubscribing_deletes_the_rules_but_keeps_the_files_for_a_while() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example", "203.0.113.7"]))
        .await
        .unwrap();
    let installs = h.seen().len();
    sink.release_blocklist_rules(ADS).await.unwrap();

    let names: Vec<_> = h
        .seen()
        .split_off(installs)
        .iter()
        .map(|s| kind_of(&s.command))
        .collect();
    assert_eq!(names, vec![delete(&domains_rule()), delete(&ips_rule())]);
    let list = IdComponent::from_id(ADS);
    assert!(h.dir.has_list(&list, ListKind::Domains));
    assert!(h.dir.has_list(&list, ListKind::Ips));
    assert!(!sink.is_current(ADS), "no rule, so not current");
    assert!(
        !sink.files_verified(ADS),
        "and its files must be checked again"
    );
}

#[tokio::test]
async fn a_quick_resubscribe_finds_its_files_in_place_and_rewrites_nothing() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    let same = hosts(&["a.example", "203.0.113.7"]);
    sink.replace_blocklist_rules(ADS, same.clone())
        .await
        .unwrap();
    let (domains, ips) = (inode(&h, ListKind::Domains), inode(&h, ListKind::Ips));
    sink.release_blocklist_rules(ADS).await.unwrap();
    let before = h.seen().len();

    sink.replace_blocklist_rules(ADS, same).await.unwrap();

    assert_eq!(
        inode(&h, ListKind::Domains),
        domains,
        "domains.list rewritten"
    );
    assert_eq!(inode(&h, ListKind::Ips), ips, "ips.list rewritten");
    let installs = h.seen().split_off(before);
    assert_eq!(installs.len(), 2, "the two rules are back");
    assert!(
        installs.iter().all(|s| s.path_existed),
        "the list files were there when each rule arrived"
    );
}

#[tokio::test]
async fn the_orphan_purge_leaves_a_released_directory_until_its_grace_is_over() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink().with_release_grace(Duration::from_secs(3600));
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    sink.release_blocklist_rules(ADS).await.unwrap();
    sink.remove_orphans(&[]).await;
    assert!(h.dir.list_dir(&IdComponent::from_id(ADS)).exists());

    let h2 = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h2.sink().with_release_grace(Duration::ZERO);
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    sink.release_blocklist_rules(ADS).await.unwrap();
    sink.remove_orphans(&[]).await;
    assert!(
        !h2.dir.list_dir(&IdComponent::from_id(ADS)).exists(),
        "after its grace the purge removes it"
    );
}

#[tokio::test]
async fn a_directory_nobody_released_in_this_run_is_purged_at_once() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    h.sink()
        .replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    let h = h.restart().connect(Daemon::Accept, Vec::new());
    let sink = h.sink().with_release_grace(Duration::from_secs(3600));
    sink.remove_orphans(&[]).await;
    assert!(!h.dir.list_dir(&IdComponent::from_id(ADS)).exists());
}

#[tokio::test]
async fn a_resubscribed_list_is_no_longer_released() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink().with_release_grace(Duration::ZERO);
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    sink.release_blocklist_rules(ADS).await.unwrap();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    sink.remove_orphans(&[ADS.to_string()]).await;
    assert!(h
        .dir
        .has_list(&IdComponent::from_id(ADS), ListKind::Domains));
}

/// A rule the daemon couldn't be told to delete must not go on blocking a list
/// the user dropped: without its files it reads nothing.
#[tokio::test]
async fn when_the_delete_cant_reach_the_daemon_the_files_go_at_once() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    h.set_daemon(Daemon::Silent);
    let err = sink.release_blocklist_rules(ADS).await.unwrap_err();
    assert!(err.daemon_unavailable);
    assert!(!h.dir.list_dir(&IdComponent::from_id(ADS)).exists());
}

#[tokio::test]
async fn removing_for_good_still_takes_the_files_with_the_rules() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    sink.remove_blocklist_rules(ADS).await.unwrap();
    assert!(!h.dir.list_dir(&IdComponent::from_id(ADS)).exists());
}
