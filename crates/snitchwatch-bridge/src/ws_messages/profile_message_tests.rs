use super::*;

fn summary(id: &str, active: bool) -> ProfileSummary {
    ProfileSummary {
        id: id.into(),
        name: "At Home".into(),
        network_matchers: vec!["Home*".into()],
        rules: vec![ProfileRuleWire {
            id: "r1".into(),
            action: "allow".into(),
            operand: "dest.host".into(),
            data: "nas.local".into(),
        }],
        active,
    }
}

#[test]
fn set_profiles_serializes_to_camel_case_action() {
    let msg = ServerMessage::SetProfiles {
        profiles: vec![summary("home", true)],
        storage: Some(StorageStatus {
            unreadable: false,
            persistent: false,
            reason: Some("profile store: disk full".into()),
        }),
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["action"], "setProfiles");
    assert_eq!(json["profiles"][0]["id"], "home");
    assert_eq!(json["profiles"][0]["networkMatchers"][0], "Home*");
    assert_eq!(json["profiles"][0]["rules"][0]["operand"], "dest.host");
    assert_eq!(json["profiles"][0]["active"], true);
    assert_eq!(json["storage"]["persistent"], false);
    assert_eq!(json["storage"]["reason"], "profile store: disk full");
}

/// Issue #46: an older bridge sends no `storage`; GUIs treat that as not
/// persistent.
#[test]
fn set_profiles_from_an_older_bridge_still_parses() {
    let json = r#"{"action":"setProfiles","profiles":[{"id":"a","name":"A",
        "networkMatchers":[],"rules":[],"active":false}]}"#;
    match serde_json::from_str::<ServerMessage>(json).unwrap() {
        ServerMessage::SetProfiles { profiles, storage } => {
            assert_eq!(storage, None);
            assert_eq!(profiles[0].id, "a");
        }
        other => panic!("expected SetProfiles, got {other:?}"),
    }
}

#[test]
fn set_profiles_round_trips() {
    let msg = ServerMessage::SetProfiles {
        profiles: vec![summary("home", false)],
        storage: Some(StorageStatus {
            unreadable: false,
            persistent: true,
            reason: None,
        }),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, msg);
}

#[test]
fn profile_changed_round_trips_with_and_without_active_id() {
    let with_id = ServerMessage::ProfileChanged {
        active_profile_id: Some("home".into()),
    };
    let json = serde_json::to_string(&with_id).unwrap();
    assert!(json.contains(r#""action":"profileChanged""#));
    assert_eq!(
        serde_json::from_str::<ServerMessage>(&json).unwrap(),
        with_id
    );

    let none = ServerMessage::ProfileChanged {
        active_profile_id: None,
    };
    let json = serde_json::to_string(&none).unwrap();
    assert_eq!(serde_json::from_str::<ServerMessage>(&json).unwrap(), none);
}

#[test]
fn create_profile_round_trips() {
    let action = ClientMessage::CreateProfile {
        id: "home".into(),
        name: "At Home".into(),
        network_matchers: vec!["Home*".into()],
    };
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains(r#""action":"createProfile""#));
    assert!(json.contains(r#""networkMatchers""#));
    assert_eq!(
        serde_json::from_str::<ClientMessage>(&json).unwrap(),
        action
    );
}

#[test]
fn activate_and_deactivate_profile_round_trip() {
    let activate = ClientMessage::ActivateProfile { id: "home".into() };
    let json = serde_json::to_string(&activate).unwrap();
    assert_eq!(
        serde_json::from_str::<ClientMessage>(&json).unwrap(),
        activate
    );

    let deactivate = ClientMessage::DeactivateProfile;
    let json = serde_json::to_string(&deactivate).unwrap();
    assert_eq!(json, r#"{"action":"deactivateProfile"}"#);
    assert_eq!(
        serde_json::from_str::<ClientMessage>(&json).unwrap(),
        deactivate
    );
}

#[test]
fn add_and_remove_profile_rule_round_trip() {
    let add = ClientMessage::AddProfileRule {
        profile_id: "home".into(),
        rule: ProfileRuleWire {
            id: "r1".into(),
            action: "deny".into(),
            operand: "dest.host".into(),
            data: "ads.example".into(),
        },
    };
    let json = serde_json::to_string(&add).unwrap();
    assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), add);

    let remove = ClientMessage::RemoveProfileRule {
        profile_id: "home".into(),
        rule_id: "r1".into(),
    };
    let json = serde_json::to_string(&remove).unwrap();
    assert_eq!(
        serde_json::from_str::<ClientMessage>(&json).unwrap(),
        remove
    );
}
