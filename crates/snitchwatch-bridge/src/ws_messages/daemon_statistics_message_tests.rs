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
