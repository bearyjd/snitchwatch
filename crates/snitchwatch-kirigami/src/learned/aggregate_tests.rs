//! The aggregator: keys, the four classes and their precedence, the caps and
//! the eviction within each class, the drop ring, and merging.

use std::collections::{BTreeSet, VecDeque};

use super::*;
use crate::bridge_runtime::PendingRow;

fn row(process: &str, path: Option<&str>, host: &str, ip: &str, port: u16) -> PendingRow {
    PendingRow {
        process: process.into(),
        process_path: path.map(str::to_string),
        dst_host: host.into(),
        dst_ip: ip.into(),
        port,
        protocol: "tcp".into(),
    }
}

/// One plain HTTPS connection of `path` at `at`.
fn https(path: &str, at: i64) -> Program {
    Program::observed(
        &row("x", Some(path), "example.com", "93.184.216.34", 443),
        at,
    )
}

/// A connection to a bare IP (a class A flag).
fn bare(path: &str, at: i64) -> Program {
    Program::observed(&row("x", Some(path), "", "203.0.113.9", 443), at)
}

fn named(name: &str, at: i64) -> Program {
    Program::observed(&row(name, None, "example.com", "93.184.216.34", 443), at)
}

fn agg_with(programs: impl IntoIterator<Item = Program>) -> Aggregate {
    let mut agg = Aggregate::default();
    for program in programs {
        agg.absorb(program);
    }
    agg
}

// --- keys -------------------------------------------------------------------

#[test]
fn an_observation_records_the_key_parts() {
    let p = Program::observed(
        &row(
            "Curl",
            Some("/usr/bin/curl"),
            "Example.COM.",
            "1.2.3.4",
            443,
        ),
        10,
    );
    assert_eq!(p.key(), "path:/usr/bin/curl");
    assert_eq!(
        p.destinations,
        BTreeMap::from([("example.com".to_string(), 1)])
    );
    assert_eq!(p.ports, BTreeSet::from([443]));
    assert_eq!(p.protocols, BTreeSet::from(["tcp".to_string()]));
    assert_eq!((p.connections, p.bare_ip, p.uncommon_port), (1, 0, 0));
    assert_eq!((p.first_seen_ms, p.last_seen_ms), (10, 10));

    // Without a hostname the address is the destination and counts as bare.
    let b = Program::observed(&row("nc", None, "", "203.0.113.9", 4444), 11);
    assert_eq!(b.key(), "name:nc");
    assert_eq!(b.destinations.keys().next().unwrap(), "203.0.113.9");
    assert_eq!((b.bare_ip, b.uncommon_port), (1, 1));
}

#[test]
fn the_exact_path_is_the_key_and_the_display_is_a_sanitised_copy() {
    let long_a = format!("/usr/bin/{}a", "d".repeat(100));
    let long_b = format!("/usr/bin/{}b", "d".repeat(100));
    let mut agg = Aggregate::default();
    agg.absorb(https(&long_a, 1));
    agg.absorb(https(&long_b, 2));
    // Two paths that differ only after the display's 80-character cap stay two.
    assert_eq!(agg.c.len(), 2);
    assert!(agg.c.contains_key(&path_key(&long_a)) && agg.c.contains_key(&path_key(&long_b)));

    let hostile = "/tmp/\u{202e}evil\n\u{0}<b>x</b>";
    let p = https(hostile, 1);
    // Control characters are replaced (so JSON can never take six bytes for
    // one), and the program says its path is no longer the exact one.
    assert_eq!(p.raw_path.as_deref(), Some("/tmp/\u{202e}evil??<b>x</b>"));
    assert!(p.truncated, "no exact-path rule may come from it");
    assert!(!https("/usr/bin/curl", 1).truncated);
    assert!(
        !p.display.contains('\u{202e}') && !p.display.contains('\n'),
        "{:?}",
        p.display
    );
    assert!(!p.display.chars().any(char::is_control));
}

// --- classes ---------------------------------------------------------------------

#[test]
fn each_program_lands_in_its_class_with_a_network_flag_first() {
    let agg = agg_with([
        bare("/usr/bin/curl", 1),
        bare("/tmp/x/evil", 1),
        Program::observed(&row("nc", None, "", "203.0.113.9", 4444), 1),
        https("/tmp/x/junk", 1),
        named("kworker", 1),
        https("/usr/bin/firefox", 1),
    ]);
    assert_eq!(agg.class_of(&path_key("/usr/bin/curl")), Some(Class::A));
    assert_eq!(
        agg.class_of(&path_key("/tmp/x/evil")),
        Some(Class::A),
        "network flag beats a temp path"
    );
    assert_eq!(
        agg.class_of(&name_key("nc")),
        Some(Class::A),
        "network flag beats no path"
    );
    assert_eq!(agg.class_of(&path_key("/tmp/x/junk")), Some(Class::B));
    assert_eq!(agg.class_of(&name_key("kworker")), Some(Class::D));
    assert_eq!(agg.class_of(&path_key("/usr/bin/firefox")), Some(Class::C));
    assert_eq!(agg.class_of(&path_key("/never/seen")), None);
    assert_eq!(
        (agg.a.len(), agg.b.len(), agg.c.len(), agg.d.len()),
        (3, 1, 1, 1)
    );
}

#[test]
fn a_program_moves_to_class_a_when_it_gains_a_network_flag() {
    for first in [
        https("/usr/bin/tool", 1),
        https("/tmp/x/tool", 1),
        named("tool", 1),
    ] {
        let key = first.key().to_string();
        let mut agg = agg_with([first.clone()]);
        assert_ne!(agg.class_of(&key), Some(Class::A));
        let flagged = Program::observed(
            &row(
                &first.raw_name,
                first.raw_path.as_deref(),
                "",
                "198.51.100.7",
                443,
            ),
            2,
        );
        agg.absorb(flagged);
        assert_eq!(agg.class_of(&key), Some(Class::A), "{key}");
        assert_eq!(
            (agg.a.len(), agg.b.len(), agg.c.len(), agg.d.len()),
            (1, 0, 0, 0),
            "{key}"
        );
        let p = &agg.a[&key];
        assert_eq!(
            (p.connections, p.bare_ip, p.first_seen_ms, p.last_seen_ms),
            (2, 1, 1, 2)
        );
    }
}

#[test]
fn class_b_is_one_entry_per_parent_directory() {
    let mut agg = Aggregate::default();
    for n in 0..(MAX_DIR_PROGRAMS + 20) {
        agg.absorb(https(&format!("/tmp/junk/p{n:03}"), n as i64 + 1));
    }
    agg.absorb(https("/tmp/other/p", 5));
    assert_eq!(agg.b.len(), 2, "junk paths in one directory collapse");
    let junk = &agg.b["/tmp/junk"];
    assert_eq!(junk.programs.len(), MAX_DIR_PROGRAMS, "the list is capped");
    assert_eq!(junk.unlisted_connections, 20, "the rest are counted");
    assert_eq!(junk.last_seen_ms, (MAX_DIR_PROGRAMS + 20) as i64);
    // A listed program is still updated in place.
    agg.absorb(https("/tmp/junk/p000", 99));
    assert_eq!(
        agg.b["/tmp/junk"].programs[&path_key("/tmp/junk/p000")].connections,
        2
    );
    assert_eq!(agg.b["/tmp/junk"].last_seen_ms, 99);
}

#[test]
fn a_directory_entry_that_loses_its_last_program_to_class_a_goes_away() {
    let mut agg = agg_with([https("/tmp/x/one", 1)]);
    agg.absorb(bare("/tmp/x/one", 2));
    assert!(agg.b.is_empty());
    assert_eq!(agg.class_of(&path_key("/tmp/x/one")), Some(Class::A));
}

// --- class A: never evicted, refuses --------------------------------------------

#[test]
fn class_a_is_never_evicted_and_refuses_new_candidates_when_full() {
    let mut agg = Aggregate::default();
    for n in 0..CAP_A {
        assert_eq!(
            agg.absorb(bare(&format!("/usr/bin/a{n:04}"), n as i64 + 1)),
            Absorbed::Recorded
        );
    }
    assert_eq!(agg.a.len(), CAP_A);
    let before = agg.a.clone();

    // A flood of newcomers is refused; nothing in A goes away.
    for n in 0..30 {
        let outcome = agg.absorb(bare(&format!("/usr/bin/new{n}"), 10_000 + n));
        assert_eq!(outcome, Absorbed::Refused);
    }
    assert_eq!(agg.a, before);
    assert_eq!(agg.class_of(&path_key("/usr/bin/new0")), None);
    assert_eq!(agg.refused, 30);
    assert_eq!(agg.evicted, 0);

    // A new program whose very first connection is flagged is a refusal too,
    // but one already in A is still updated.
    assert_eq!(
        agg.absorb(bare("/usr/bin/a0000", 20_000)),
        Absorbed::Recorded
    );
    assert_eq!(agg.a[&path_key("/usr/bin/a0000")].connections, 2);
    assert_eq!(agg.a.len(), CAP_A);
}

#[test]
fn a_full_class_a_does_not_touch_the_other_classes() {
    let mut agg = agg_with([
        https("/usr/bin/calm", 1),
        named("daemon", 1),
        https("/tmp/q/x", 1),
    ]);
    for n in 0..CAP_A {
        agg.absorb(bare(&format!("/usr/bin/a{n:04}"), 2));
    }
    assert_eq!((agg.b.len(), agg.c.len(), agg.d.len()), (1, 1, 1));
    assert_eq!(agg.absorb(https("/usr/bin/calm2", 3)), Absorbed::Recorded);
    assert_eq!(agg.c.len(), 2);
}

#[test]
fn refusals_are_listed_once_per_key_with_a_growing_count() {
    let mut agg = Aggregate::default();
    for n in 0..CAP_A {
        agg.absorb(bare(&format!("/usr/bin/a{n:04}"), 1));
    }
    for _ in 0..3 {
        agg.absorb(bare("/usr/bin/intruder", 2));
    }
    assert_eq!(agg.refused, 1, "one key refused, however often it knocks");
    assert_eq!(agg.ring.len(), 1);
    let entry = &agg.ring[0];
    assert_eq!(entry.kind, DropKind::Refused);
    assert_eq!(entry.connections, 3);
    assert!(entry.flagged);
    assert_eq!(entry.display, "/usr/bin/intruder");
}

// --- the other classes evict only within themselves ----------------------------

#[test]
fn class_b_evicts_the_oldest_last_seen_directory_and_only_within_itself() {
    let mut agg = agg_with([https("/usr/bin/calm", 1), named("daemon", 1)]);
    for n in 0..CAP_A {
        agg.absorb(bare(&format!("/usr/bin/a{n:04}"), 1));
    }
    for n in 0..CAP_B {
        agg.absorb(https(&format!("/tmp/d{n:04}/x"), 100 + n as i64));
    }
    assert_eq!(agg.b.len(), CAP_B);
    // One more directory: the one last seen longest ago (d0000) goes.
    assert_eq!(
        agg.absorb(https("/tmp/newest/x", 9_999)),
        Absorbed::Recorded
    );
    assert_eq!(agg.b.len(), CAP_B);
    assert!(!agg.b.contains_key("/tmp/d0000"));
    assert!(agg.b.contains_key("/tmp/d0001") && agg.b.contains_key("/tmp/newest"));
    assert_eq!(agg.evicted, 1);
    assert_eq!(agg.dropped(), 1);
    assert_eq!(agg.ring.back().unwrap().kind, DropKind::Evicted);
    assert_eq!(agg.ring.back().unwrap().class, Class::B);
    assert_eq!(agg.ring.back().unwrap().display, "/tmp/d0000");
    // Nothing outside B changed.
    assert_eq!((agg.a.len(), agg.c.len(), agg.d.len()), (CAP_A, 1, 1));
}

#[test]
fn a_directory_seen_again_is_not_the_oldest_any_more() {
    let mut agg = Aggregate::default();
    for n in 0..CAP_B {
        agg.absorb(https(&format!("/tmp/d{n:04}/x"), 100 + n as i64));
    }
    agg.absorb(https("/tmp/d0000/y", 5_000)); // d0000 is fresh now
    agg.absorb(https("/tmp/extra/x", 5_001));
    assert!(agg.b.contains_key("/tmp/d0000"));
    assert!(!agg.b.contains_key("/tmp/d0001"), "the next oldest went");
}

#[test]
fn class_c_evicts_the_least_active_program_and_never_the_newcomer() {
    let mut agg = Aggregate::default();
    for n in 0..CAP_C {
        let path = format!("/usr/bin/c{n:04}");
        // Every program but c0003 has two connections.
        agg.absorb(https(&path, 1));
        if n != 3 {
            agg.absorb(https(&path, 2));
        }
    }
    assert_eq!(agg.c.len(), CAP_C);
    agg.absorb(https("/usr/bin/newcomer", 3));
    assert_eq!(agg.c.len(), CAP_C);
    assert!(
        agg.c.contains_key(&path_key("/usr/bin/newcomer")),
        "the newcomer stays"
    );
    assert!(
        !agg.c.contains_key(&path_key("/usr/bin/c0003")),
        "the least active went"
    );
    assert_eq!(agg.ring.back().unwrap().class, Class::C);
}

#[test]
fn class_d_evicts_the_oldest_last_seen_name() {
    let mut agg = Aggregate::default();
    for n in 0..CAP_D {
        agg.absorb(named(&format!("comm{n:03}"), 10 + n as i64));
    }
    agg.absorb(named("comm000", 500)); // seen again: no longer the oldest
    agg.absorb(named("fresh", 600));
    assert_eq!(agg.d.len(), CAP_D);
    assert!(agg.d.contains_key(&name_key("comm000")) && agg.d.contains_key(&name_key("fresh")));
    assert!(!agg.d.contains_key(&name_key("comm001")));
}

#[test]
fn a_program_that_becomes_flagged_while_a_is_full_stays_put_and_is_evicted_last() {
    let mut agg = Aggregate::default();
    for n in 0..CAP_C {
        // c0000 is the least active of all, so it would be evicted first.
        agg.absorb(https(&format!("/usr/bin/c{n:04}"), 1));
        if n != 0 {
            agg.absorb(https(&format!("/usr/bin/c{n:04}"), 2));
        }
    }
    for n in 0..CAP_A {
        agg.absorb(bare(&format!("/opt/a{n:04}"), 1));
    }
    // c0000 now gains a network flag, but A is full: it stays in C, flagged.
    assert_eq!(agg.absorb(bare("/usr/bin/c0000", 3)), Absorbed::Recorded);
    assert_eq!(agg.class_of(&path_key("/usr/bin/c0000")), Some(Class::C));
    assert!(agg.c[&path_key("/usr/bin/c0000")].network_flagged());
    assert_eq!(agg.a.len(), CAP_A);
    // A newcomer evicts someone else: the flagged one goes last.
    agg.absorb(https("/usr/bin/newcomer", 4));
    assert!(
        agg.c.contains_key(&path_key("/usr/bin/c0000")),
        "evicted last"
    );
    assert!(!agg.c.contains_key(&path_key("/usr/bin/c0001")));
    // The same holds for a flagged directory member staying in B.
    let mut agg = Aggregate::default();
    for n in 0..CAP_A {
        agg.absorb(bare(&format!("/opt/a{n:04}"), 1));
    }
    agg.absorb(https("/tmp/old/x", 1));
    agg.absorb(bare("/tmp/old/x", 2));
    assert_eq!(agg.class_of(&path_key("/tmp/old/x")), Some(Class::B));
    for n in 0..CAP_B {
        agg.absorb(https(&format!("/tmp/d{n:04}/x"), 10 + n as i64));
    }
    assert!(
        agg.b.contains_key("/tmp/old"),
        "the flagged directory is evicted last"
    );
}

// --- the ring ------------------------------------------------------------------

#[test]
fn the_ring_keeps_the_last_fifty_dropped_keys() {
    let mut agg = Aggregate::default();
    for n in 0..(CAP_D + 70) {
        agg.absorb(named(&format!("comm{n:04}"), n as i64 + 1));
    }
    assert_eq!(agg.evicted, 70);
    assert_eq!(agg.ring.len(), RING_LEN);
    assert_eq!(agg.ring.front().unwrap().display, "comm0020");
    assert_eq!(agg.ring.back().unwrap().display, "comm0069");
    assert_eq!(agg.dropped(), 70);
}

// --- merging -------------------------------------------------------------------

#[test]
fn merging_sums_counts_and_keeps_the_earliest_and_latest() {
    let left = agg_with([
        https("/usr/bin/curl", 10),
        https("/usr/bin/curl", 20),
        https("/tmp/x/a", 15),
    ]);
    let right = agg_with([
        Program::observed(
            &row(
                "curl",
                Some("/usr/bin/curl"),
                "other.example",
                "1.1.1.1",
                443,
            ),
            5,
        ),
        https("/tmp/x/b", 30),
        named("daemon", 7),
    ]);
    let merged = left.merge(right);
    let curl = &merged.c[&path_key("/usr/bin/curl")];
    assert_eq!(curl.connections, 3);
    assert_eq!((curl.first_seen_ms, curl.last_seen_ms), (5, 20));
    assert_eq!(curl.destinations["example.com"], 2);
    assert_eq!(curl.destinations["other.example"], 1);
    assert_eq!(merged.b["/tmp/x"].programs.len(), 2);
    assert_eq!(merged.b["/tmp/x"].last_seen_ms, 30);
    assert_eq!(merged.d[&name_key("daemon")].connections, 1);
    assert_eq!(merged.programs_seen(), 4);
}

#[test]
fn merging_unions_the_capped_sets_and_applies_the_caps_again() {
    let mut left = Program::observed(
        &row("p", Some("/usr/bin/p"), "h0.example", "1.1.1.1", 1000),
        1,
    );
    let mut right = left.clone();
    for n in 0..60 {
        left.destinations.insert(format!("l{n:02}.example"), 1);
        right.destinations.insert(format!("r{n:02}.example"), 2);
        left.ports.insert(2000 + n);
        right.ports.insert(3000 + n);
    }
    let merged = agg_with([left]).merge(agg_with([right]));
    let p = &merged.a[&path_key("/usr/bin/p")];
    assert_eq!(p.destinations.len(), MAX_DESTINATIONS);
    assert!(p.dest_overflow > 0, "the rest are only counted");
    assert!(p.ports.len() <= MAX_PORTS);
    // Hosts with more connections are the ones kept.
    assert!(p.destinations.contains_key("r00.example"));
    assert!(!p.destinations.contains_key("l59.example"));
}

#[test]
fn merging_applies_class_a_s_cap_and_sums_the_counters() {
    let full = |tag: &str| {
        let mut agg = Aggregate::default();
        for n in 0..CAP_A {
            agg.absorb(bare(&format!("/opt/{tag}{n:04}"), 1));
        }
        agg
    };
    let mut left = full("l");
    left.refused = 2;
    left.incomplete = true;
    let mut right = full("r");
    right.refused = 3;
    let merged = left.merge(right);
    assert_eq!(merged.a.len(), CAP_A, "the cap holds after a merge");
    assert!(merged.refused >= 5 + CAP_A as u64, "{}", merged.refused);
    assert!(merged.incomplete);
    assert_eq!(merged.ring.len(), RING_LEN);
}

#[test]
fn a_hostile_program_read_from_a_file_is_cut_down_when_absorbed() {
    let mut hostile = https("/usr/bin/x", 1);
    hostile.raw_name = "n".repeat(10_000);
    hostile.raw_path = Some(format!("/usr/{}", "p".repeat(10_000)));
    hostile.display = "<script>".into();
    for n in 0..1000 {
        hostile
            .destinations
            .insert(format!("{n}{}", "h".repeat(1000)), 1);
        hostile.ports.insert(n as u16);
    }
    let mut agg = Aggregate::default();
    agg.absorb(hostile);
    let p = agg.c.values().next().unwrap();
    assert!(p.raw_path.as_ref().unwrap().len() <= 4096);
    assert!(p.raw_name.len() <= 256);
    assert_eq!(p.destinations.len(), MAX_DESTINATIONS);
    assert!(p.ports.len() <= MAX_PORTS);
    assert!(!p.display.contains('<') || p.display.len() <= 80 * 4);
    assert_ne!(
        p.display, "<script>",
        "the display is recomputed, never trusted"
    );
}

#[test]
fn the_summary_survives_json() {
    let mut agg = agg_with([
        https("/usr/bin/curl", 1),
        https("/tmp/x/a", 2),
        bare("/opt/z", 3),
        named("daemon", 4),
    ]);
    agg.incomplete = true;
    agg.created_ms = 1;
    let text = serde_json::to_string(&agg).unwrap();
    let back: Aggregate = serde_json::from_str(&text).unwrap();
    assert_eq!(back, agg);
    assert_eq!(Aggregate::default().merge(back), agg);
}

#[test]
fn counters_report_what_the_banner_shows() {
    let mut agg = Aggregate::default();
    assert!(agg.is_empty());
    agg.absorb(https("/usr/bin/curl", 1));
    agg.absorb(https("/tmp/x/a", 2));
    agg.absorb(https("/tmp/x/b", 2));
    agg.absorb(named("d", 3));
    assert!(!agg.is_empty());
    assert_eq!(agg.programs_seen(), 4, "directory members count one each");
    assert_eq!(agg.dropped(), 0);
    assert_eq!(
        agg.created_ms, 1,
        "the file's age starts at the first thing seen"
    );
}

// --- bytes, not just counts ---------------------------------------------------

/// A program at the limit of every field: the most bytes one can take.
fn maximal(tag: &str, n: usize, dir: &str, flagged: bool) -> Program {
    let mut p = Program::observed(
        &row(
            &"n".repeat(MAX_NAME_BYTES),
            Some(&format!("{dir}{tag}{n:05}{}", "p".repeat(2_000))),
            "x.example",
            "1.1.1.1",
            443,
        ),
        n as i64 + 1,
    );
    p.destinations.clear();
    for d in 0..MAX_DESTINATIONS + 10 {
        p.destinations
            .insert(format!("{d:03}{}", "h".repeat(400)), 1 + d as u64);
    }
    for port in 0..64 {
        p.ports.insert(1000 + port);
    }
    p.protocols = (0..8)
        .map(|i| format!("proto{i}{}", "x".repeat(40)))
        .collect();
    if flagged {
        p.uncommon_port = 1;
    }
    p.normalized()
}

#[test]
fn every_field_has_a_byte_limit() {
    let p = maximal("a", 1, "/tmp/", false);
    assert!(p.raw_path.as_ref().unwrap().len() <= 1024);
    assert!(
        p.truncated,
        "a cut path is marked, so no exact rule is built from it"
    );
    assert!(p.raw_name.len() <= MAX_NAME_BYTES);
    assert!(p.display.len() <= MAX_DISPLAY_BYTES);
    assert_eq!(p.destinations.len(), MAX_DESTINATIONS);
    assert!(p.destinations.keys().all(|d| d.len() <= MAX_DEST_BYTES));
    assert!(p.ports.len() <= MAX_PORTS && p.protocols.len() <= MAX_PROTOCOLS);
    assert!(
        p.est_bytes() <= MAX_PROGRAM_BYTES,
        "{} > {MAX_PROGRAM_BYTES}",
        p.est_bytes()
    );
    // The estimate really is an upper bound on what JSON writes.
    assert!(serde_json::to_string(&p).unwrap().len() <= p.est_bytes());
}

#[test]
fn the_estimate_bounds_the_json_even_for_quotes_and_backslashes() {
    let mut p = Program::observed(
        &row(
            "q\"\\",
            Some(&format!("/usr/bin/\"{}\\", "\"".repeat(100))),
            "a.example",
            "1.1.1.1",
            443,
        ),
        1,
    );
    p.destinations.insert("\"\\\"".repeat(80), 1);
    let p = p.normalized();
    assert!(serde_json::to_string(&p).unwrap().len() <= p.est_bytes());
}

#[test]
fn the_byte_budget_holds_whatever_is_thrown_at_it() {
    let mut agg = Aggregate::default();
    // Round robin over the classes, so each one is populated when the
    // budget runs out.
    for n in 0..700 {
        agg.absorb(maximal("a", n, "/opt/", true));
        agg.absorb(maximal("b", n, &format!("/tmp/d{}/", n % 400), false));
        agg.absorb(maximal("c", n, "/usr/bin/", false));
        let mut d = maximal("d", n, "/x/", false);
        d.raw_path = None;
        d.raw_name = format!("{n}{}", "d".repeat(200));
        agg.absorb(d);
        assert!(agg.bytes <= BYTE_BUDGET, "round {n}: {}", agg.bytes);
        assert_eq!(agg.bytes, agg.recount(), "round {n}");
        if n == 20 {
            assert!(
                !agg.a.is_empty() && !agg.b.is_empty() && !agg.c.is_empty() && !agg.d.is_empty(),
                "every class is populated before the budget runs out"
            );
        }
    }
    assert!(agg.programs_seen() > 100);
    assert!(agg.dropped() > 0, "something had to be refused or evicted");
    let json = serde_json::to_vec(&agg).unwrap();
    assert!(
        json.len() <= BYTE_BUDGET + FILE_OVERHEAD,
        "{} bytes",
        json.len()
    );
}

#[test]
fn with_no_room_a_known_program_only_counts_and_a_new_one_is_refused() {
    let mut agg = Aggregate::default();
    let mut n = 0;
    while agg.bytes + 2 * MAX_PROGRAM_BYTES + MAX_DIR_BYTES < BYTE_BUDGET {
        agg.absorb(maximal("a", n, "/opt/", true));
        n += 1;
    }
    // Fill the rest, all flagged (class A is never evicted).
    while agg.refused == 0 {
        agg.absorb(maximal("a", n, "/opt/", true));
        n += 1;
    }
    // Small flagged programs until even those no longer fit.
    let mut fill = 0;
    while agg.absorb(bare(&format!("/opt/fill{fill}"), 1)) == Absorbed::Recorded {
        fill += 1;
    }
    let before = agg.bytes;
    // A new program: refused.
    assert_eq!(
        agg.absorb(maximal("z", 1, "/opt/", true)),
        Absorbed::Refused
    );
    // A known one with new detail: counted, no new detail kept.
    let known = agg.a.values().next().unwrap().clone();
    let mut again = known.clone();
    again.destinations.clear();
    again.destinations.insert("brand-new.example".into(), 5);
    again.ports = BTreeSet::from([9]);
    again.connections = 7;
    again.uncommon_port = 7;
    assert_eq!(agg.absorb(again), Absorbed::Recorded);
    let updated = &agg.a[known.key()];
    assert_eq!(updated.connections, known.connections + 7);
    assert!(!updated.destinations.contains_key("brand-new.example"));
    assert!(!updated.ports.contains(&9));
    assert!(updated.dest_overflow >= known.dest_overflow + 5);
    assert_eq!(agg.bytes, before);
    assert_eq!(agg.bytes, agg.recount());
}

#[test]
fn the_byte_count_follows_every_kind_of_change() {
    let mut agg = agg_with([
        https("/usr/bin/curl", 1),
        https("/usr/bin/curl", 2),
        https("/tmp/x/a", 1),
        https("/tmp/x/b", 1),
        named("daemon", 1),
    ]);
    assert_eq!(agg.bytes, agg.recount());
    agg.absorb(bare("/tmp/x/a", 3)); // B -> A
    assert_eq!(agg.bytes, agg.recount());
    agg.absorb(bare("/tmp/x/b", 3)); // the directory goes away
    assert!(agg.b.is_empty());
    assert_eq!(agg.bytes, agg.recount());
    for n in 0..(CAP_D + 5) {
        agg.absorb(named(&format!("c{n}"), n as i64));
    }
    assert_eq!(agg.bytes, agg.recount());
    let merged = agg.clone().merge(agg.clone());
    assert_eq!(merged.bytes, merged.recount());
    assert_eq!(
        Aggregate::default().merge(merged.clone()).bytes,
        merged.bytes
    );
}

// --- keys and file input ----------------------------------------------------------

#[test]
fn a_process_named_like_a_path_does_not_share_an_entry_with_that_path() {
    let mut agg = Aggregate::default();
    agg.absorb(Program::observed(
        &row("/usr/bin/curl", None, "a.example", "1.1.1.1", 443),
        1,
    ));
    agg.absorb(https("/usr/bin/curl", 2));
    assert_eq!(agg.d.len(), 1, "the name");
    assert_eq!(agg.c.len(), 1, "the path");
    assert!(agg.d.contains_key(&name_key("/usr/bin/curl")));
    assert!(agg.c.contains_key(&path_key("/usr/bin/curl")));
    assert_eq!(agg.programs_seen(), 2);
}

#[test]
fn a_hostile_drop_ring_entry_is_cut_and_made_safe_when_merged() {
    let hostile = Dropped {
        display: format!("<b>\u{202e}evil\n{}", "x".repeat(10_000)),
        kind: DropKind::Evicted,
        class: Class::C,
        connections: 3,
        flagged: false,
    };
    let from_file = Aggregate {
        ring: VecDeque::from([hostile]),
        ..Aggregate::default()
    };
    let merged = Aggregate::default().merge(from_file);
    let shown = &merged.ring[0].display;
    assert!(shown.len() <= MAX_DISPLAY_BYTES, "{}", shown.len());
    assert!(!shown.contains('\u{202e}') && !shown.contains('\n'));
    assert!(!shown.chars().any(char::is_control));
}

#[test]
fn a_full_budget_makes_room_by_evicting_unflagged_entries_never_class_a() {
    let mut agg = Aggregate::default();
    let mut n = 0;
    while agg.bytes + 2 * (MAX_PROGRAM_BYTES + MAX_DIR_BYTES) < BYTE_BUDGET {
        agg.absorb(maximal("c", n, "/usr/bin/", false));
        n += 1;
    }
    agg.absorb(maximal("a", 0, "/opt/", true));
    let a_before = agg.a.clone();
    assert_eq!(agg.evicted, 0);
    // Flagged newcomers keep coming: unflagged paths make way for them.
    for i in 1..20 {
        assert_eq!(
            agg.absorb(maximal("a", i, "/opt/", true)),
            Absorbed::Recorded
        );
    }
    assert!(agg.evicted > 0, "unflagged entries made room");
    assert_eq!(agg.a.len(), 20);
    assert!(
        a_before.iter().all(|(k, v)| agg.a.get(k) == Some(v)),
        "class A untouched"
    );
    assert!(agg.bytes <= BYTE_BUDGET);
    assert_eq!(agg.bytes, agg.recount());
}
