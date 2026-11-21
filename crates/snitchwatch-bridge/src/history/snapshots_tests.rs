//! PR 4b: the redacted snapshot built from a `RuleIdentity`, its caps with
//! snapshot-only eviction, same-transaction orphan cleanup, and crash safety.
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
    let s = snap(r);
    let hash = s.rule_hash;
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
    assert_eq!(a.rule_hash, b.rule_hash);
    assert_eq!(a.snapshot.as_str(), b.snapshot.as_str());
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
    assert_ne!(a.rule_hash, b.rule_hash);
    assert_ne!(a.snapshot.as_str(), b.snapshot.as_str());
}

#[test]
fn list_backed_rules_carry_the_qualifier_flag() {
    let flagged = |r: &Rule| {
        let v: serde_json::Value = serde_json::from_str(snap(r).snapshot.as_str()).unwrap();
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
        serde_json::from_str(snap(&withheld_rule("CANARY")).snapshot.as_str()).unwrap();
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

// ---- caps and snapshot-only eviction --------------------------------

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

#[test]
fn the_count_cap_refuses_a_new_snapshot_when_every_one_is_referenced() {
    let conn = mem();
    let limits = SnapshotLimits::new(2, u64::MAX);
    let rules: Vec<Rule> = (0..3).map(|i| host_rule(&format!("r{i}"))).collect();
    for (i, r) in rules.iter().take(2).enumerate() {
        assert_eq!(
            admit_snapshot(&conn, &limits, &snap(r)).unwrap(),
            SnapshotOutcome::Stored
        );
        add_event(&conn, i as i64, Some(snap(r).rule_hash));
    }
    assert_eq!(
        admit_snapshot(&conn, &limits, &snap(&rules[2])).unwrap(),
        SnapshotOutcome::Refused
    );
    assert_eq!(rows(&conn, "rules_seen"), 2, "referenced snapshots stay");
    assert_eq!(rows(&conn, "events"), 2, "no event was evicted");
    // Re-admitting one already there is not a refusal.
    assert_eq!(
        admit_snapshot(&conn, &limits, &snap(&rules[0])).unwrap(),
        SnapshotOutcome::AlreadyPresent
    );
}

#[test]
fn the_total_bytes_cap_refuses_and_a_fitting_one_is_stored() {
    let conn = mem();
    let one = snap(&host_rule("r0")).snapshot.as_str().len() as u64;
    let limits = SnapshotLimits::new(100, one * 2 + one / 2);
    let rules: Vec<Rule> = (0..3).map(|i| host_rule(&format!("r{i}"))).collect();
    for r in &rules[..2] {
        admit_snapshot(&conn, &limits, &snap(r)).unwrap();
        add_event(&conn, 1, Some(snap(r).rule_hash));
    }
    assert_eq!(
        admit_snapshot(&conn, &limits, &snap(&rules[2])).unwrap(),
        SnapshotOutcome::Refused
    );
    assert_eq!(rows(&conn, "rules_seen"), 2);
}

#[test]
fn eviction_removes_only_unreferenced_snapshots_oldest_first_and_never_events() {
    let conn = mem();
    let limits = SnapshotLimits::new(3, u64::MAX);
    let rules: Vec<Rule> = (0..4).map(|i| host_rule(&format!("r{i}"))).collect();
    // r0 (oldest) is referenced; r1 and r2 are not.
    let mut r0 = snap(&rules[0]);
    r0.first_seen_ms = 1;
    insert_snapshot(&conn, &r0).unwrap();
    add_event(&conn, 1, Some(r0.rule_hash));
    for (i, r) in rules[1..3].iter().enumerate() {
        let mut s = snap(r);
        s.first_seen_ms = 10 + i as i64;
        insert_snapshot(&conn, &s).unwrap();
    }
    assert_eq!(
        admit_snapshot(&conn, &limits, &snap(&rules[3])).unwrap(),
        SnapshotOutcome::Stored
    );
    let left: Vec<Vec<u8>> = conn
        .prepare("SELECT rule_hash FROM rules_seen ORDER BY first_seen_ms, id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(left.len(), 3);
    assert!(left.contains(&r0.rule_hash.to_vec()), "referenced survives");
    assert!(
        !left.contains(&snap(&rules[1]).rule_hash.to_vec()),
        "the oldest unreferenced one went"
    );
    assert_eq!(rows(&conn, "events"), 1);
}

#[test]
fn a_batch_through_the_writer_keeps_events_when_snapshots_hit_the_cap() {
    let mut rig = rig();
    let limits = SnapshotLimits::from_budget(rig.engine.budget());
    // Wide rules: each snapshot is a few KiB.
    let big = |i: usize| {
        let members = (0..40).map(|j| op("simple", "dest.host", &format!("h{i}-{j}.example.org")));
        rule(&format!("r{i}"), Some(group(members.collect())))
    };
    let n = limits.max_count as usize + 30;
    let total_cap = limits.max_total_bytes;
    for i in 0..n {
        let it = with_snapshot(i as i64, &big(i));
        rig.engine.insert_batch(&[it]).unwrap();
        if rig.count("rules_seen") as u64 >= limits.max_count {
            break;
        }
        let used: i64 = rig
            .engine
            .connection()
            .query_row(
                "SELECT coalesce(sum(length(CAST(snapshot AS BLOB))), 0) FROM rules_seen",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(used as u64 <= total_cap);
    }
    let events = rig.count("events");
    let snaps = rig.count("rules_seen");
    assert!(events > snaps, "refused snapshots never cost an event");
    // One more past the cap: the event is stored, the snapshot is not.
    let before = rig.count("rules_seen");
    let sat = |rig: &mut Rig, i: usize| {
        let it = with_snapshot(10_000 + i as i64, &big(10_000 + i));
        rig.engine.insert_batch(&[it]).unwrap();
    };
    for i in 0..(limits.max_count as usize + 5) {
        sat(&mut rig, i);
    }
    assert!(rig.count("rules_seen") as u64 <= limits.max_count);
    assert!(rig.count("rules_seen") >= before);
    assert!(rig.count("events") > events);
}

// ---- same-transaction orphan cleanup --------------------------------

#[test]
fn deleting_an_event_removes_its_unreferenced_snapshot_and_keeps_shared_ones() {
    let conn = mem();
    let (a, b) = (snap(&host_rule("a")), snap(&host_rule("b")));
    for s in [&a, &b] {
        insert_snapshot(&conn, s).unwrap();
    }
    add_event(&conn, 1, Some(a.rule_hash));
    add_event(&conn, 2, Some(b.rule_hash));
    add_event(&conn, 3, Some(b.rule_hash));
    add_event(&conn, 4, None);
    assert_eq!(delete_oldest_events(&conn, 2).unwrap(), 2);
    let hashes: Vec<Vec<u8>> = conn
        .prepare("SELECT rule_hash FROM rules_seen")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(hashes, vec![b.rule_hash.to_vec()], "a went, b is shared");
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
    // An unreferenced snapshot, as a legacy or interrupted file could hold.
    let old = snap(&host_rule("old"));
    insert_snapshot(rig.engine.connection(), &old).unwrap();
    let tight = SnapshotLimits::new(1, limits.max_total_bytes);
    let new = snap(&host_rule("new"));
    rig.engine.fault_before_commit_for_test(fail_once());
    let result = rig.engine.transact(TxnKind::Insert, |conn| {
        admit_snapshot(conn, &tight, &new).map_err(WriteError::from)?;
        Ok(1)
    });
    assert!(result.is_err());
    assert_eq!(rig.count("rules_seen"), 1);
    let kept: Vec<u8> = rig
        .engine
        .connection()
        .query_row("SELECT rule_hash FROM rules_seen", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, old.rule_hash.to_vec(), "the eviction was rolled back");
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
            .map(|j| with_snapshot((chunk * 32 + j) as i64, &fat(chunk * 32 + j)))
            .collect();
        events += items.len() as i64;
        let batch = pinned(&rig, items);
        rig.engine.commit_pinned(&batch, &rig.registry).unwrap();
    }
    assert_eq!(count_of(&rig, "events"), events, "no event was refused");
    let used: i64 = rig
        .engine
        .connection()
        .query_row(
            "SELECT sum(length(CAST(snapshot AS BLOB))) FROM rules_seen",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(used as u64 <= limits.max_total_bytes, "{used}");
    assert!((count_of(&rig, "rules_seen")) < events, "some were refused");
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
