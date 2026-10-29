//! S1: for every persisted column, a value one byte over its cap never
//! reaches the file at more than the cap (the API cuts it; the schema's own
//! CHECK refuses it if the API is bypassed).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use proptest::prelude::*;
use rusqlite::Connection;

use super::caps::{self, *};
use super::record::*;
use super::test_support::{state_dir, tempdir};
use super::*;

/// A store in a fresh enabled history; the tempdir is returned to keep it.
fn store() -> (tempfile::TempDir, Store) {
    let root = tempdir();
    let state = state_dir(&root);
    let at = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &at,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let store = open_store(&at).unwrap();
    (root, store)
}

fn largest(conn: &Connection, column: &str) -> i64 {
    conn.query_row(
        &format!("SELECT coalesce(max(length(CAST({column} AS BLOB))), 0) FROM events"),
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// Text that is `len` bytes of multi-byte characters plus filler, so a cut
/// can land inside a character.
fn text_of(len: usize, wide: bool) -> String {
    let unit = if wide { "\u{20ac}" } else { "a" };
    unit.repeat(len / unit.len() + 1)
}

const TEXT_COLUMNS: [(&str, usize); 5] = [
    ("rule_name", RULE_NAME),
    ("process_path", PROCESS_PATH),
    ("dst_host", DST_HOST),
    ("protocol", PROTOCOL),
    ("rule_class", RULE_CLASS),
];

fn record_with(column: &str, value: &str) -> EventRecord {
    let mut e = EventRecord::new(1, Action::Allow, AnsweredBy::Rule);
    match column {
        "rule_name" => e.rule_name = Some(RuleName::new(value)),
        "process_path" => e.process_path = Some(ProcessPath::new(value)),
        "dst_host" => e.dst_host = Some(DstHost::new(value)),
        "protocol" => e.protocol = Some(Protocol::new(value)),
        "rule_class" => e.rule_class = Some(RuleClass::new(value)),
        other => panic!("{other}"),
    }
    e
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn no_text_column_exceeds_its_cap_on_disk(
        which in 0usize..5,
        extra in 0usize..40,
        short_by in 0usize..8,
        wide in any::<bool>(),
    ) {
        let (column, cap) = TEXT_COLUMNS[which];
        let (_keep, store) = store();
        // Around the cap on both sides, including exactly cap+1.
        let len = (cap + extra + 1).saturating_sub(short_by);
        insert_event(store.connection(), &record_with(column, &text_of(len, wide))).unwrap();
        prop_assert!(largest(store.connection(), column) as usize <= cap);
    }
}

#[test]
fn a_value_at_cap_plus_one_is_cut_for_every_text_column() {
    for (column, cap) in TEXT_COLUMNS {
        let (_keep, store) = store();
        for wide in [false, true] {
            let value = text_of(cap + 1, wide);
            assert!(value.len() > cap);
            insert_event(store.connection(), &record_with(column, &value)).unwrap();
        }
        assert!(
            largest(store.connection(), column) as usize <= cap,
            "{column}"
        );
    }
}

#[test]
fn a_value_at_the_cap_is_stored_whole() {
    let (_keep, store) = store();
    let value = "p".repeat(PROCESS_PATH);
    insert_event(store.connection(), &record_with("process_path", &value)).unwrap();
    let stored: String = store
        .connection()
        .query_row("SELECT process_path FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, value);
}

#[test]
fn the_schema_itself_refuses_a_value_over_the_cap() {
    // Raw SQL, bypassing the insert API: the CHECK constraints are the
    // second guard.
    for (column, cap) in TEXT_COLUMNS {
        let (_keep, store) = store();
        let sql = format!(
            "INSERT INTO events (ts_ms, action, answered_by, {column}) VALUES (1, 'allow', 'rule', ?1)"
        );
        assert!(
            store.connection().execute(&sql, ["x".repeat(cap)]).is_ok(),
            "{column} at cap"
        );
        assert!(
            store
                .connection()
                .execute(&sql, ["x".repeat(cap + 1)])
                .is_err(),
            "{column}"
        );
        // A NUL does not hide bytes from the check.
        let nul = format!("{}\u{0}{}", "x".repeat(cap), "y");
        assert!(
            store.connection().execute(&sql, [nul]).is_err(),
            "{column} with NUL"
        );
    }
}

#[test]
fn the_schema_refuses_over_long_ips_bad_hashes_snapshots_and_enums() {
    let (_keep, store) = store();
    let c = store.connection();
    let base = |col: &str| {
        format!("INSERT INTO events (ts_ms, action, answered_by, {col}) VALUES (1, 'allow', 'rule', ?1)")
    };
    let long_ip = "1".repeat(caps::IP_TEXT + 1);
    assert!(c.execute(&base("dst_ip"), [&long_ip]).is_err());
    assert!(c.execute(&base("src_ip"), [&long_ip]).is_err());
    assert!(c
        .execute(&base("dst_ip"), ["1".repeat(caps::IP_TEXT)])
        .is_ok());
    for len in [RULE_HASH - 1, RULE_HASH + 1] {
        assert!(
            c.execute(&base("rule_hash"), [vec![0u8; len]]).is_err(),
            "{len}"
        );
    }
    assert!(c
        .execute(&base("rule_hash"), [vec![0u8; RULE_HASH]])
        .is_ok());
    assert!(c.execute(&base("dst_port"), [65536]).is_err());
    assert!(c.execute(&base("src_port"), [-1]).is_err());
    assert!(c.execute(&base("repeat_count"), [0]).is_err());
    assert!(c
        .execute(
            "INSERT INTO events (ts_ms, action, answered_by) VALUES (1, 'bogus', 'rule')",
            []
        )
        .is_err());
    assert!(c
        .execute(
            "INSERT INTO events (ts_ms, action, answered_by) VALUES (1, 'allow', 'bogus')",
            []
        )
        .is_err());
    let snap = "INSERT INTO rules_seen (rule_hash, snapshot, first_seen_ms) VALUES (?1, ?2, 1)";
    let hash = vec![1u8; RULE_HASH];
    assert!(c
        .execute(snap, rusqlite::params![hash, "s".repeat(SNAPSHOT)])
        .is_ok());
    let hash2 = vec![2u8; RULE_HASH];
    assert!(c
        .execute(snap, rusqlite::params![hash2, "s".repeat(SNAPSHOT + 1)])
        .is_err());
}

#[test]
fn ip_addresses_store_at_most_their_longest_text() {
    let (_keep, store) = store();
    let mut e = EventRecord::new(1, Action::Deny, AnsweredBy::Rule);
    e.dst_ip = Some(IpAddr::V6(Ipv6Addr::new(
        0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
    )));
    e.src_ip = Some(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)));
    insert_event(store.connection(), &e).unwrap();
    assert!(largest(store.connection(), "dst_ip") as usize <= IP_TEXT);
    assert_eq!(
        largest(store.connection(), "dst_ip") as usize,
        IP_TEXT,
        "the longest form"
    );
    // The other long forms are shorter than the cap.
    let mapped: Ipv6Addr = "::ffff:255.255.255.255".parse().unwrap();
    assert!(mapped.to_string().len() < IP_TEXT);
    assert!(Ipv4Addr::new(255, 255, 255, 255).to_string().len() < IP_TEXT);
}

#[test]
fn snapshots_round_trip_and_the_hash_is_unique() {
    let (_keep, store) = store();
    let s = SnapshotRecord {
        rule_hash: [7; RULE_HASH],
        snapshot: Snapshot::new("{}").unwrap(),
        first_seen_ms: 5,
    };
    insert_snapshot(store.connection(), &s).unwrap();
    assert!(
        insert_snapshot(store.connection(), &s).is_err(),
        "one snapshot per hash"
    );
}

#[test]
fn the_cap_table_matches_the_owner_set_values() {
    assert_eq!((RULE_NAME, PROCESS_PATH, DST_HOST), (256, 4096, 253));
}

#[test]
fn column_types_are_enforced_so_no_value_hides_behind_affinity() {
    // Found in review: with plain affinity, a TEXT "hash" with a NUL after 16
    // characters passed `length() = 16` while storing a million bytes.
    let (_keep, store) = store();
    let c = store.connection();
    let mut sneaky = b"0123456789abcdef\0".to_vec();
    sneaky.resize(1_000_017, b'x');
    let sneaky = String::from_utf8(sneaky).unwrap();
    let ev = "INSERT INTO events (ts_ms, action, answered_by, rule_hash) VALUES (1, 'allow', 'rule', ?1)";
    assert!(c.execute(ev, [&sneaky]).is_err(), "text in a BLOB column");
    let snap = "INSERT INTO rules_seen (rule_hash, snapshot, first_seen_ms) VALUES (?1, '{}', 1)";
    assert!(
        c.execute(snap, [&sneaky]).is_err(),
        "text in rules_seen.rule_hash"
    );
    assert!(
        c.execute(snap, [vec![0u8; RULE_HASH]]).is_ok(),
        "a real 16-byte blob"
    );
    let other = "INSERT INTO events (ts_ms, action, answered_by, process_path) VALUES (1, 'allow', 'rule', ?1)";
    assert!(
        c.execute(other, [vec![b'a'; 10]]).is_err(),
        "a blob in a TEXT column"
    );
    for column in ["decided_ts_ms", "dst_port", "src_port", "repeat_count"] {
        let sql = format!(
            "INSERT INTO events (ts_ms, action, answered_by, {column}) VALUES (1, 'allow', 'rule', ?1)"
        );
        assert!(
            c.execute(&sql, ["x".repeat(1000)]).is_err(),
            "text in {column}"
        );
    }
    assert!(c
        .execute(
            "INSERT INTO events (ts_ms, action, answered_by) VALUES (?1, 'allow', 'rule')",
            ["x".repeat(1000)]
        )
        .is_err());
    let ts = "INSERT INTO rules_seen (rule_hash, snapshot, first_seen_ms) VALUES (?1, '{}', ?2)";
    assert!(c
        .execute(
            ts,
            rusqlite::params![vec![1u8; RULE_HASH], "x".repeat(1000)]
        )
        .is_err());
}
