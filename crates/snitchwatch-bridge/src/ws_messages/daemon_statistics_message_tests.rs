use super::*;

#[test]
fn daemon_statistics_round_trips_via_json() {
    let msg = ServerMessage::DaemonStatistics {
        daemon_version: "1.8.0".to_string(),
        uptime: 3661,
        rules: 12,
        connections: 4200,
        ignored: 10,
        accepted: 4000,
        dropped: 200,
        rule_hits: 3900,
        rule_misses: 300,
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["action"], "daemonStatistics");
    assert_eq!(json["daemonVersion"], "1.8.0");
    assert_eq!(json["uptime"], 3661);
    assert_eq!(json["rules"], 12);
    assert_eq!(json["connections"], 4200);
    assert_eq!(json["ignored"], 10);
    assert_eq!(json["accepted"], 4000);
    assert_eq!(json["dropped"], 200);
    assert_eq!(json["ruleHits"], 3900);
    assert_eq!(json["ruleMisses"], 300);

    let parsed: ServerMessage = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, msg);
}

#[test]
fn daemon_policy_round_trips_via_json() {
    let msg = ServerMessage::DaemonPolicy(DaemonPolicy::for_daemon(Some(
        crate::daemon_config::DaemonVersion::new(1, 9),
    )));
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "action": "daemonPolicy",
            "refuseSourceNetwork": true,
            "daemonVersion": "1.9",
        })
    );
    let parsed: ServerMessage = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, msg);
}

#[test]
fn daemon_policy_for_an_unknown_or_1_8_daemon_refuses_nothing() {
    let unknown = DaemonPolicy::for_daemon(None);
    assert!(!unknown.refuse_source_network);
    assert_eq!(unknown.daemon_version, None);
    let old = DaemonPolicy::for_daemon(Some(crate::daemon_config::DaemonVersion::new(1, 8)));
    assert!(!old.refuse_source_network);
    let decoded: ServerMessage =
        serde_json::from_str(r#"{"action":"daemonPolicy","refuseSourceNetwork":false}"#).unwrap();
    assert_eq!(decoded, ServerMessage::DaemonPolicy(unknown));
}
