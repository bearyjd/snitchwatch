//! `docs/schemas/snitchwatch-rules-v1.schema.json` against the code (P2.7
//! review #16): the published schema's enums, caps and reserved prefixes
//! are the validator's. The Rust validator stays authoritative (it also
//! checks pairing, regexps, CIDRs, ports and match-all shapes).

use super::*;
use crate::rule_io::{FORMAT, VERSION};
use crate::rule_name::{
    BLOCKLIST_RULE_NAME_PREFIX, CURATED_DEFAULT_RULE_NAME_PREFIX,
    LEGACY_BLOCKLIST_RULE_NAME_PREFIX, MAX_RULE_NAME_BYTES, PACKAGED_RULE_NAME_PREFIX,
};
use serde_json::Value;

const SCHEMA: &str = include_str!("../../../../docs/schemas/snitchwatch-rules-v1.schema.json");

fn schema() -> Value {
    serde_json::from_str(SCHEMA).expect("the schema is JSON")
}

fn strings(value: &Value) -> Vec<String> {
    let mut out: Vec<String> = value
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| v.as_str().expect("a string").to_string())
        .collect();
    out.sort();
    out
}

#[test]
fn the_envelope_matches_the_code() {
    let schema = schema();
    let props = &schema["properties"];
    assert_eq!(props["format"]["const"], FORMAT);
    assert_eq!(props["version"]["const"], VERSION);
    assert_eq!(
        props["rules"]["maxItems"],
        crate::cache::rules::MAX_SNAPSHOT_RULES
    );
}

#[test]
fn a_rule_matches_the_import_profile() {
    let schema = schema();
    let rule = &schema["$defs"]["rule"];
    assert_eq!(
        strings(&rule["required"]),
        ["action", "duration", "enabled", "name", "operator"]
    );
    let props = &rule["properties"];
    assert_eq!(
        strings(&props["action"]["enum"]),
        ["allow", "deny", "reject"]
    );
    assert_eq!(
        strings(&props["duration"]["enum"]),
        ["always", "until restart"]
    );
    assert_eq!(props["name"]["maxLength"], MAX_RULE_NAME_BYTES);
    assert_eq!(
        props["description"]["maxLength"],
        crate::cache::rules::MAX_RULE_FIELD_BYTES
    );
    let reserved = props["name"]["not"]["pattern"].as_str().unwrap();
    for prefix in [
        BLOCKLIST_RULE_NAME_PREFIX,
        LEGACY_BLOCKLIST_RULE_NAME_PREFIX,
        CURATED_DEFAULT_RULE_NAME_PREFIX,
        PACKAGED_RULE_NAME_PREFIX,
    ] {
        assert!(
            reserved.contains(prefix),
            "{prefix} missing from {reserved}"
        );
    }
    let list = &props["operator"]["oneOf"][1]["properties"]["operands"];
    assert_eq!(list["maxItems"], crate::cache::rules::MAX_OPERATOR_LIST_LEN);
}

#[test]
fn the_operand_vocabulary_is_the_validators_minus_hashes() {
    let schema = schema();
    let leaf = &schema["$defs"]["leaf"]["properties"];
    let listed = strings(&leaf["operand"]["anyOf"][0]["enum"]);
    let mut code: Vec<String> = std::iter::once("true")
        .chain(NETWORK_OPERANDS.iter().copied())
        .chain(
            STRING_OPERANDS
                .iter()
                .copied()
                .filter(|o| !HASH_OPERANDS.contains(o)),
        )
        .map(str::to_string)
        .collect();
    code.sort();
    assert_eq!(listed, code);
    assert_eq!(
        leaf["data"]["maxLength"],
        crate::cache::rules::MAX_RULE_FIELD_BYTES
    );
    assert_eq!(
        strings(&leaf["type"]["enum"]),
        ["network", "regexp", "simple"]
    );
}
