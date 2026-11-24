//! PR 5: capture. The mapping from a daemon `Event` to the D4 allowlist, the
//! coalescing, and the whole pipeline (admission, pinned queue, writer
//! thread, commit) against a real store in a tempdir.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use rusqlite::Connection as Db;
use snitchwatch_proto::protocol::{Connection, Event, Operator, Rule};

use super::super::admission::AdmissionConfig;
use super::super::budget::Budget;
use super::super::durable::tests::{engine_at, manual_clock};
use super::super::pins::PinRegistry;
use super::super::record::{Action, AnsweredBy};
use super::super::test_support::{state_dir, tempdir};
use super::super::{enable, locate, Consent, Location, Mode};
use super::*;

const MIB: u64 = 1024 * 1024;

fn conn(path: &str, host: &str, port: u32) -> Connection {
    Connection {
        protocol: "tcp".into(),
        src_ip: "192.168.1.10".into(),
        src_port: 51544,
        dst_ip: "140.82.121.4".into(),
        dst_host: host.into(),
        dst_port: port,
        user_id: 1000,
        process_id: 4242,
        process_path: path.into(),
        ..Connection::default()
    }
}

fn rule(name: &str, action: &str) -> Rule {
    Rule {
        name: name.into(),
        action: action.into(),
        duration: "always".into(),
        enabled: true,
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "process.path".into(),
            data: "/usr/bin/curl".into(),
            ..Operator::default()
        }),
        ..Rule::default()
    }
}

fn event(c: Connection, r: Rule, ms: i64) -> Event {
    Event {
        time: String::new(),
        connection: Some(c),
        rule: Some(r),
        unixnano: ms * 1_000_000,
    }
}

fn default_event(c: Connection, action: &str, ms: i64) -> Event {
    event(
        c,
        Rule {
            name: String::new(),
            description: crate::daemon_contract::DEFAULT_ACTION_MARKER.into(),
            action: action.into(),
            duration: "once".into(),
            ..Rule::default()
        },
        ms,
    )
}

fn simple(path: &str, host: &str, ms: i64) -> Event {
    event(conn(path, host, 443), rule("allow-web", "allow"), ms)
}

// ---- mapping ----

#[test]
fn a_matched_rule_event_maps_to_the_d4_fields_and_nothing_else() {
    let m = map_event(&simple("/usr/bin/curl", "github.com", 1_700_000_000_123)).unwrap();
    let r = &m.record;
    assert_eq!(r.ts_ms, 1_700_000_000_123);
    assert_eq!(r.process_path.as_ref().unwrap().as_str(), "/usr/bin/curl");
    assert_eq!(r.dst_host.as_ref().unwrap().as_str(), "github.com");
    assert_eq!(r.dst_ip, Some("140.82.121.4".parse().unwrap()));
    assert_eq!(r.dst_port, Some(443));
    assert_eq!(r.protocol.as_ref().unwrap().as_str(), "tcp");
    assert_eq!(r.action, Action::Allow);
    assert_eq!(r.answered_by, AnsweredBy::Rule);
    assert_eq!(r.rule_name.as_ref().unwrap().as_str(), "allow-web");
    assert_eq!(r.rule_class.as_ref().unwrap().as_str(), "user");
    assert_eq!(r.rule_hash, Some(m.identity.as_ref().unwrap().hash));
    assert_eq!(r.repeat_count, 1);
    assert_eq!(
        (r.src_ip, r.src_port),
        (Some("192.168.1.10".parse().unwrap()), Some(51544))
    );
    assert_eq!(r.decided_ts_ms, None);
    assert!(!m.nolog);
}

#[test]
fn a_deny_rule_is_deny_and_a_blocklist_rule_is_blocklist() {
    let deny = event(conn("/a", "h", 80), rule("block-ads", "deny"), 1);
    assert_eq!(map_event(&deny).unwrap().record.action, Action::Deny);
    let bl = event(conn("/a", "h", 80), rule("z00-blocklist:ads", "deny"), 1);
    let m = map_event(&bl).unwrap();
    assert_eq!(m.record.action, Action::Blocklist);
    assert_eq!(
        m.record.rule_class.as_ref().unwrap().as_str(),
        "z00-blocklist"
    );
}

#[test]
fn a_default_action_event_has_no_rule_name_hash_or_class() {
    let allow = map_event(&default_event(conn("/a", "h", 80), "allow", 5)).unwrap();
    let deny = map_event(&default_event(conn("/a", "h", 80), "deny", 5)).unwrap();
    let reject = map_event(&default_event(conn("/a", "h", 80), "reject", 5)).unwrap();
    assert_eq!(allow.record.action, Action::DefaultAllow);
    assert_eq!(deny.record.action, Action::DefaultDeny);
    assert_eq!(reject.record.action, Action::DefaultDeny);
    for m in [&allow, &deny, &reject] {
        assert_eq!(m.record.answered_by, AnsweredBy::Default);
        assert!(m.record.rule_name.is_none());
        assert!(m.record.rule_hash.is_none());
        assert!(m.record.rule_class.is_none());
        assert!(m.identity.is_none());
    }
}

#[test]
fn unusable_strings_and_ports_become_none_not_garbage() {
    let c = Connection {
        process_path: String::new(),
        dst_host: String::new(),
        dst_ip: "not an ip".into(),
        dst_port: 70_000,
        protocol: String::new(),
        ..Connection::default()
    };
    let r = map_event(&event(c, rule("r", "allow"), 1)).unwrap().record;
    assert!(r.process_path.is_none() && r.dst_host.is_none() && r.protocol.is_none());
    assert_eq!((r.dst_ip, r.dst_port), (None, None));
}

#[test]
fn an_event_without_a_connection_or_rule_maps_to_nothing() {
    let mut e = simple("/a", "h", 1);
    e.rule = None;
    assert!(map_event(&e).is_none());
    let mut e = simple("/a", "h", 1);
    e.connection = None;
    assert!(map_event(&e).is_none());
}

#[test]
fn a_nolog_rule_event_is_marked_so_it_is_never_recorded() {
    let mut r = rule("quiet", "allow");
    r.nolog = true;
    let m = map_event(&event(conn("/a", "h", 80), r, 1)).unwrap();
    assert!(m.nolog);
}

/// The identity `Err` path. A rule whose operator type this build does not
/// identify (`range`, `complex`) is captured with no `rule_hash`.
#[test]
fn a_rule_without_an_identity_is_captured_with_no_hash_and_no_snapshot() {
    for kind in ["range", "complex", "something-new"] {
        let mut r = rule("odd-rule", "deny");
        r.operator = Some(Operator {
            r#type: kind.into(),
            operand: "dest.port".into(),
            data: "SECRETCANARY-1-65535".into(),
            ..Operator::default()
        });
        let m = map_event(&event(conn("/a", "h", 80), r, 1)).unwrap();
        assert!(m.identity.is_none(), "{kind}");
        assert!(m.record.rule_hash.is_none(), "{kind}");
        // The name and class are D4 fields and stay.
        assert_eq!(m.record.rule_name.as_ref().unwrap().as_str(), "odd-rule");
        assert_eq!(m.record.rule_class.as_ref().unwrap().as_str(), "user");
        assert_eq!(m.record.action, Action::Deny);
        assert_eq!(m.record.answered_by, AnsweredBy::Rule);
    }
}

// ---- coalescing ----

fn maps(events: &[Event]) -> Vec<Mapped> {
    events.iter().filter_map(map_event).collect()
}

#[test]
fn consecutive_identical_events_within_a_second_become_one_row_with_an_exact_count() {
    let events: Vec<Event> = (0..5).map(|i| simple("/a", "h", 1_000 + i * 100)).collect();
    let out = coalesce(maps(&events));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].record.repeat_count, 5);
    assert_eq!(out[0].record.ts_ms, 1_000, "the first one's time");
}

#[test]
fn the_window_is_one_second_from_the_first_event() {
    let events = [
        simple("/a", "h", 1_000),
        simple("/a", "h", 2_000),
        simple("/a", "h", 2_001),
    ];
    let out = coalesce(maps(&events));
    let counts: Vec<u32> = out.iter().map(|m| m.record.repeat_count).collect();
    assert_eq!(counts, [2, 1]);
}

#[test]
fn only_consecutive_events_coalesce_and_any_differing_field_splits() {
    let a = simple("/a", "h", 1);
    let b = simple("/a", "other", 2);
    let out = coalesce(maps(&[a.clone(), b, a]));
    assert_eq!(out.len(), 3);
    let by_port = event(conn("/a", "h", 8443), rule("allow-web", "allow"), 1);
    let by_rule = event(conn("/a", "h", 443), rule("other-rule", "allow"), 1);
    let by_action = event(conn("/a", "h", 443), rule("allow-web", "deny"), 1);
    let by_proc = simple("/b", "h", 1);
    for odd in [by_port, by_rule, by_action, by_proc] {
        assert_eq!(coalesce(maps(&[simple("/a", "h", 1), odd])).len(), 2);
    }
}

#[test]
fn an_edited_rule_with_the_same_name_is_not_merged_with_its_old_self() {
    let mut edited = rule("allow-web", "allow");
    edited.operator.as_mut().unwrap().data = "/usr/bin/wget".into();
    let events = [simple("/a", "h", 1), event(conn("/a", "h", 443), edited, 2)];
    assert_eq!(coalesce(maps(&events)).len(), 2);
}

#[test]
fn coalescing_never_loses_or_invents_an_event() {
    let mut events = Vec::new();
    for i in 0..200 {
        events.push(simple("/a", if i % 7 == 0 { "x" } else { "y" }, i * 40));
    }
    let total: u32 = coalesce(maps(&events))
        .iter()
        .map(|m| m.record.repeat_count)
        .sum();
    assert_eq!(total, 200);
}

// ---- the pipeline ----

struct Rig {
    _root: tempfile::TempDir,
    location: Location,
    registry: Arc<PinRegistry>,
    clock: Arc<AtomicI64>,
    capture: Capture,
}

fn config_for(budget: &Budget) -> CaptureConfig {
    CaptureConfig {
        queue_max_bytes: super::super::writer::QUEUE_MAX_BYTES,
        queue_max_count: super::super::writer::QUEUE_MAX_COUNT,
        admission: AdmissionConfig::for_retained_bytes(budget.main_bytes),
        heartbeat_every: std::time::Duration::from_secs(3600),
    }
}

fn unlimited(mut c: CaptureConfig) -> CaptureConfig {
    c.admission.process_rate = 1_000_000;
    c.admission.process_burst = 1_000_000;
    c.admission.global_rate = 1_000_000;
    c.admission.global_burst = 1_000_000;
    c
}

fn rig_with(tweak: impl FnOnce(CaptureConfig) -> CaptureConfig) -> Rig {
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
    let (clock, read) = manual_clock(10_000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let engine = engine_at(&location);
    let config = tweak(config_for(engine.budget()));
    let capture = Capture::spawn(engine, registry.clone(), config).unwrap();
    Rig {
        _root: root,
        location,
        registry,
        clock,
        capture,
    }
}

fn rig() -> Rig {
    rig_with(|c| c)
}

#[derive(Debug)]
struct Row {
    process_path: Option<String>,
    dst_host: Option<String>,
    action: String,
    answered_by: String,
    rule_name: Option<String>,
    rule_hash: Option<Vec<u8>>,
    repeat_count: u32,
}

struct Done {
    rows: Vec<Row>,
    stats: CaptureStats,
    registry: Arc<PinRegistry>,
    location: Location,
    _root: tempfile::TempDir,
}

impl Rig {
    fn send(&self, events: &[Event]) {
        let refs: Vec<&Event> = events.iter().collect();
        self.capture.observe(&refs);
    }

    /// Flush the queue, stop the writer, then read the store.
    fn finish(self) -> Done {
        let stats = self.capture.shutdown();
        let db = Db::open(self.location.db_path()).unwrap();
        let rows = db
            .prepare(
                "SELECT process_path, dst_host, action, answered_by, rule_name, rule_hash, \
                 repeat_count FROM events ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok(Row {
                    process_path: r.get(0)?,
                    dst_host: r.get(1)?,
                    action: r.get(2)?,
                    answered_by: r.get(3)?,
                    rule_name: r.get(4)?,
                    rule_hash: r.get(5)?,
                    repeat_count: r.get(6)?,
                })
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        Done {
            rows,
            stats,
            registry: self.registry,
            location: self.location,
            _root: self._root,
        }
    }
}

fn count(db: &Db, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn events_total(rows: &[Row]) -> u64 {
    rows.iter().map(|r| u64::from(r.repeat_count)).sum()
}

#[test]
fn events_reach_the_store_through_the_pinned_writer_and_release_their_pins() {
    let rig = rig();
    rig.send(&[
        simple("/usr/bin/curl", "github.com", 1),
        default_event(conn("/usr/bin/wget", "example.org", 80), "deny", 2),
    ]);
    let done = rig.finish();
    assert_eq!(done.rows.len(), 2);
    assert_eq!(done.rows[0].rule_name.as_deref(), Some("allow-web"));
    assert_eq!(done.rows[0].dst_host.as_deref(), Some("github.com"));
    assert_eq!(done.rows[0].answered_by, "rule");
    assert_eq!(done.rows[1].action, "default-deny");
    assert_eq!(done.rows[1].rule_name, None);
    // Committed, so the pins went in the same unit as the rows.
    assert!(done.registry.is_empty());
    let db = Db::open(done.location.db_path()).unwrap();
    assert!(super::super::session::read_watermark(&db)
        .unwrap()
        .is_some());
    // The matched rule's snapshot was stored with it.
    assert_eq!(count(&db, "SELECT count(*) FROM rules_seen"), 1);
}

#[test]
fn many_admitted_events_commit_in_batches_within_rows_per_txn() {
    let rig = rig_with(unlimited);
    let events: Vec<Event> = (0..300)
        .map(|i| simple(&format!("/usr/bin/p{i}"), "h", i))
        .collect();
    rig.send(&events);
    let done = rig.finish();
    assert_eq!(done.rows.len(), 300, "{:?}", done.stats);
    assert_eq!(done.stats.written, 300);
    assert_eq!(done.stats.lost_write, 0);
    assert!(done.registry.is_empty());
}

#[test]
fn coalesced_rows_keep_every_event_in_their_count() {
    let rig = rig();
    let burst: Vec<Event> = (0..40).map(|i| simple("/a", "h", 1_000 + i * 10)).collect();
    rig.send(&burst);
    let done = rig.finish();
    assert_eq!(done.rows.len(), 1);
    assert_eq!(done.rows[0].repeat_count, 40);
    assert_eq!(done.stats.seen, 40);
    assert_eq!(done.stats.written, 40);
}

#[test]
fn every_event_is_a_row_count_or_a_counted_drop() {
    let rig = rig();
    // One process, 400 distinct destinations at once: burst 100 passes.
    let events: Vec<Event> = (0..400)
        .map(|i| simple("/tmp/flood", &format!("h{i}"), i))
        .collect();
    rig.send(&events);
    let done = rig.finish();
    let s = done.stats;
    assert_eq!(s.seen, 400);
    assert_eq!(s.refused_process_rate, 300);
    assert_eq!(events_total(&done.rows), s.written);
    assert_eq!(
        s.written
            + s.skipped_nolog
            + s.refused_process_rate
            + s.refused_byte_share
            + s.refused_global_rate
            + s.dropped_queue
            + s.dropped_pin
            + s.lost_write,
        s.seen
    );
}

#[test]
fn a_flood_from_one_process_does_not_evict_or_starve_another() {
    let rig = rig();
    let mut events: Vec<Event> = (0..5_000)
        .map(|i| simple("/tmp/flood", &format!("h{i}"), i))
        .collect();
    events.push(simple("/usr/bin/firefox", "mozilla.org", 6_000));
    rig.send(&events);
    let done = rig.finish();
    assert!(
        done.rows
            .iter()
            .any(|r| r.process_path.as_deref() == Some("/usr/bin/firefox")),
        "the other process's row is stored"
    );
    let flood = done
        .rows
        .iter()
        .filter(|r| r.process_path.as_deref() == Some("/tmp/flood"))
        .count();
    assert_eq!(flood, 100, "the flooder got its burst and no more");
}

#[test]
fn a_mixed_size_flood_stops_at_its_byte_share_and_others_small_records_stay() {
    // 64 KiB retained: a 16 KiB share.
    let rig = rig_with(|c| {
        let mut c = unlimited(c);
        c.admission.retained_bytes = 64 * 1024;
        c
    });
    let big_path = format!("/opt/{}", "p".repeat(4000));
    let big_host = "h".repeat(250);
    let mut events: Vec<Event> = (0..100)
        .map(|i| {
            let mut c = conn(&big_path, &big_host, 443);
            c.dst_port = 1000 + i;
            event(c, rule("allow-web", "allow"), i64::from(i))
        })
        .collect();
    events.extend((0..20).map(|i| simple("/usr/bin/small", &format!("s{i}"), 500 + i)));
    rig.send(&events);
    let done = rig.finish();
    let big = done
        .rows
        .iter()
        .filter(|r| r.process_path.as_deref() == Some(big_path.as_str()))
        .count();
    let small = done
        .rows
        .iter()
        .filter(|r| r.process_path.as_deref() == Some("/usr/bin/small"))
        .count();
    assert!(
        (1..=4).contains(&big),
        "{big} large rows within a 16 KiB share"
    );
    assert_eq!(small, 20);
    assert!(done.stats.refused_byte_share >= 96);
}

#[test]
fn a_clock_step_refills_the_process_bucket() {
    let rig = rig();
    let first: Vec<Event> = (0..100)
        .map(|i| simple("/tmp/p", &format!("a{i}"), i))
        .collect();
    rig.send(&first);
    rig.clock.fetch_add(1_000, Ordering::SeqCst);
    let second: Vec<Event> = (0..30)
        .map(|i| simple("/tmp/p", &format!("b{i}"), 2_000 + i))
        .collect();
    rig.send(&second);
    let done = rig.finish();
    assert_eq!(done.rows.len(), 120, "100 burst + 20 refilled in a second");
}

#[test]
fn nolog_events_are_never_recorded_and_traffic_nobody_reported_leaves_no_trace() {
    let rig = rig();
    let mut quiet = rule("quiet", "allow");
    quiet.nolog = true;
    rig.send(&[event(conn("/a", "h", 80), quiet, 1)]);
    // A stock daemon reports no default-applied events at all: an empty
    // batch, then nothing. No row, no fabricated gap.
    rig.send(&[]);
    let done = rig.finish();
    assert!(done.rows.is_empty());
    assert_eq!(done.stats.skipped_nolog, 1);
    assert_eq!(done.stats.seen, 1);
    let db = Db::open(done.location.db_path()).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM gaps"), 0);
}

#[test]
fn a_rule_without_an_identity_is_stored_with_a_null_hash_and_no_snapshot() {
    let rig = rig();
    let mut odd = rule("odd-rule", "deny");
    odd.operator = Some(Operator {
        r#type: "range".into(),
        operand: "dest.port".into(),
        data: "RANGECANARY-1-65535".into(),
        ..Operator::default()
    });
    rig.send(&[event(conn("/a", "h", 80), odd, 1)]);
    let done = rig.finish();
    assert_eq!(done.rows.len(), 1);
    assert_eq!(done.rows[0].rule_name.as_deref(), Some("odd-rule"));
    assert!(done.rows[0].rule_hash.is_none());
    assert_eq!(done.stats.unidentified_rules, 1);
    let db = Db::open(done.location.db_path()).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM rules_seen"), 0);
    let bytes = std::fs::read(done.location.db_path()).unwrap();
    assert!(!contains(&bytes, "RANGECANARY"));
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// D4: canaries in every field the allowlist excludes never reach a byte of
/// the database file.
#[test]
fn excluded_fields_never_reach_the_store() {
    let rig = rig();
    let mut c = conn("/usr/bin/curl", "github.com", 443);
    c.user_id = 7_777_777;
    c.process_id = 8_888_888;
    c.process_cwd = "/home/CWDCANARY".into();
    c.process_args = vec!["curl".into(), "https://u:ARGSCANARY@host".into()];
    c.process_env.insert("TOKEN".into(), "ENVCANARY".into());
    c.process_checksums.insert("md5".into(), "SUMCANARY".into());
    c.process_tree = vec![snitchwatch_proto::protocol::StringInt {
        key: "TREECANARY".into(),
        value: 1,
    }];
    let mut r = rule("allow-web", "allow");
    r.description = "DESCCANARY".into();
    r.operator = Some(Operator {
        r#type: "simple".into(),
        operand: "process.command".into(),
        data: "COMMANDCANARY".into(),
        ..Operator::default()
    });
    rig.send(&[event(c, r, 1)]);
    let done = rig.finish();
    assert_eq!(done.rows.len(), 1);
    let db_path = done.location.db_path();
    let mut all = std::fs::read(&db_path).unwrap();
    let mut journal = db_path.into_os_string();
    journal.push("-journal");
    all.extend(std::fs::read(journal).unwrap_or_default());
    for canary in [
        "ARGSCANARY",
        "ENVCANARY",
        "CWDCANARY",
        "SUMCANARY",
        "TREECANARY",
        "DESCCANARY",
        "COMMANDCANARY",
    ] {
        assert!(!contains(&all, canary), "{canary} reached the store");
    }
    // UID and PID are withheld. Source address and port are allowlisted.
    let db = Db::open(done.location.db_path()).unwrap();
    let dump: String = db
        .query_row(
            "SELECT group_concat(coalesce(ts_ms,'') || '|' || coalesce(decided_ts_ms,'') || '|' \
             || coalesce(process_path,'') || '|' || coalesce(dst_host,'') || '|' || \
             coalesce(dst_ip,'') || '|' || coalesce(dst_port,'') || '|' || \
             coalesce(protocol,'') || '|' || action || '|' || answered_by || '|' || \
             coalesce(rule_name,'') || '|' || coalesce(rule_class,'') || '|' || repeat_count || \
             '|' || coalesce(src_port,'') || '|' || coalesce(src_ip,'')) FROM events",
            [],
            |r| r.get(0),
        )
        .unwrap();
    for absent in ["7777777", "8888888"] {
        assert!(!dump.contains(absent), "{absent} in {dump}");
    }
    let stored: (Option<i64>, Option<String>) = db
        .query_row("SELECT src_port, src_ip FROM events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(stored, (Some(51544), Some("192.168.1.10".into())));
}

#[test]
fn a_full_queue_drops_and_counts_and_leaks_no_pin() {
    let registry = Arc::new(PinRegistry::new());
    let mut config = config_for(&Budget::new(16 * MIB).unwrap());
    config.queue_max_count = 3;
    let capture = Capture::new_unstarted_for_test(registry.clone(), unlimited(config));
    let events: Vec<Event> = (0..10).map(|i| simple("/a", &format!("h{i}"), i)).collect();
    let refs: Vec<&Event> = events.iter().collect();
    capture.observe(&refs);
    let stats = capture.stats();
    assert_eq!(capture.queued_for_test(), 3);
    assert_eq!(stats.dropped_queue, 7);
    // One live pin per queued item, plus ONE for all the dropped ones: the
    // watermark may not pass events that were never recorded.
    assert_eq!(registry.len(), 4);
    assert_eq!(
        capture.shared.losses.lock().unwrap().queue.pending_count(),
        7
    );
}

#[test]
fn admission_refusals_hold_exactly_one_pin_however_many_there_are() {
    let registry = Arc::new(PinRegistry::new());
    let mut config = config_for(&Budget::new(16 * MIB).unwrap());
    config.admission.process_rate = 1;
    config.admission.process_burst = 1;
    let capture = Capture::new_unstarted_for_test(registry.clone(), config);
    for round in 0..3 {
        let events: Vec<Event> = (0..20)
            .map(|i| simple("/a", &format!("h{round}-{i}"), i))
            .collect();
        let refs: Vec<&Event> = events.iter().collect();
        capture.observe(&refs);
    }
    let refused = capture.stats().refused_process_rate;
    assert!(refused >= 50, "{refused}");
    assert_eq!(registry.len(), capture.queued_for_test() + 1);
    let pending = capture
        .shared
        .losses
        .lock()
        .unwrap()
        .admission
        .pending_count();
    assert_eq!(pending, refused);
}

#[test]
fn a_snapshot_that_cannot_be_built_is_counted() {
    let registry = Arc::new(PinRegistry::new());
    let capture = Capture::new_unstarted_for_test(
        registry,
        unlimited(config_for(&Budget::new(16 * MIB).unwrap())),
    );
    let mut m = map_event(&event(conn("/a", "h", 80), rule("r", "deny"), 1)).unwrap();
    m.identity.as_mut().unwrap().canonical.push(0);
    capture.shared.enqueue(m, 1, None);
    assert_eq!(capture.stats().snapshot_errors, 1);
    assert_eq!(capture.queued_for_test(), 1);
}

fn locked_rig() -> (Capture, Arc<PinRegistry>, Db, tempfile::TempDir) {
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
    let registry = Arc::new(PinRegistry::new());
    let engine = engine_at(&location);
    let config = unlimited(config_for(engine.budget()));
    let capture = Capture::spawn(engine, registry.clone(), config).unwrap();
    // The "slow disk": a transaction held open by another connection.
    let blocker = Db::open(location.db_path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    (capture, registry, blocker, root)
}

#[test]
fn shutdown_with_a_locked_database_is_bounded() {
    let (capture, _registry, _blocker, _root) = locked_rig();
    let events: Vec<Event> = (0..50).map(|i| simple("/a", &format!("h{i}"), i)).collect();
    let refs: Vec<&Event> = events.iter().collect();
    capture.observe(&refs);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let start = std::time::Instant::now();
    let stats = capture.shutdown();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(stats.written, 0);
    assert_eq!(stats.lost_write, 50);
}

#[test]
fn a_batch_that_fails_once_is_retried_and_written() {
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
    let registry = Arc::new(PinRegistry::new());
    let mut engine = engine_at(&location);
    let tries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t = tries.clone();
    // Commit 0 is the session start; commit 1 is the batch's first try.
    engine.fault_before_commit_for_test(Box::new(move || {
        (t.fetch_add(1, Ordering::SeqCst) == 1)
            .then(|| super::super::writer::WriteError::Io(std::io::Error::other("injected")))
    }));
    let config = unlimited(config_for(engine.budget()));
    let capture = Capture::spawn(engine, registry.clone(), config).unwrap();
    capture.observe(&[&simple("/a", "first", 1)]);
    let stats = capture.shutdown();
    assert_eq!((stats.lost_write, stats.written), (0, 1));
    assert_eq!(registry.len(), 0);
}

#[test]
fn a_failed_commit_loses_the_batch_and_holds_its_oldest_pin_until_the_gap_row_commits() {
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
    let (clock, read) = manual_clock(50_000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let mut engine = engine_at(&location);
    let failed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let f = failed.clone();
    engine.fault_before_commit_for_test(Box::new(move || {
        // Commit 0 is the session start; the first batch fails its try and
        // its retry (commits 1 and 2).
        matches!(f.fetch_add(1, Ordering::SeqCst), 1 | 2)
            .then(|| super::super::writer::WriteError::Io(std::io::Error::other("injected")))
    }));
    let config = unlimited(config_for(engine.budget()));
    let capture = Capture::spawn(engine, registry.clone(), config).unwrap();
    capture.observe(&[&simple("/a", "first", 1)]);
    // Wait for the writer to take (and fail) that batch before the next.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while capture.stats().lost_write == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the first batch was never tried"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    clock.store(60_000, Ordering::SeqCst);
    capture.observe(&[&simple("/a", "second", 2)]);
    // The watermark may not pass the first lost event: its pin stays, alone,
    // until the `write-failed` row that covers it commits.
    while capture.stats().written == 0 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.oldest(), Some(50_000));
    let stats = capture.shutdown();
    assert_eq!((stats.lost_write, stats.written), (1, 1));
    assert!(registry.is_empty(), "released with its gap row");
    let db = Db::open(location.db_path()).unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM gaps WHERE cause = 'write-failed' AND start_ms = 50000"
        ),
        1
    );
}

#[test]
fn the_capture_hook_runs_without_blocking_when_the_writer_is_far_behind() {
    let registry = Arc::new(PinRegistry::new());
    let capture = Capture::new_unstarted_for_test(
        registry,
        unlimited(config_for(&Budget::new(16 * MIB).unwrap())),
    );
    let events: Vec<Event> = (0..2_000)
        .map(|i| simple("/a", &format!("h{i}"), i))
        .collect();
    let refs: Vec<&Event> = events.iter().collect();
    let start = std::time::Instant::now();
    capture.observe(&refs);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

// ---- start(): the only way the bridge turns capture on ----

fn consent() -> Consent {
    Consent {
        text_id: "t".into(),
        accepted_ms: 1,
    }
}

#[test]
fn start_with_history_off_makes_nothing_in_either_mode() {
    let root = tempdir();
    let state = state_dir(&root);
    for mode in [Mode::User, Mode::System] {
        assert!(start(mode, Some(&state)).is_none());
        assert!(start(mode, None).is_none());
    }
    assert_eq!(
        std::fs::read_dir(&state).unwrap().count(),
        0,
        "no directory"
    );
}

#[test]
fn start_with_history_enabled_opens_the_store_and_a_writer() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    let capture = start(Mode::User, Some(&state)).expect("enabled");
    capture.observe(&[&simple("/a", "h", 1)]);
    let stats = Arc::try_unwrap(capture)
        .ok()
        .expect("sole owner")
        .shutdown();
    assert_eq!(stats.written, 1);
}

#[test]
fn the_system_bridge_refuses_a_state_directory_that_is_not_its_own() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    assert!(start(Mode::System, Some(&state)).is_none());
}

#[test]
fn the_probe_runs_at_the_start_of_every_observe() {
    let capture = Capture::new_unstarted_for_test(
        Arc::new(PinRegistry::new()),
        config_for(&Budget::new(16 * MIB).unwrap()),
    );
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = hits.clone();
    capture.set_probe_for_test(Box::new(move || {
        h.fetch_add(1, Ordering::SeqCst);
    }));
    capture.observe(&[]);
    capture.observe(&[&simple("/a", "h", 1)]);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn pending_prompt_keeps_arrival_pin_and_drop_records_cancelled_once() {
    let rig = rig();
    let guard = rig.capture.begin_decision(&conn("/a", "h", 443)).unwrap();
    assert_eq!(rig.registry.oldest(), Some(10_000));
    rig.clock.store(12_000, Ordering::Relaxed);
    drop(guard);
    let done = rig.finish();
    assert!(done.registry.is_empty());
    let db = Db::open(done.location.db_path()).unwrap();
    let outcome: (String, i64, i64) = db
        .query_row(
            "SELECT prompt_outcome, ts_ms, decided_ts_ms FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(done.rows.len(), 1);
    assert_eq!(outcome, ("cancelled".into(), 10_000, 12_000));
}

#[test]
fn accepting_before_a_generation_or_consent_change_never_writes_a_snapshot() {
    let rig = rig();
    let connection = conn("/a", "h", 443);
    let reply = rule("test", "allow");
    let outcome = || DecisionOutcome::Replied {
        rule: &reply,
        answered_by: AnsweredBy::Gui,
    };
    let first = rig.capture.begin_decision(&connection).unwrap();
    rig.capture.shared.generation.advance().unwrap();
    first.settle(outcome());
    assert!(rig.registry.is_empty());
    assert_eq!(rig.capture.stats().seen, 0);

    let second = rig.capture.begin_decision(&connection).unwrap();
    *rig.capture.shared.control.lock().unwrap() = Some(Arc::new(Consent {
        text_id: "new".into(),
        accepted_ms: 2,
    }));
    second.settle(outcome());

    *rig.capture.shared.control.lock().unwrap() = None;
    assert!(rig.capture.begin_decision(&connection).is_none());
    let done = rig.finish();
    assert!(done.rows.is_empty());
    assert!(done.registry.is_empty());
    let db = Db::open(done.location.db_path()).unwrap();
    let snapshots: i64 = db
        .query_row("SELECT count(*) FROM rules_seen", [], |row| row.get(0))
        .unwrap();
    assert_eq!(snapshots, 0);
}

#[test]
fn a_rejected_prompt_keeps_its_arrival_pin_in_the_queue_loss_tally() {
    let rig = rig_with(|mut config| {
        config.queue_max_count = 0;
        config.queue_max_bytes = 0;
        config
    });
    let guard = rig.capture.begin_decision(&conn("/a", "h", 443)).unwrap();
    rig.clock.store(12_000, Ordering::Relaxed);
    drop(guard);
    assert_eq!(rig.capture.stats().dropped_queue, 1);
    assert_eq!(rig.registry.oldest(), Some(10_000));
    assert_eq!(rig.registry.len(), 1);
}

#[test]
fn a_settled_prompt_and_matching_daemon_event_keep_both_flags_and_one_snapshot() {
    let rig = rig();
    let connection = conn("/a", "h", 443);
    let reply = rule("test", "allow");
    let guard = rig.capture.begin_decision(&connection).unwrap();
    rig.clock.store(11_000, Ordering::Relaxed);
    guard.settle(DecisionOutcome::Replied {
        rule: &reply,
        answered_by: AnsweredBy::Gui,
    });
    rig.send(&[event(connection, reply, 11_100)]);

    let done = rig.finish();
    assert!(done.registry.is_empty());
    assert_eq!(done.rows.len(), 2);
    let db = Db::open(done.location.db_path()).unwrap();
    let (possible_duplicates, snapshots): (i64, i64) = db
        .query_row(
            "SELECT (SELECT count(*) FROM history_events WHERE possible_duplicate = 1), \
                    (SELECT count(*) FROM rules_seen)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((possible_duplicates, snapshots), (2, 1));
}
