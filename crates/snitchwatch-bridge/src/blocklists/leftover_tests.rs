use std::time::Duration;

use super::LeftoverRules;
use crate::blocklists::daemon_sink::tests::{
    delete, kind_of, legacy_rule, user_rule, Daemon, Harness, ADS,
};
use crate::blocklists::materializer::ListKind;

fn leftover(h: &Harness) -> LeftoverRules {
    LeftoverRules::new(h.commands.clone(), h.rules.cache()).with_timeout(Duration::from_millis(300))
}

/// A foreign rule under a blocklist name: not the bridge's shape.
fn foreign() -> snitchwatch_proto::protocol::Rule {
    user_rule("z00-blocklist:foreign:domains")
}

#[tokio::test]
async fn it_lists_the_rules_snitchwatch_made_and_nothing_else() {
    let h = Harness::new();
    let snapshot = vec![
        h.bridge_rule(ADS, ListKind::Domains),
        h.bridge_rule(ADS, ListKind::Ips),
        legacy_rule("900-blocklist:old:0001-x.example"),
        foreign(),
        user_rule("899-firefox"),
    ];
    let h = h.connect(Daemon::Accept, snapshot);
    let names = leftover(&h).names().expect("the rule list is known");
    assert_eq!(
        names,
        vec![
            "900-blocklist:old:0001-x.example".to_string(),
            format!("z00-blocklist:{ADS}:domains"),
            format!("z00-blocklist:{ADS}:ips"),
        ]
    );
}

#[tokio::test]
async fn it_says_nothing_while_the_daemons_rule_list_is_unknown() {
    let h = Harness::new();
    assert_eq!(leftover(&h).names(), None);
}

#[tokio::test]
async fn removing_deletes_each_rule_and_only_those() {
    let h = Harness::new();
    let snapshot = vec![
        h.bridge_rule(ADS, ListKind::Domains),
        h.bridge_rule(ADS, ListKind::Ips),
        foreign(),
        user_rule("899-firefox"),
    ];
    let h = h.connect(Daemon::Accept, snapshot);
    let outcome = leftover(&h).remove_all().await.unwrap();
    assert_eq!((outcome.removed, outcome.refused), (2, 0));
    let sent: Vec<_> = h.seen().iter().map(|s| kind_of(&s.command)).collect();
    assert_eq!(
        sent,
        vec![
            delete(&format!("z00-blocklist:{ADS}:domains")),
            delete(&format!("z00-blocklist:{ADS}:ips")),
        ]
    );
    assert_eq!(
        leftover(&h).names(),
        Some(Vec::new()),
        "the OKs left the cache"
    );
    assert!(h
        .rules
        .cache()
        .lock()
        .unwrap()
        .rules()
        .is_some_and(|rules| rules.contains_key("z00-blocklist:foreign:domains")
            && rules.contains_key("899-firefox")));
}

#[tokio::test]
async fn a_refused_delete_is_counted_and_the_rest_are_still_tried() {
    let h = Harness::new();
    let snapshot = vec![
        h.bridge_rule(ADS, ListKind::Domains),
        h.bridge_rule(ADS, ListKind::Ips),
    ];
    let h = h.connect(Daemon::Refuse("no"), snapshot);
    let outcome = leftover(&h).remove_all().await.unwrap();
    assert_eq!((outcome.removed, outcome.refused), (0, 2));
    assert_eq!(leftover(&h).names().map(|n| n.len()), Some(2));
}

#[tokio::test]
async fn an_unanswered_delete_stops_the_pass_and_says_the_daemon_is_unavailable() {
    let h = Harness::new();
    let snapshot = vec![
        h.bridge_rule(ADS, ListKind::Domains),
        h.bridge_rule(ADS, ListKind::Ips),
    ];
    let h = h.connect(Daemon::Silent, snapshot);
    let err = leftover(&h).remove_all().await.unwrap_err();
    assert!(err.daemon_unavailable);
    assert_eq!(h.seen().len(), 1, "the second delete wasn't tried");
}

#[tokio::test]
async fn with_no_daemon_connected_nothing_is_sent() {
    let h = Harness::new();
    let err = leftover(&h).remove_all().await.unwrap_err();
    assert!(err.daemon_unavailable);
    assert!(h.seen().is_empty());
}

/// A name the bridge could not send a delete for is not counted, or the page
/// would offer to remove what removing can't.
#[tokio::test]
async fn a_rule_whose_delete_could_not_be_sent_is_not_counted() {
    let h = Harness::new();
    let mut unsendable = h.bridge_rule(ADS, ListKind::Domains);
    unsendable.name = "z00-blocklist:ads/../domains".to_string();
    let snapshot = vec![unsendable, h.bridge_rule(ADS, ListKind::Ips)];
    let h = h.connect(Daemon::Accept, snapshot);
    assert_eq!(
        leftover(&h).names(),
        Some(vec![format!("z00-blocklist:{ADS}:ips")])
    );
}

/// Over the legacy TCP connection nothing is listed and nothing deleted.
#[tokio::test]
async fn over_tcp_nothing_is_listed_or_deleted() {
    use crate::daemon_commands::{DaemonCommands, DaemonTransport};
    let h = Harness::new().connect(Daemon::Accept, vec![user_rule("z00-blocklist:ads:domains")]);
    let tcp = DaemonCommands::new(DaemonTransport::Tcp, h.rules.clone());
    let leftover = LeftoverRules::new(tcp, h.rules.cache());
    assert_eq!(leftover.names(), None);
    assert!(leftover.remove_all().await.is_err());
    assert!(h.seen().is_empty());
}
