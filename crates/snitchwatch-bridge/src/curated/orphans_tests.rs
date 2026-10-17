use super::*;
use crate::curated::entries;

const GONE: &str = "snitchwatch-default-retired-service";

fn rule(name: &str, enabled: bool) -> Rule {
    Rule {
        name: name.into(),
        enabled,
        action: "allow".into(),
        duration: "always".into(),
        ..Default::default()
    }
}

fn list(rules: &[Rule]) -> BTreeMap<String, Rule> {
    rules.iter().map(|r| (r.name.clone(), r.clone())).collect()
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

#[test]
fn a_prefixed_rule_with_no_entry_and_no_saved_copy_is_an_orphan() {
    let rules = list(&[rule(GONE, true)]);
    assert!(is_orphan(
        GONE,
        entries(),
        &rules,
        &none(),
        &Choices::default()
    ));
    assert_eq!(
        orphans(entries(), &rules, &none(), &Choices::default()),
        vec![("retired-service".to_string(), true)]
    );
}

#[test]
fn a_disabled_orphan_is_listed_as_disabled() {
    let rules = list(&[rule(GONE, false)]);
    assert_eq!(
        orphans(entries(), &rules, &none(), &Choices::default()),
        vec![("retired-service".to_string(), false)]
    );
}

#[test]
fn a_current_entry_is_not_an_orphan() {
    let entry = &entries()[0];
    let name = entry.rule_name();
    let rules = list(&[rule(&name, true)]);
    assert!(!is_orphan(
        &name,
        entries(),
        &rules,
        &none(),
        &Choices::default()
    ));
    assert!(orphans(entries(), &rules, &none(), &Choices::default()).is_empty());
}

#[test]
fn a_rule_with_a_saved_copy_is_not_an_orphan() {
    let present = rule(GONE, true);
    let rules = list(&[present.clone()]);
    let recorded = Choices::default().installed("retired-service", &present);
    assert!(recorded.installed.contains_key("retired-service"));
    assert!(!is_orphan(GONE, entries(), &rules, &none(), &recorded));
    assert!(orphans(entries(), &rules, &none(), &recorded).is_empty());
}

#[test]
fn a_rule_recorded_only_by_id_is_not_an_orphan() {
    let rules = list(&[rule(GONE, true)]);
    let mut recorded = Choices::default();
    recorded.installed_ids.insert("retired-service".into());
    assert!(!is_orphan(GONE, entries(), &rules, &none(), &recorded));
}

#[test]
fn an_unlisted_or_left_out_name_is_not_an_orphan() {
    let rules = list(&[rule(GONE, true)]);
    assert!(!is_orphan(
        "snitchwatch-default-elsewhere",
        entries(),
        &rules,
        &none(),
        &Choices::default()
    ));
    let left_out: BTreeSet<String> = [GONE.to_string()].into();
    assert!(!is_orphan(
        GONE,
        entries(),
        &rules,
        &left_out,
        &Choices::default()
    ));
}

#[test]
fn a_name_outside_the_prefix_or_with_an_invalid_id_is_not_an_orphan() {
    for name in [
        "user-rule",
        "snitchwatch-default-",
        "snitchwatch-default-Bad_Id",
    ] {
        let rules = list(&[rule(name, true)]);
        assert!(
            !is_orphan(name, entries(), &rules, &none(), &Choices::default()),
            "{name}"
        );
        assert!(orphans(entries(), &rules, &none(), &Choices::default()).is_empty());
    }
}
