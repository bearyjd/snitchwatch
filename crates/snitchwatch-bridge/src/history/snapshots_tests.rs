//! PR 4b: the redacted snapshot built from a `RuleIdentity`, its D7 caps
//! (oldest dropped even when referenced), same-transaction orphan cleanup,
//! and crash safety.
//! The writer path is exercised for real: canaries are searched for in the
//! database file's bytes. User mode only, as the other writer tests: the
//! system mode differs in where the directory lives, not in what is stored.

use rusqlite::Connection;
use snitchwatch_proto::protocol::{Operator, Rule};

use super::budget::Budget;
use super::record::{insert_snapshot, Action, AnsweredBy, EventRecord, SnapshotRecord};
use super::rule_identity::identify;
use super::snapshots::*;
use super::test_support::{state_dir, tempdir};
use super::writer::{Engine, Item, TxnKind, WriteError};
use super::*;

fn op(kind: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: kind.into(),
        operand: operand.into(),
        data: data.into(),
        sensitive: false,
        list: vec![],
    }
}

fn group(members: Vec<Operator>) -> Operator {
    Operator {
        list: members,
        ..op("list", "list", "[]")
    }
}

fn rule(name: &str, operator: Option<Operator>) -> Rule {
    Rule {
        name: name.into(),
        action: "allow".into(),
        duration: "always".into(),
        enabled: true,
        operator,
        ..Rule::default()
    }
}

fn snap(r: &Rule) -> SnapshotRecord {
    snapshot_for(&identify(r).expect("identity"), 5).expect("snapshot")
}

fn host_rule(name: &str) -> Rule {
    rule(name, Some(op("simple", "dest.host", "example.org")))
}

struct Rig {
    _root: tempfile::TempDir,
    location: Location,
    engine: Engine,
}

fn rig() -> Rig {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let store = open_store(&location).unwrap();
    let engine = Engine::new(store, &location, Budget::new(16 * 1024 * 1024).unwrap()).unwrap();
    engine
        .connection()
        .pragma_update(None, "synchronous", "OFF")
        .unwrap();
    Rig {
        _root: root,
        location,
        engine,
    }
}

impl Rig {
    fn count(&self, table: &str) -> i64 {
        self.engine
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn file_has(&self, needle: &str) -> bool {
        let mut all = std::fs::read(self.location.db_path()).unwrap();
        for ext in ["db-journal", "db-wal"] {
            if let Ok(more) = std::fs::read(self.location.db_path().with_extension(ext)) {
                all.extend(more);
            }
        }
        all.windows(needle.len()).any(|w| w == needle.as_bytes())
    }
}

fn item(ts: i64, snapshot: Option<SnapshotRecord>, hash: [u8; 16]) -> Item {
    let mut event = EventRecord::new(ts, Action::Allow, AnsweredBy::Rule);
    event.rule_hash = Some(hash);
    Item { event, snapshot }
}

fn with_snapshot(ts: i64, r: &Rule) -> Item {
    item_of(ts, snap(r))
}

fn item_of(ts: i64, s: SnapshotRecord) -> Item {
    let hash = s.rule_hash();
    item(ts, Some(s), hash)
}

fn fail_once() -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let mut armed = true;
    Box::new(move || {
        std::mem::take(&mut armed).then(|| WriteError::Io(std::io::Error::other("injected")))
    })
}

fn withheld_rule(v: &str) -> Rule {
    let nested = group(vec![
        op("simple", "process.command", v),
        op("regexp", "process.env.HOME", v),
        op("simple", "x-unknown-kind", v),
        op("simple", "user.name", v),
        group(vec![op("simple", "process.hash.md5", v)]),
    ]);
    let mut top = group(vec![op("simple", "dest.host", "example.org"), nested]);
    top.data = format!("[{{\"operand\":\"process.command\",\"data\":\"{v}\"}}]");
    Rule {
        description: v.into(),
        ..rule("r", Some(top))
    }
}

// ---- redaction reaches the DB bytes ---------------------------------

#[test]
fn canaries_in_withheld_slots_never_reach_the_database_bytes() {
    let mut rig = rig();
    let canary = "CANARY-9e41c7-withheld";
    let it = with_snapshot(1, &withheld_rule(canary));
    rig.engine.insert_batch(&[it]).unwrap();
    assert_eq!(rig.count("rules_seen"), 1);
    assert!(rig.file_has("example.org"), "the kept value is stored");
    assert!(!rig.file_has("CANARY"), "no withheld value is stored");
    // The text is the stored snapshot, and carries no canary either.
    let text: String = rig
        .engine
        .connection()
        .query_row("SELECT snapshot FROM rules_seen", [], |r| r.get(0))
        .unwrap();
    assert!(!text.contains("CANARY"));
}

#[test]
fn redaction_equivalence_varying_only_withheld_values() {
    let a = snap(&withheld_rule("one"));
    let b = snap(&withheld_rule("a-very-different-and-longer-secret"));
    assert_eq!(a.rule_hash(), b.rule_hash());
    assert_eq!(a.snapshot().as_str(), b.snapshot().as_str());
}

#[test]
fn changing_only_sensitive_changes_hash_and_snapshot() {
    let mk = |s: bool| {
        snap(&rule(
            "r",
            Some(Operator {
                sensitive: s,
                ..op("simple", "dest.host", "example.org")
            }),
        ))
    };
    let (a, b) = (mk(false), mk(true));
    assert_ne!(a.rule_hash(), b.rule_hash());
    assert_ne!(a.snapshot().as_str(), b.snapshot().as_str());
}

#[test]
fn list_backed_rules_carry_the_qualifier_flag() {
    let flagged = |r: &Rule| {
        let v: serde_json::Value = serde_json::from_str(snap(r).snapshot().as_str()).unwrap();
        v["list_contents_not_recorded"].as_bool().unwrap()
    };
    assert!(flagged(&rule(
        "r",
        Some(op("lists", "lists.domains", "/d"))
    )));
    assert!(!flagged(&host_rule("r")));
    // Nested inside a group too.
    let nested = group(vec![group(vec![op("lists", "lists.ips", "/d")])]);
    assert!(flagged(&rule("r", Some(nested))));
}

#[test]
fn the_snapshot_is_json_with_withheld_marked_not_valued() {
    let v: serde_json::Value =
        serde_json::from_str(snap(&withheld_rule("CANARY")).snapshot().as_str()).unwrap();
    let text = v.to_string();
    assert!(text.contains("\"withheld\":true"));
    assert!(text.contains("process.command"), "the kind is kept");
    assert!(!text.contains("CANARY"));
}

#[test]
fn an_escaping_blowup_past_the_snapshot_cap_is_refused_not_cut() {
    // Within the canonical cap, but every quote escapes to two bytes.
    let r = rule("r", Some(op("simple", "dest.host", &"\"".repeat(7_000))));
    assert!(identify(&r).is_ok());
    assert_eq!(
        snapshot_for(&identify(&r).unwrap(), 1).unwrap_err(),
        SnapshotError::TooLarge
    );
}

#[test]
fn snapshot_for_refuses_an_identity_whose_hash_is_not_its_canonical_bytes() {
    let mut wrong_hash = identify(&host_rule("r")).unwrap();
    wrong_hash.hash[0] ^= 1;
    assert_eq!(
        snapshot_for(&wrong_hash, 1).unwrap_err(),
        SnapshotError::HashMismatch
    );
    // Same hash, different (still well-formed) bytes: the name's first byte.
    let mut wrong_bytes = identify(&host_rule("r")).unwrap();
    wrong_bytes.canonical[9] = b's';
    assert_eq!(
        snapshot_for(&wrong_bytes, 1).unwrap_err(),
        SnapshotError::HashMismatch
    );
}

// ---- caps: D7's bounds, eviction independent of events ---------------

fn mem() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&super::schema::schema_sql()).unwrap();
    conn
}

fn add_event(conn: &Connection, ts: i64, hash: Option<[u8; 16]>) {
    let mut e = EventRecord::new(ts, Action::Allow, AnsweredBy::Rule);
    e.rule_hash = hash;
    super::record::insert_event(conn, &e).unwrap();
}

fn rows(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn snap_at(r: &Rule, ms: i64) -> SnapshotRecord {
    snapshot_for(&identify(r).expect("identity"), ms).expect("snapshot")
}

/// Admits with a fresh usage and checks the incremental bookkeeping against
/// the table every time.
fn admit(conn: &Connection, limits: &SnapshotLimits, rec: &SnapshotRecord) -> SnapshotOutcome {
    let mut usage = SnapshotUsage::load(conn).unwrap();
    let out = admit_snapshot(conn, limits, &mut usage, rec).unwrap();
    assert_eq!(usage, SnapshotUsage::load(conn).unwrap(), "usage drifted");
    out
}

/// The stored hashes, oldest first.
fn stored(conn: &Connection) -> Vec<Vec<u8>> {
    conn.prepare("SELECT rule_hash FROM rules_seen ORDER BY first_seen_ms, id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn stored_bytes(conn: &Connection) -> u64 {
    conn.query_row(
        "SELECT coalesce(sum(length(CAST(snapshot AS BLOB))), 0) FROM rules_seen",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap() as u64
}

fn hash_of_rule(r: &Rule) -> Vec<u8> {
    snap(r).rule_hash().to_vec()
}

#[test]
fn the_limits_are_d7s_and_the_budget_can_only_lower_them() {
    const MIB: u64 = 1024 * 1024;
    let big = Budget::new(1024 * MIB).unwrap();
    let limits = SnapshotLimits::from_budget(&big);
    assert_eq!((limits.max_count, limits.max_total_bytes), (2000, 4 * MIB));
    let small = Budget::new(16 * MIB).unwrap();
    let limits = SnapshotLimits::from_budget(&small);
    assert_eq!(limits.max_count, 2000);
    assert_eq!(limits.max_total_bytes, small.main_bytes / 10);
    assert!(limits.max_total_bytes < 4 * MIB);
}

#[test]
fn over_the_count_cap_the_oldest_snapshot_goes_even_though_an_event_refers_to_it() {
    let conn = mem();
    let limits = SnapshotLimits::new(2, u64::MAX);
    let rules: Vec<Rule> = (0..3).map(|i| host_rule(&format!("r{i}"))).collect();
    for (i, r) in rules.iter().take(2).enumerate() {
        let s = snap_at(r, i as i64 + 1);
        assert_eq!(admit(&conn, &limits, &s), SnapshotOutcome::Stored);
        add_event(&conn, i as i64, Some(s.rule_hash()));
    }
    assert_eq!(
        admit(&conn, &limits, &snap_at(&rules[2], 3)),
        SnapshotOutcome::Stored,
        "the limit always has a victim"
    );
    assert_eq!(
        stored(&conn),
        vec![hash_of_rule(&rules[1]), hash_of_rule(&rules[2])]
    );
    assert_eq!(rows(&conn, "events"), 2, "no event was touched");
    let kept: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE rule_hash = ?1",
            [hash_of_rule(&rules[0])],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1, "the event keeps its rule_hash");
    // Re-admitting one already there is not an eviction.
    assert_eq!(
        admit(&conn, &limits, &snap_at(&rules[2], 9)),
        SnapshotOutcome::AlreadyPresent
    );
    assert_eq!(stored(&conn).len(), 2);
}

#[test]
fn over_the_byte_cap_the_oldest_snapshot_goes_even_though_an_event_refers_to_it() {
    let conn = mem();
    let one = snap(&host_rule("r0")).snapshot().as_str().len() as u64;
    let cap = one * 2 + one / 2;
    let limits = SnapshotLimits::new(100, cap);
    let rules: Vec<Rule> = (0..3).map(|i| host_rule(&format!("r{i}"))).collect();
    for (i, r) in rules.iter().take(2).enumerate() {
        let s = snap_at(r, i as i64 + 1);
        admit(&conn, &limits, &s);
        add_event(&conn, 1, Some(s.rule_hash()));
    }
    assert_eq!(
        admit(&conn, &limits, &snap_at(&rules[2], 3)),
        SnapshotOutcome::Stored
    );
    assert_eq!(
        stored(&conn),
        vec![hash_of_rule(&rules[1]), hash_of_rule(&rules[2])]
    );
    assert!(stored_bytes(&conn) <= cap);
    assert_eq!(rows(&conn, "events"), 2);
}

#[test]
fn a_large_snapshot_drops_as_many_of_the_oldest_as_it_needs() {
    let conn = mem();
    let one = snap(&host_rule("r0")).snapshot().as_str().len() as u64;
    let members = (0..4).map(|j| op("simple", "dest.host", &format!("h{j}.example.org")));
    let wide = snap_at(&rule("wide", Some(group(members.collect()))), 4);
    let wide_len = wide.snapshot().as_str().len() as u64;
    assert!(wide_len > one);
    // Room for the wide one plus a single small one: two small ones must go.
    let cap = wide_len + one;
    let limits = SnapshotLimits::new(100, cap);
    let rules: Vec<Rule> = (0..3).map(|i| host_rule(&format!("r{i}"))).collect();
    for (i, r) in rules.iter().enumerate() {
        let s = snap_at(r, i as i64 + 1);
        admit(&conn, &limits, &s);
        add_event(&conn, 1, Some(s.rule_hash()));
    }
    assert_eq!(stored(&conn).len(), 3);
    assert_eq!(admit(&conn, &limits, &wide), SnapshotOutcome::Stored);
    assert_eq!(
        stored(&conn),
        vec![hash_of_rule(&rules[2]), wide.rule_hash().to_vec()]
    );
    assert!(stored_bytes(&conn) <= cap);
    assert_eq!(rows(&conn, "events"), 3);
}

#[test]
fn a_snapshot_over_the_total_by_itself_is_refused_and_evicts_nothing() {
    let conn = mem();
    let roomy = SnapshotLimits::new(100, u64::MAX);
    admit(&conn, &roomy, &snap_at(&host_rule("r0"), 1));
    let one = snap(&host_rule("r1")).snapshot().as_str().len() as u64;
    let tight = SnapshotLimits::new(100, one - 1);
    assert_eq!(
        admit(&conn, &tight, &snap_at(&host_rule("r1"), 2)),
        SnapshotOutcome::Refused
    );
    let none = SnapshotLimits::new(0, u64::MAX);
    assert_eq!(
        admit(&conn, &none, &snap_at(&host_rule("r1"), 2)),
        SnapshotOutcome::Refused
    );
    assert_eq!(stored(&conn), vec![hash_of_rule(&host_rule("r0"))]);
}

#[test]
fn one_usage_carried_across_many_admissions_never_drifts_from_the_table() {
    let conn = mem();
    let one = snap(&host_rule("r00")).snapshot().as_str().len() as u64;
    let limits = SnapshotLimits::new(3, one * 5);
    let mut usage = SnapshotUsage::load(&conn).unwrap();
    for i in 0..12 {
        let r = if i % 4 == 3 {
            let m = (0..2).map(|j| op("simple", "dest.host", &format!("h{i}-{j}.example.org")));
            rule(&format!("w{i}"), Some(group(m.collect())))
        } else {
            host_rule(&format!("r{i:02}"))
        };
        admit_snapshot(&conn, &limits, &mut usage, &snap_at(&r, i)).unwrap();
        assert_eq!(usage, SnapshotUsage::load(&conn).unwrap(), "step {i}");
        assert!(rows(&conn, "rules_seen") <= 3);
        assert!(stored_bytes(&conn) <= one * 5);
    }
}

#[test]
fn the_writer_keeps_every_event_and_the_newest_snapshots_at_the_count_cap() {
    let mut rig = rig();
    let limits = SnapshotLimits::from_budget(rig.engine.budget());
    assert_eq!(limits.max_count, 2000);
    let n = limits.max_count as usize + 100;
    let rules: Vec<Rule> = (0..n).map(|i| host_rule(&format!("r{i}"))).collect();
    let items: Vec<Item> = rules
        .iter()
        .enumerate()
        .map(|(i, r)| item_of(i as i64, snap_at(r, i as i64)))
        .collect();
    for chunk in items.chunks(rig.engine.budget().rows_per_txn as usize) {
        rig.engine.insert_batch(chunk).unwrap();
    }
    assert_eq!(rig.count("events") as usize, n, "no event was refused");
    assert_eq!(rig.count("rules_seen") as u64, limits.max_count);
    let present = |r: &Rule| -> bool {
        rig.engine
            .connection()
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM rules_seen WHERE rule_hash = ?1)",
                [hash_of_rule(r)],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(!present(&rules[0]), "the oldest was dropped");
    assert!(present(&rules[n - 1]), "the newest was kept");
}

#[test]
fn the_writer_stays_under_the_byte_cap_and_keeps_every_event() {
    let mut rig = rig();
    let limits = SnapshotLimits::from_budget(rig.engine.budget());
    // About 3 KiB each: the byte cap is reached long before the count cap.
    let big = |i: usize| {
        let members = (0..60).map(|j| op("simple", "dest.host", &format!("h{i}-{j}.example.org")));
        rule(&format!("r{i}"), Some(group(members.collect())))
    };
    let n = 400;
    let items: Vec<Item> = (0..n)
        .map(|i| item_of(i as i64, snap_at(&big(i), i as i64)))
        .collect();
    for chunk in items.chunks(rig.engine.budget().rows_per_txn as usize) {
        rig.engine.insert_batch(chunk).unwrap();
    }
    assert_eq!(rig.count("events"), n as i64);
    let snaps = rig.count("rules_seen");
    assert!(snaps > 0 && snaps < n as i64, "{snaps}");
    assert!(stored_bytes(rig.engine.connection()) <= limits.max_total_bytes);
    let newest = hash_of_rule(&big(n - 1));
    assert!(stored(rig.engine.connection()).contains(&newest));
}

// ---- same-transaction orphan cleanup --------------------------------

#[test]
fn deleting_an_event_removes_its_unreferenced_snapshot_and_keeps_shared_ones() {
    let conn = mem();
    let (a, b) = (snap(&host_rule("a")), snap(&host_rule("b")));
    for s in [&a, &b] {
        insert_snapshot(&conn, s).unwrap();
    }
    add_event(&conn, 1, Some(a.rule_hash()));
    add_event(&conn, 2, Some(b.rule_hash()));
    add_event(&conn, 3, Some(b.rule_hash()));
    add_event(&conn, 4, None);
    assert_eq!(delete_oldest_events(&conn, 2).unwrap(), 2);
    let hashes: Vec<Vec<u8>> = conn
        .prepare("SELECT rule_hash FROM rules_seen")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(hashes, vec![b.rule_hash().to_vec()], "a went, b is shared");
    assert_eq!(rows(&conn, "events"), 2);
    assert_eq!(delete_oldest_events(&conn, 10).unwrap(), 2);
    assert_eq!(rows(&conn, "rules_seen"), 0);
}

#[test]
fn eviction_through_the_writer_cleans_snapshots_in_one_transaction() {
    let mut rig = rig();
    let it = with_snapshot(1, &withheld_rule("CANARY-evict"));
    rig.engine.insert_batch(&[it]).unwrap();
    rig.engine.evict_chunk().unwrap();
    assert_eq!(rig.count("events"), 0);
    assert_eq!(rig.count("rules_seen"), 0);
    assert!(!rig.file_has("example.org"), "zeroed on delete");
}

// ---- crash safety ----------------------------------------------------

#[test]
fn a_fault_before_commit_leaves_neither_event_nor_snapshot() {
    let mut rig = rig();
    let it = with_snapshot(1, &host_rule("r"));
    rig.engine.fault_before_commit_for_test(fail_once());
    assert!(rig.engine.insert_batch(std::slice::from_ref(&it)).is_err());
    assert_eq!(rig.count("events"), 0);
    assert_eq!(rig.count("rules_seen"), 0);
    assert!(!rig.file_has("example.org"));
    rig.engine.insert_batch(&[it]).unwrap();
    assert_eq!(rig.count("events"), 1);
    assert_eq!(rig.count("rules_seen"), 1);
}

#[test]
fn a_fault_before_commit_leaves_no_partial_cleanup() {
    let mut rig = rig();
    let shared = with_snapshot(1, &host_rule("shared"));
    let hash = shared.event.rule_hash.unwrap();
    let only = with_snapshot(2, &host_rule("only"));
    rig.engine
        .insert_batch(&[shared, only, item(3, None, hash)])
        .unwrap();
    let (events, snaps) = (rig.count("events"), rig.count("rules_seen"));
    assert_eq!((events, snaps), (3, 2));
    rig.engine.fault_before_commit_for_test(fail_once());
    assert!(rig.engine.evict_chunk().is_err());
    assert_eq!(rig.count("events"), events, "no event deleted");
    assert_eq!(rig.count("rules_seen"), snaps, "no snapshot deleted");
    rig.engine.evict_chunk().unwrap();
    assert_eq!((rig.count("events"), rig.count("rules_seen")), (0, 0));
}

#[test]
fn a_fault_after_cap_eviction_restores_the_evicted_snapshot() {
    let mut rig = rig();
    let limits = SnapshotLimits::from_budget(rig.engine.budget());
    // The oldest snapshot is referenced: D7 still drops it at the cap.
    let old = snap(&host_rule("old"));
    insert_snapshot(rig.engine.connection(), &old).unwrap();
    add_event(rig.engine.connection(), 1, Some(old.rule_hash()));
    let tight = SnapshotLimits::new(1, limits.max_total_bytes);
    let new = snap(&host_rule("new"));
    rig.engine.fault_before_commit_for_test(fail_once());
    let result = rig.engine.transact(TxnKind::Insert, |conn| {
        let mut usage = SnapshotUsage::load(conn)?;
        admit_snapshot(conn, &tight, &mut usage, &new).map_err(WriteError::from)?;
        Ok(1)
    });
    assert!(result.is_err());
    assert_eq!(rig.count("rules_seen"), 1);
    assert_eq!(rig.count("events"), 1);
    let kept: Vec<u8> = rig
        .engine
        .connection()
        .query_row("SELECT rule_hash FROM rules_seen", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        kept,
        old.rule_hash().to_vec(),
        "the eviction was rolled back"
    );
}

// ---- the durable (pinned) commit path --------------------------------

fn pinned(rig: &super::durable::tests::Rig, items: Vec<Item>) -> super::durable::PinnedBatch {
    let pins = items
        .iter()
        .map(|_| rig.registry.accept().unwrap())
        .collect();
    super::durable::PinnedBatch { items, pins }
}

fn count_of(rig: &super::durable::tests::Rig, table: &str) -> i64 {
    rig.engine
        .connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_pinned_commit_obeys_the_snapshot_caps_and_keeps_every_event() {
    let mut rig = super::durable::tests::rig(1000);
    let limits = SnapshotLimits::from_budget(rig.engine.budget());
    // ~6 KiB each: the byte cap is reached long before the count cap.
    let fat = |i: usize| {
        rule(
            &format!("r{i}"),
            Some(op(
                "simple",
                "dest.host",
                &format!("{i:05}{}", "h".repeat(6_000)),
            )),
        )
    };
    let mut events = 0;
    for chunk in 0..8 {
        let items: Vec<Item> = (0..32)
            .map(|j| {
                let i = chunk * 32 + j;
                item_of(i as i64, snap_at(&fat(i), i as i64))
            })
            .collect();
        events += items.len() as i64;
        let batch = pinned(&rig, items);
        rig.engine.commit_pinned(&batch, &rig.registry).unwrap();
    }
    assert_eq!(count_of(&rig, "events"), events, "no event was refused");
    let used = stored_bytes(rig.engine.connection());
    assert!(used <= limits.max_total_bytes, "{used}");
    assert!(count_of(&rig, "rules_seen") < events, "some were dropped");
    assert!(
        stored(rig.engine.connection()).contains(&hash_of_rule(&fat(255))),
        "the newest was kept"
    );
}

#[test]
fn a_pinned_commit_that_faults_leaves_neither_event_nor_snapshot() {
    let mut rig = super::durable::tests::rig(1000);
    let batch = pinned(&rig, vec![with_snapshot(1, &host_rule("r"))]);
    rig.engine.fault_before_commit_for_test(fail_once());
    assert!(rig.engine.commit_pinned(&batch, &rig.registry).is_err());
    assert_eq!(count_of(&rig, "events"), 0);
    assert_eq!(count_of(&rig, "rules_seen"), 0);
    assert_eq!(rig.registry.len(), 1, "the pin was not released");
}

// ---- an item's snapshot must be its event's --------------------------

fn mismatched() -> Vec<(&'static str, Item)> {
    let other = snap(&host_rule("other"));
    let wrong_hash = item(2, Some(snap(&host_rule("a"))), other.rule_hash());
    let mut no_hash = with_snapshot(3, &host_rule("b"));
    no_hash.event.rule_hash = None;
    vec![("another rule's hash", wrong_hash), ("no hash", no_hash)]
}

#[test]
fn insert_batch_refuses_a_snapshot_that_is_not_its_events_before_writing() {
    let mut rig = rig();
    for (what, bad) in mismatched() {
        let batch = [with_snapshot(1, &host_rule("ok")), bad];
        let err = rig.engine.insert_batch(&batch).unwrap_err();
        assert!(matches!(err, WriteError::SnapshotMismatch), "{what}: {err}");
        assert_eq!(rig.count("events"), 0, "{what}");
        assert_eq!(rig.count("rules_seen"), 0, "{what}");
    }
}

#[test]
fn a_pinned_commit_refuses_a_snapshot_that_is_not_its_events_before_writing() {
    let mut rig = super::durable::tests::rig(1000);
    for (what, bad) in mismatched() {
        let batch = pinned(&rig, vec![with_snapshot(1, &host_rule("ok")), bad]);
        let held = rig.registry.len();
        let err = rig.engine.commit_pinned(&batch, &rig.registry).unwrap_err();
        assert!(
            matches!(
                err,
                super::durable::DurableError::Write(WriteError::SnapshotMismatch)
            ),
            "{what}: {err}"
        );
        assert_eq!(count_of(&rig, "events"), 0, "{what}");
        assert_eq!(count_of(&rig, "rules_seen"), 0, "{what}");
        assert_eq!(rig.registry.len(), held, "{what}: no pin was released");
    }
}
