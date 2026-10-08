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
