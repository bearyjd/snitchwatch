use super::*;

// -- validate_rule_shape / FIX 4 (issue #14 security review) ---------

fn valid_operator() -> snitchwatch_proto::protocol::Operator {
    snitchwatch_proto::protocol::Operator {
        r#type: "simple".to_string(),
        operand: "dest.host".to_string(),
        data: "github.com".to_string(),
        sensitive: false,
        list: Vec::new(),
    }
}

fn valid_rule() -> Rule {
    Rule {
        created: 0,
        name: "snitchwatch-allow-github.com-443".to_string(),
        description: String::new(),
        enabled: true,
        precedence: false,
        nolog: false,
        action: "allow".to_string(),
        duration: "once".to_string(),
        operator: Some(valid_operator()),
    }
}

#[test]
fn validate_rule_shape_accepts_a_well_formed_rule() {
    assert!(validate_rule_shape(&valid_rule()).is_ok());
}

#[test]
fn validate_rule_shape_rejects_none_operator() {
    let mut rule = valid_rule();
    rule.operator = None;
    assert!(validate_rule_shape(&rule).is_err());
}

#[test]
fn validate_rule_shape_rejects_unknown_operator_type() {
    let mut rule = valid_rule();
    rule.operator.as_mut().unwrap().r#type = "bogus".to_string();
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("unknown operator type")));
}

#[test]
fn validate_rule_shape_rejects_uncompilable_regexp_data() {
    let mut rule = valid_rule();
    let op = rule.operator.as_mut().unwrap();
    op.r#type = "regexp".to_string();
    op.data = "(unclosed".to_string();
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("does not compile")));
}

#[test]
fn validate_rule_shape_accepts_a_compilable_regexp() {
    let mut rule = valid_rule();
    let op = rule.operator.as_mut().unwrap();
    op.r#type = "regexp".to_string();
    op.data = r"^(?:[^.]+\.)*example\.com$".to_string();
    assert!(validate_rule_shape(&rule).is_ok());
}

// Issue #44: `ThisHost` / `AnyHostOnDomain` verdicts nest the host
// operator inside a `list`, so the canary must reach list members the
// way loader.go:413-420 compiles each of them.

fn list_member(r#type: &str, operand: &str, data: &str) -> snitchwatch_proto::protocol::Operator {
    snitchwatch_proto::protocol::Operator {
        r#type: r#type.to_string(),
        operand: operand.to_string(),
        data: data.to_string(),
        ..Default::default()
    }
}

fn list_rule(members: Vec<snitchwatch_proto::protocol::Operator>) -> Rule {
    let mut rule = valid_rule();
    rule.operator = Some(snitchwatch_proto::protocol::Operator {
        r#type: "list".to_string(),
        operand: "list".to_string(),
        list: members,
        ..Default::default()
    });
    rule
}

#[test]
fn validate_rule_shape_accepts_a_process_bound_host_list() {
    let rule = list_rule(vec![
        list_member("simple", "process.path", "/usr/bin/curl"),
        list_member("regexp", "dest.host", r"^(?:[^.]+\.)*example\.com$"),
    ]);
    assert!(validate_rule_shape(&rule).is_ok());
}

#[test]
fn validate_rule_shape_rejects_an_uncompilable_regexp_inside_a_list() {
    let rule = list_rule(vec![
        list_member("simple", "process.path", "/usr/bin/curl"),
        list_member("regexp", "dest.host", "(unclosed"),
    ]);
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("does not compile")));
}

#[test]
fn validate_rule_shape_rejects_an_unknown_operand_inside_a_list() {
    let rule = list_rule(vec![
        list_member("simple", "process.path", "/usr/bin/curl"),
        list_member("simple", "dest.hostname", "example.com"),
    ]);
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("unknown operator operand")));
}

#[test]
fn validate_rule_shape_rejects_an_empty_list() {
    // listMatch (operator.go:327-333) starts from `true`, so a list with
    // no members would match every connection.
    let err = validate_rule_shape(&list_rule(Vec::new())).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("no members")));
}

#[test]
fn validate_rule_shape_rejects_a_list_operand_with_no_members_whatever_its_type() {
    // `Match` dispatches on the *operand* (operator.go:340), so a
    // memberless `operand: "list"` matches everything even when its
    // `type` isn't "list".
    let mut rule = valid_rule();
    let op = rule.operator.as_mut().unwrap();
    op.r#type = "simple".to_string();
    op.operand = "list".to_string();
    op.list = Vec::new();
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("no members")));
}

#[test]
fn validate_rule_shape_accepts_a_list_with_an_empty_operand() {
    // A GUI toggle sends the rule back through the bridge's
    // `operator_from_wire`, which leaves a list's operand empty; the daemon
    // accepts that because `Compile` overwrites a list's operand with
    // `OpList` (operator.go:154-155). The mock must not be stricter here.
    let mut rule = list_rule(vec![
        list_member("simple", "process.path", "/usr/bin/curl"),
        list_member("simple", "dest.host", "example.com"),
    ]);
    rule.operator.as_mut().unwrap().operand = String::new();
    assert!(validate_rule_shape(&rule).is_ok());
}

// Shapes the real daemon accepts and then panics on (operator.go's
// callbacks type-assert what `Match` hands them) or reads as an empty,
// match-everything list (`Deserialize` copies members one level deep).

fn leaf_rule(r#type: &str, operand: &str, data: &str) -> Rule {
    let mut rule = valid_rule();
    rule.operator = Some(list_member(r#type, operand, data));
    rule
}

#[test]
fn validate_rule_shape_rejects_a_network_type_with_a_string_operand() {
    let err = validate_rule_shape(&leaf_rule("network", "dest.ip", "10.0.0.0/8")).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("panic")));
}

#[test]
fn validate_rule_shape_rejects_an_ip_operand_without_the_network_type() {
    for operand in ["dest.network", "source.network", "lists.nets"] {
        let rule = leaf_rule("simple", operand, "10.0.0.0/8");
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(
            matches!(&err, MockError::InvalidRule(msg) if msg.contains("panic")),
            "{operand}: {err:?}"
        );
    }
    assert!(validate_rule_shape(&leaf_rule("network", "dest.network", "10.0.0.0/8")).is_ok());
    assert!(validate_rule_shape(&leaf_rule("lists", "lists.nets", "/tmp/x")).is_ok());
}

#[test]
fn validate_rule_shape_rejects_a_nested_list() {
    let inner = list_rule(vec![list_member("simple", "dest.host", "example.com")])
        .operator
        .unwrap();
    let rule = list_rule(vec![
        list_member("simple", "process.path", "/usr/bin/curl"),
        inner,
    ]);
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("nested")));
}

#[test]
fn validate_rule_shape_rejects_members_on_a_non_list_type() {
    // Even with operand `list`: Deserialize keeps members only for the
    // `list` type, so this reaches the daemon as an empty list.
    for operand in ["list", "dest.host"] {
        let mut rule = list_rule(vec![list_member("simple", "dest.host", "example.com")]);
        let op = rule.operator.as_mut().unwrap();
        op.r#type = "simple".to_string();
        op.operand = operand.to_string();
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(
            matches!(&err, MockError::InvalidRule(msg) if msg.contains("dropped")),
            "{operand}: {err:?}"
        );
    }
}

#[test]
fn validate_rule_shape_rejects_unnamed_unparseable_duration() {
    let mut rule = valid_rule();
    rule.duration = "not-a-duration".to_string();
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("duration")));
}

#[test]
fn validate_rule_shape_accepts_go_duration_strings() {
    for duration in ["5m", "30s", "1h30m", "0", "until restart", "always", "once"] {
        let mut rule = valid_rule();
        rule.duration = duration.to_string();
        assert!(
            validate_rule_shape(&rule).is_ok(),
            "expected `{duration}` to be accepted"
        );
    }
}

#[test]
fn validate_rule_shape_rejects_empty_rule_name() {
    let mut rule = valid_rule();
    rule.name = String::new();
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("empty")));
}

#[test]
fn validate_rule_shape_rejects_rule_name_with_path_separator() {
    // This is the check that would have caught issue #14 FIX 1 (path
    // traversal via an unsanitized rule name) at test time.
    let mut rule = valid_rule();
    rule.name = "../../../../etc/cron.d/x".to_string();
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("path separator")));
}

// -- is_known_operand / LOW (issue #14 security review) ---------------

#[test]
fn validate_rule_shape_rejects_unknown_operand() {
    let mut rule = valid_rule();
    rule.operator.as_mut().unwrap().operand = "dest.hostt".to_string(); // typo
    let err = validate_rule_shape(&rule).unwrap_err();
    assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("unknown operator operand")));
}

#[test]
fn validate_rule_shape_accepts_process_env_prefixed_operand() {
    let mut rule = valid_rule();
    rule.operator.as_mut().unwrap().operand = "process.env.PATH".to_string();
    assert!(validate_rule_shape(&rule).is_ok());
}
