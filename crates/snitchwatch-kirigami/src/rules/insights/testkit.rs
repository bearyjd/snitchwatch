//! Builders shared by the insights tests.

use serde_json::{json, Value};

use crate::rules::row_store::Rule;

pub fn simple(operand: &str, data: &str) -> Value {
    json!({"type": "simple", "operand": operand, "data": data, "sensitive": false})
}

pub fn simple_sensitive(operand: &str, data: &str) -> Value {
    json!({"type": "simple", "operand": operand, "data": data, "sensitive": true})
}

pub fn regexp(operand: &str, pattern: &str) -> Value {
    json!({"type": "regexp", "operand": operand, "data": pattern, "sensitive": false})
}

pub fn regexp_sensitive(operand: &str, pattern: &str) -> Value {
    json!({"type": "regexp", "operand": operand, "data": pattern, "sensitive": true})
}

pub fn network(operand: &str, cidr: &str) -> Value {
    json!({"type": "network", "operand": operand, "data": cidr, "sensitive": false})
}

pub fn truth() -> Value {
    json!({"type": "simple", "operand": "true", "data": "", "sensitive": false})
}

pub fn all_of(members: Vec<Value>) -> Value {
    json!({"type": "list", "operand": "list", "list": members})
}

/// An enabled, permanent rule named `name`.
pub fn rule(name: &str, action: &str, operator: Value) -> Rule {
    Rule {
        name: name.to_string(),
        enabled: true,
        action: action.to_string(),
        duration: "always".to_string(),
        operator,
        ..Default::default()
    }
}

pub fn allow(name: &str, operator: Value) -> Rule {
    rule(name, "allow", operator)
}

pub fn deny(name: &str, operator: Value) -> Rule {
    rule(name, "deny", operator)
}

pub fn host(data: &str) -> Value {
    simple("dest.host", data)
}
