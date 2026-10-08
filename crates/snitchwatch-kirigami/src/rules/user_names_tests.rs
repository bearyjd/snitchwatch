//! The Rules list's `user.name` conditions with the account names the
//! bridge gives (PR #106 review M4).

use super::row_store::Rule;

/// The bridge names a `user.name` uid (PR #106 review M4); without a
/// name, or for another operand, the value is shown as it is.
#[test]
fn a_user_name_uid_is_shown_with_the_name_the_bridge_gives() {
    let r: Rule = serde_json::from_value(serde_json::json!({
        "name": "r",
        "operator": { "type": "list", "operands": [
            { "type": "simple", "operand": "user.name", "data": "958" },
            { "type": "simple", "operand": "user.name", "data": "5" },
            { "type": "simple", "operand": "user.id", "data": "958" },
        ]},
        "userNames": { "958": "snitchwatch" },
    }))
    .unwrap();
    assert_eq!(
        r.operator_summary(),
        "user.name = snitchwatch (958) AND user.name = 5 AND user.id = 958"
    );
    let wire = serde_json::to_value(&r).unwrap();
    assert!(wire.get("userNames").is_none(), "never sent back");
}
