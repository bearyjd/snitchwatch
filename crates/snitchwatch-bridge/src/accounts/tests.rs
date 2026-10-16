//! Tests for [`super`]. None reads the host's accounts: lookups are fakes.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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

#[test]
fn lookups_only_show_clean_names() {
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
    let found = look_up_blocking(&lookup, vec![958, 7, 5], Duration::from_secs(60));
    assert_eq!(
        found,
        vec![(958, Some("snitchwatch".into())), (7, None), (5, None)]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// #115: a batch stops starting lookups past its time budget (the first
/// always runs); the rest are left for a later snapshot.
#[test]
fn a_batch_stops_starting_lookups_past_its_budget() {
    let lookup: AccountLookup = Arc::new(|_| Some("x".into()));
    let found = look_up_blocking(&lookup, vec![1, 2, 3], Duration::ZERO);
    assert_eq!(found, vec![(1, Some("x".into()))]);
}

#[test]
fn known_accounts_are_named_per_rule() {
    let now = Instant::now();
    let mut known = KnownAccounts::default();
    known.learn(vec![(958, Some("snitchwatch".into())), (5, None)], now);
    let rule = with(leaf("user.name", "958"));
    assert_eq!(
        known.names_for(&rule),
        BTreeMap::from([("958".to_string(), "snitchwatch".to_string())])
    );
    assert!(known.names_for(&with(leaf("user.name", "5"))).is_empty());
}

/// #115: one batch at a time. Uids wanted meanwhile wait for the running
/// batch to drain them, and nothing is wanted twice.
#[test]
fn one_batch_runs_at_a_time_and_later_uids_queue_for_it() {
    let now = Instant::now();
    let mut known = KnownAccounts::default();
    assert_eq!(known.want(BTreeSet::from([6, 8]), now), Some(vec![6, 8]));
    assert_eq!(
        known.want(BTreeSet::from([6, 8, 9]), now),
        None,
        "in flight"
    );
    assert_eq!(known.next_batch(), Some(vec![9]), "queued for the batch");
    known.learn(vec![(6, Some("a".into())), (8, None), (9, None)], now);
    assert_eq!(known.next_batch(), None);
    assert_eq!(
        known.want(BTreeSet::from([6, 8, 9, 10]), now),
        Some(vec![10]),
        "the batch ended: a new one starts with what isn't known"
    );
}

#[test]
fn a_batch_takes_at_most_the_per_snapshot_limit() {
    let now = Instant::now();
    let mut known = KnownAccounts::default();
    let many: BTreeSet<u32> = (0..2 * MAX_LOOKUPS_PER_SNAPSHOT as u32).collect();
    assert_eq!(
        known.want(many.clone(), now).unwrap().len(),
        MAX_LOOKUPS_PER_SNAPSHOT
    );
    known.want(many, now);
    assert_eq!(
        known.next_batch().unwrap().len(),
        MAX_LOOKUPS_PER_SNAPSHOT,
        "the queue is bounded too"
    );
}

/// #115: a lookup that found nothing (no such account, or NSS failed) is
/// tried again after a cool-down, not remembered as nameless for good.
#[test]
fn a_failed_lookup_is_retried_after_a_cool_down() {
    let then = Instant::now();
    let mut known = KnownAccounts::default();
    known.want(BTreeSet::from([5]), then);
    known.learn(vec![(5, None)], then);
    assert_eq!(known.next_batch(), None);
    assert_eq!(known.want(BTreeSet::from([5]), then), None, "cooling down");
    assert_eq!(
        known.want(BTreeSet::from([5]), then + MISSING_RETRY),
        Some(vec![5])
    );
}

/// #115: uids a batch ran out of time for are no longer claimed, so a later
/// snapshot wants them again.
#[test]
fn uids_a_batch_ran_out_of_time_for_are_wanted_again() {
    let now = Instant::now();
    let mut known = KnownAccounts::default();
    let batch = known.want(BTreeSet::from([1, 2, 3]), now).unwrap();
    known.learn(vec![(1, Some("a".into()))], now);
    known.release(&batch);
    assert_eq!(known.next_batch(), None);
    assert_eq!(known.want(BTreeSet::from([1, 2, 3]), now), Some(vec![2, 3]));
}

/// #115: a full map makes room for one entry; it never forgets everything,
/// and never what a running batch is looking up.
#[test]
fn a_full_map_evicts_one_entry_and_keeps_claims() {
    let now = Instant::now();
    let mut known = KnownAccounts::default();
    let full: Vec<_> = (0..MAX_KNOWN_ACCOUNTS as u32)
        .map(|uid| (uid, Some(format!("u{uid}"))))
        .collect();
    known.learn(full, now);
    assert_eq!(
        known.want(BTreeSet::from([50_000, 50_001]), now),
        Some(vec![50_000, 50_001])
    );
    known.learn(vec![(50_000, Some("new".into()))], now);
    assert_eq!(known.names.len(), MAX_KNOWN_ACCOUNTS);
    assert_eq!(
        known
            .names
            .values()
            .filter(|k| matches!(k, Known::Named(_)))
            .count(),
        MAX_KNOWN_ACCOUNTS
    );
    assert_eq!(
        known.want(BTreeSet::from([50_001]), now),
        None,
        "50001 is still claimed by the running batch"
    );
}

/// #115: a batch that ended without finishing (its lookup panicked) frees
/// the slot and its claims, so the next snapshot looks them up.
#[test]
fn an_abandoned_batch_frees_the_slot_and_its_claims() {
    let now = Instant::now();
    let mut known = KnownAccounts::default();
    known.want(BTreeSet::from([6]), now);
    let serial = known.batch_serial();
    known.want(BTreeSet::from([7]), now);
    known.abandon(serial);
    assert_eq!(known.want(BTreeSet::from([6, 7]), now), Some(vec![6, 7]));
    known.abandon(serial);
    assert_eq!(
        known.want(BTreeSet::from([8]), now),
        None,
        "an older batch's guard leaves a newer one alone"
    );
}
