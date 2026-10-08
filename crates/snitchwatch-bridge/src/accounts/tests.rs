//! Tests for [`super`]. None reads the host's accounts: lookups are fakes.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn leaf(operand: &str, data: &str) -> Operator {
    Operator {
        r#type: "simple".into(),
        operand: operand.into(),
        data: data.into(),
        ..Default::default()
    }
}

fn with(op: Operator) -> Rule {
    Rule {
        name: "r".into(),
        operator: Some(op),
        ..Default::default()
    }
}

#[test]
fn only_canonical_decimal_names_a_uid() {
    assert_eq!(canonical_uid("958"), Some(958));
    assert_eq!(canonical_uid("0"), Some(0));
    for not in ["0958", "+958", " 958", "958 ", "jd", "", "4294967296", "-1"] {
        assert_eq!(canonical_uid(not), None, "{not:?}");
    }
}

#[test]
fn uids_come_from_user_name_conditions_in_lists_too() {
    let list = Operator {
        r#type: "list".into(),
        operand: "list".into(),
        list: vec![
            leaf("user.name", "958"),
            leaf("user.id", "1000"),
            leaf("user.name", "jd"),
            leaf("user.name", "0"),
        ],
        ..Default::default()
    };
    assert_eq!(user_name_uids(&with(list)), BTreeSet::from([0, 958]));
}

#[test]
fn a_name_is_cleaned_and_capped() {
    assert_eq!(display_name("snitchwatch").as_deref(), Some("snitchwatch"));
    assert_eq!(display_name("ro\u{202e}ot\n").as_deref(), Some("root"));
    assert_eq!(display_name("\u{200b}"), None);
    let long = display_name(&"a".repeat(100)).unwrap();
    assert_eq!(long.chars().count(), MAX_ACCOUNT_NAME_CHARS);
}

#[tokio::test]
async fn lookups_run_off_the_runtime_and_only_show_clean_names() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let lookup: AccountLookup = Arc::new(move |uid| {
        counted.fetch_add(1, Ordering::SeqCst);
        match uid {
            958 => Some("snitchwatch".into()),
            7 => Some("\u{202e}".into()),
            _ => None,
        }
    });
    let found = look_up(lookup, vec![958, 7, 5]).await;
    assert_eq!(
        found,
        vec![(958, Some("snitchwatch".into())), (7, None), (5, None)]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn known_accounts_are_remembered_bounded_and_named_per_rule() {
    let mut known = KnownAccounts::default();
    known.learn(vec![(958, Some("snitchwatch".into())), (5, None)]);
    assert_eq!(known.not_looked_up(BTreeSet::from([5, 958, 6])), vec![6]);
    let rule = with(leaf("user.name", "958"));
    assert_eq!(
        known.names_for(&rule),
        BTreeMap::from([("958".to_string(), "snitchwatch".to_string())])
    );
    assert!(known.names_for(&with(leaf("user.name", "5"))).is_empty());
    let many: BTreeSet<u32> = (10_000..10_000 + 2 * MAX_LOOKUPS_PER_SNAPSHOT as u32).collect();
    assert_eq!(known.not_looked_up(many).len(), MAX_LOOKUPS_PER_SNAPSHOT);
    known.learn(
        (0..MAX_KNOWN_ACCOUNTS as u32 + 10)
            .map(|uid| (uid + 20_000, None))
            .collect(),
    );
    assert!(known.names.len() <= MAX_KNOWN_ACCOUNTS);
}
