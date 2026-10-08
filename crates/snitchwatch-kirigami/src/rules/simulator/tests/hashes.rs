//! Hash-operand tests for `simulate`: opensnitchd's checksum quirks and the
//! checksum setting being unknown. Builders live in the parent module.

use super::*;

// ---- hash operands ---------------------------------------------------------

fn hash_rule() -> Value {
    simple("process.hash.md5", "deadbeef")
}

fn with_checksums(enabled: Option<bool>, sums: Option<&[(&str, &str)]>) -> SimulationInput {
    base_with(|i| {
        i.checksums_enabled = enabled;
        i.checksums = sums.map(|s| {
            s.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        });
    })
}

#[test]
fn hash_matches_every_program_while_checksums_are_off() {
    let result = run(
        hash_rule(),
        &with_checksums(Some(false), Some(&[("md5", "cafe")])),
    );
    assert_eq!(result.matched_rule.as_deref(), Some("r"));
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("match every program while checksums are off")),
        "{:?}",
        result.warnings
    );
}

#[test]
fn hash_with_the_checksum_setting_unknown_is_decided_only_when_on_and_off_agree() {
    // Off always matches; so the rule is decided only if the checksums-on
    // answer is a match too.
    let hit = run(
        hash_rule(),
        &with_checksums(None, Some(&[("md5", "deadbeef")])),
    );
    assert_eq!(hit.matched_rule.as_deref(), Some("r"));
    assert!(hit.unevaluated.is_empty());

    let none_recorded = run(hash_rule(), &with_checksums(None, Some(&[])));
    assert_eq!(none_recorded.matched_rule.as_deref(), Some("r"));
    assert!(
        none_recorded
            .warnings
            .iter()
            .any(|w| w.contains("no recorded checksum")),
        "{:?}",
        none_recorded.warnings
    );

    // On: a mismatch. Off: a match. Not decidable.
    let miss = run(
        hash_rule(),
        &with_checksums(None, Some(&[("md5", "cafebabe")])),
    );
    assert_eq!(miss.matched_rule, None);
    assert_eq!(miss.unevaluated.len(), 1);
    assert_eq!(miss.unevaluated[0].operand, "process.hash.md5");
    assert!(miss.unevaluated[0].missing.contains("checksums are on"));
}

#[test]
fn hash_with_nothing_known_about_checksums_is_not_a_default_match() {
    // The form's default: checksum setting and checksum both unknown.
    let result = run(hash_rule(), &with_checksums(None, None));
    assert_eq!(result.matched_rule, None);
    // Both what is missing: whether checksums are on, and the checksum.
    assert_eq!(result.unevaluated.len(), 2, "{:?}", result.unevaluated);
    assert!(result.unevaluated[0].missing.contains("checksums are on"));
    assert!(result.unevaluated[1].missing.contains("MD5"));
}

#[test]
fn a_hash_deny_does_not_decide_while_the_checksum_setting_is_unknown() {
    let rules = vec![
        deny("100-deny-hash", hash_rule()),
        allow("200-allow", simple("dest.host", "example.com")),
    ];
    let result = simulate(&store_with(rules), &with_checksums(None, None));
    assert_eq!(result.matched_rule.as_deref(), Some("200-allow"));
    // The checksum setting and the checksum itself, both for the deny.
    assert_eq!(result.unevaluated.len(), 2, "{:?}", result.unevaluated);
    assert!(result.unevaluated.iter().all(|u| u.rule == "100-deny-hash"));
}

#[test]
fn hash_with_checksums_on_compares_the_programs_checksum() {
    let hit = run(
        hash_rule(),
        &with_checksums(Some(true), Some(&[("md5", "deadbeef")])),
    );
    assert_eq!(hit.matched_rule.as_deref(), Some("r"));
    assert!(hit.warnings.is_empty(), "{:?}", hit.warnings);

    let miss = run(
        hash_rule(),
        &with_checksums(Some(true), Some(&[("md5", "cafebabe")])),
    );
    assert_eq!(miss.matched_rule, None);
    assert!(miss.unevaluated.is_empty());
}

#[test]
fn hash_with_checksums_on_but_none_recorded_still_matches_with_a_warning() {
    // `ret` starts true and is only overwritten while iterating the process's
    // checksums, so a process with none matches every hash rule.
    let result = run(hash_rule(), &with_checksums(Some(true), Some(&[])));
    assert_eq!(result.matched_rule.as_deref(), Some("r"));
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("no recorded checksum")),
        "{:?}",
        result.warnings
    );
}

#[test]
fn hash_with_checksums_on_and_the_programs_checksum_unknown_is_unevaluated() {
    let result = run(hash_rule(), &with_checksums(Some(true), None));
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].rule, "r");
    assert_eq!(result.unevaluated[0].operand, "process.hash.md5");
}

#[test]
fn hash_compare_is_exact_and_tries_every_recorded_algorithm() {
    // `hashCmp` is a plain `==`: no case folding, whatever `sensitive` says.
    let upper = simple("process.hash.md5", "DEADBEEF");
    assert!(!matched(
        upper,
        &with_checksums(Some(true), Some(&[("md5", "deadbeef")]))
    ));
    // `Match` iterates every checksum the process has, whatever the operand's
    // algorithm: an md5 rule matches an equal sha1 value.
    assert!(matched(
        simple("process.hash.sha1", "deadbeef"),
        &with_checksums(Some(true), Some(&[("md5", "deadbeef")]))
    ));
    // An empty recorded checksum is a fake match ("avoid displaying a pop-up").
    assert!(matched(
        hash_rule(),
        &with_checksums(Some(true), Some(&[("md5", "")]))
    ));
    // A regexp-typed hash rule goes through `reCmp`.
    let sums = with_checksums(Some(true), Some(&[("md5", "deadbeef")]));
    assert!(matched(op("regexp", "process.hash.md5", "^dead"), &sums));
    assert!(!matched(op("regexp", "process.hash.md5", "^beef"), &sums));
}

#[test]
fn a_hash_warning_is_dropped_when_the_rule_cannot_match_anyway() {
    let operator = list_op(vec![hash_rule(), simple("process.path", "/usr/bin/wget")]);
    let result = run(operator, &with_checksums(Some(false), None));
    assert_eq!(result.matched_rule, None);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

#[test]
fn the_hash_warning_comes_from_the_deciding_rule_only() {
    // An earlier hash allow that is overwritten by a later allow is not the
    // reason for the verdict, so its warning is not shown.
    let rules = vec![
        allow("100-hash", hash_rule()),
        allow("200-host", simple("dest.host", "example.com")),
    ];
    let result = simulate(&store_with(rules), &with_checksums(Some(false), None));
    assert_eq!(result.matched_rule.as_deref(), Some("200-host"));
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

fn sha1_rule() -> Value {
    simple("process.hash.sha1", "deadbeef")
}

#[test]
fn a_sha1_condition_with_only_an_md5_known_is_not_evaluated() {
    // Match compares every checksum the process has; the operand's own
    // algorithm is not among those given, so a mismatch proves nothing.
    let result = run(
        sha1_rule(),
        &with_checksums(Some(true), Some(&[("md5", "cafebabe")])),
    );
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1, "{:?}", result.unevaluated);
    assert_eq!(result.unevaluated[0].operand, "process.hash.sha1");
    assert!(result.unevaluated[0].missing.contains("SHA1"));
}

#[test]
fn a_sha1_condition_is_decided_once_the_sha1_is_known() {
    let miss = with_checksums(Some(true), Some(&[("md5", "cafebabe"), ("sha1", "0123")]));
    let result = run(sha1_rule(), &miss);
    assert_eq!(result.matched_rule, None);
    assert!(result.unevaluated.is_empty());

    let hit = with_checksums(Some(true), Some(&[("sha1", "deadbeef")]));
    assert_eq!(run(sha1_rule(), &hit).matched_rule.as_deref(), Some("r"));
}

#[test]
fn an_md5_condition_with_only_a_sha1_known_is_not_evaluated() {
    let result = run(
        hash_rule(),
        &with_checksums(Some(true), Some(&[("sha1", "cafebabe")])),
    );
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
    assert!(result.unevaluated[0].missing.contains("MD5"));
}
