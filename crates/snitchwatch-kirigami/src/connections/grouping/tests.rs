use super::*;

pub(super) fn row(
    id: &str,
    process: &str,
    host: &str,
    ip: &str,
    action: Option<&str>,
) -> ConnectionRow {
    ConnectionRow {
        id: id.to_string(),
        process: process.to_string(),
        process_path: None,
        dst_host: host.to_string(),
        dst_ip: ip.to_string(),
        dst_port: 443,
        protocol: "tcp".to_string(),
        direction: "outgoing".to_string(),
        action: action.map(|s| s.to_string()),
        bytes_sent: 10,
        bytes_received: 20,
        started_at_ms: 0,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    }
}

// --- registrable_domain heuristic ------------------------------------

#[test]
fn registrable_domain_strips_subdomains() {
    assert_eq!(registrable_domain("github.com"), "github.com");
    assert_eq!(registrable_domain("www.github.com"), "github.com");
    assert_eq!(registrable_domain("api.foo.bar.github.com"), "github.com");
}

#[test]
fn registrable_domain_handles_two_part_tlds() {
    assert_eq!(registrable_domain("example.co.uk"), "example.co.uk");
    assert_eq!(registrable_domain("www.example.co.uk"), "example.co.uk");
    assert_eq!(registrable_domain("sub.www.example.co.uk"), "example.co.uk");
    assert_eq!(registrable_domain("shop.example.com.au"), "example.com.au");
}

#[test]
fn registrable_domain_single_label_is_returned_as_is() {
    assert_eq!(registrable_domain("localhost"), "localhost");
    assert_eq!(registrable_domain(""), "");
}

#[test]
fn registrable_domain_is_case_insensitive() {
    assert_eq!(registrable_domain("WWW.GitHub.COM"), "github.com");
}

// --- tree membership/aggregates --------------------------------------

/// Like [`row`] but with an explicit executable path, for exercising
/// `process_key`'s exe-path branch (the plain `row` helper always leaves
/// `process_path: None`, so it only ever exercises the process-name
/// fallback).
pub(super) fn row_with_process_path(
    id: &str,
    process: &str,
    process_path: &str,
    host: &str,
    ip: &str,
    action: Option<&str>,
) -> ConnectionRow {
    ConnectionRow {
        process_path: Some(process_path.to_string()),
        ..row(id, process, host, ip, action)
    }
}

pub(super) fn build(rows: &[ConnectionRow]) -> GroupTree {
    let mut t = GroupTree::new();
    for r in rows {
        t.upsert_row(r);
    }
    t
}

pub(super) fn rows_map(rows: &[ConnectionRow]) -> HashMap<&str, &ConnectionRow> {
    rows.iter().map(|r| (r.id.as_str(), r)).collect()
}

#[test]
fn insert_groups_by_process_then_domain() {
    let rows = vec![
        row("a", "firefox", "www.github.com", "1.1.1.1", Some("allow")),
        row("b", "firefox", "api.github.com", "1.1.1.2", None),
        row("c", "firefox", "slack.com", "2.2.2.2", Some("deny")),
        row("d", "slack", "slack.com", "2.2.2.2", Some("allow")),
    ];
    let mut tree = build(&rows);
    assert_eq!(tree.process_count(), 2);
    // Pending rows no longer force expansion; open firefox explicitly so
    // its domain headers are emitted for the grouping assertions below.
    tree.toggle_process("firefox");

    let no_filter = ConnectionFilter::default();
    let map = rows_map(&rows);
    let proj = tree.build_projection(&map, &no_filter);

    match &proj[0] {
        VisibleEntry::ProcessHeader {
            key,
            counts,
            expanded,
            ..
        } => {
            assert_eq!(key, "firefox");
            assert_eq!(counts.total, 3);
            assert_eq!(counts.pending, 1);
            assert!(expanded);
        }
        other => panic!("expected firefox ProcessHeader, got {other:?}"),
    }
    // github.com groups a+b under one domain header.
    let github_header = proj
        .iter()
        .find(|e| matches!(e, VisibleEntry::DomainHeader { key, .. } if key == "github.com"));
    match github_header {
        Some(VisibleEntry::DomainHeader { counts, .. }) => assert_eq!(counts.total, 2),
        other => panic!("expected github.com DomainHeader, got {other:?}"),
    }
}

#[test]
fn bare_ip_hosts_group_under_the_ip() {
    let rows = vec![
        row("a", "curl", "", "10.0.0.5", Some("allow")),
        row("b", "curl", "10.0.0.5", "10.0.0.5", Some("allow")),
    ];
    let mut tree = build(&rows);
    tree.toggle_process("curl"); // both rows decided -> expand to inspect
    let filter = ConnectionFilter::default();
    let map = rows_map(&rows);
    let proj = tree.build_projection(&map, &filter);
    let domain_headers: Vec<&String> = proj
        .iter()
        .filter_map(|e| match e {
            VisibleEntry::DomainHeader { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    assert_eq!(domain_headers, vec![&"10.0.0.5".to_string()]);
}

#[test]
fn update_in_place_moves_counts_not_membership() {
    let mut rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
    let mut tree = build(&rows);
    {
        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &filter);
        // Pending is signalled via the header counts (the badge source).
        match &proj[0] {
            VisibleEntry::ProcessHeader { counts, .. } => assert_eq!(counts.pending, 1),
            other => panic!("unexpected {other:?}"),
        }
    }

    rows[0].action = Some("allow".to_string());
    tree.upsert_row(&rows[0]);
    let filter = ConnectionFilter::default();
    let map = rows_map(&rows);
    let proj = tree.build_projection(&map, &filter);
    match &proj[0] {
        VisibleEntry::ProcessHeader {
            counts, expanded, ..
        } => {
            assert_eq!(counts.pending, 0);
            assert_eq!(counts.allowed, 1);
            // No longer forced open (not pending, no filter, not toggled).
            assert!(!expanded);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn update_that_changes_host_moves_row_to_new_domain_group() {
    let mut rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
    let mut tree = build(&rows);
    rows[0].dst_host = "slack.com".to_string();
    rows[0].dst_ip = "2.2.2.2".to_string();
    tree.upsert_row(&rows[0]);
    // After the move (which prunes toggle state for the emptied group),
    // expand the process so its domain headers are emitted for the assert.
    tree.toggle_process("firefox");

    let filter = ConnectionFilter::default();
    let map = rows_map(&rows);
    let proj = tree.build_projection(&map, &filter);
    let domain_keys: Vec<&String> = proj
        .iter()
        .filter_map(|e| match e {
            VisibleEntry::DomainHeader { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    assert_eq!(domain_keys, vec![&"slack.com".to_string()]);
}

#[test]
fn remove_prunes_empty_domain_and_process_groups() {
    let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
    let mut tree = build(&rows);
    assert_eq!(tree.process_count(), 1);
    tree.remove_row("a");
    assert_eq!(tree.process_count(), 0);

    let filter = ConnectionFilter::default();
    let map: HashMap<&str, &ConnectionRow> = HashMap::new();
    assert!(tree.build_projection(&map, &filter).is_empty());
}

#[test]
fn remove_of_one_row_keeps_sibling_domain_group() {
    let rows = vec![
        row("a", "firefox", "github.com", "1.1.1.1", Some("allow")),
        row("b", "firefox", "slack.com", "2.2.2.2", Some("allow")),
    ];
    let mut tree = build(&rows);
    tree.remove_row("a");
    assert_eq!(tree.process_count(), 1);
    tree.toggle_process("firefox"); // decided row -> expand to inspect
    let filter = ConnectionFilter::default();
    let remaining = vec![rows[1].clone()];
    let map = rows_map(&remaining);
    let proj = tree.build_projection(&map, &filter);
    let domain_keys: Vec<&String> = proj
        .iter()
        .filter_map(|e| match e {
            VisibleEntry::DomainHeader { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    assert_eq!(domain_keys, vec![&"slack.com".to_string()]);
}

#[test]
fn clear_empties_the_tree() {
    let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
    let mut tree = build(&rows);
    tree.clear();
    assert_eq!(tree.process_count(), 0);
}

#[test]
fn expand_collapse_toggles_control_non_pending_groups() {
    let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
    let tree_default = build(&rows);
    let filter = ConnectionFilter::default();
    let map = rows_map(&rows);
    // Decided row, not filtered, not toggled -> collapsed by default.
    let proj = tree_default.build_projection(&map, &filter);
    assert_eq!(proj.len(), 1, "process header only, domain+row collapsed");

    let mut tree = build(&rows);
    tree.toggle_process("firefox");
    let proj = tree.build_projection(&map, &filter);
    assert!(
        proj.len() > 1,
        "expanding the process should reveal its domain"
    );

    tree.toggle_domain("firefox", "github.com");
    let proj = tree.build_projection(&map, &filter);
    assert_eq!(proj.len(), 3, "process header + domain header + leaf row");
    match &proj[2] {
        VisibleEntry::Row { id, .. } => assert_eq!(id, "a"),
        other => panic!("expected leaf Row, got {other:?}"),
    }
}

#[test]
fn pending_row_stays_collapsed_and_surfaces_through_header_counts() {
    let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
    let mut tree = build(&rows); // never toggled expanded
    let filter = ConnectionFilter::default();
    let map = rows_map(&rows);

    let proj = tree.build_projection(&map, &filter);
    assert_eq!(proj.len(), 1, "collapse state wins over pending rows");
    match &proj[0] {
        VisibleEntry::ProcessHeader {
            expanded, counts, ..
        } => {
            assert!(!expanded);
            assert_eq!(counts.pending, 1, "badge count carries the signal");
        }
        other => panic!("expected ProcessHeader, got {other:?}"),
    }

    // Manual expansion still reveals the pending path in full.
    tree.toggle_process("firefox");
    tree.toggle_domain("firefox", "github.com");
    let proj = tree.build_projection(&map, &filter);
    assert_eq!(proj.len(), 3, "process + domain headers + pending leaf");
}

#[test]
fn filter_hides_groups_with_no_matching_descendants_but_shows_matching_ancestors() {
    let rows = vec![
        row("a", "firefox", "github.com", "1.1.1.1", Some("allow")),
        row("b", "slack", "slack.com", "2.2.2.2", Some("allow")),
    ];
    let tree = build(&rows);
    let filter = ConnectionFilter::new("github", false);
    let map = rows_map(&rows);
    let proj = tree.build_projection(&map, &filter);

    // Only the firefox/github.com path survives, and it is forced open
    // by the active filter so the match is actually visible.
    assert_eq!(proj.len(), 3);
    match &proj[0] {
        VisibleEntry::ProcessHeader { key, expanded, .. } => {
            assert_eq!(key, "firefox");
            assert!(expanded);
        }
        other => panic!("unexpected {other:?}"),
    }
    match &proj[2] {
        VisibleEntry::Row { id, .. } => assert_eq!(id, "a"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn move_to_front_reorders_domain_and_process_groups() {
    let rows = vec![
        row("a", "firefox", "github.com", "1.1.1.1", Some("allow")),
        row("b", "slack", "slack.com", "2.2.2.2", Some("allow")),
    ];
    let mut tree = build(&rows);
    // slack was inserted second; move it to front.
    tree.move_to_front(&["b".to_string()]);
    assert_eq!(
        tree.process_order.first().map(String::as_str),
        Some("slack")
    );
}

#[test]
fn move_to_front_of_unknown_id_is_a_no_op() {
    let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
    let mut tree = build(&rows);
    tree.move_to_front(&["nope".to_string()]);
    assert_eq!(tree.process_count(), 1);
}

// --- pending_row_ids (issue #18 batch actions) ------------------------

#[test]
fn pending_row_ids_collects_only_pending_rows_under_a_process_in_display_order() {
    // Insertion order deliberately differs from lexical id/domain order
    // (slack.com before github.com; "c" before "a") so a regression that
    // sorts the result rather than preserving group-then-insertion order
    // would go undetected by a lexically-sorted assertion.
    let rows = vec![
        row("c", "firefox", "slack.com", "2.2.2.2", None),
        row("a", "firefox", "github.com", "1.1.1.1", None),
        row("b", "firefox", "github.com", "1.1.1.1", Some("allow")),
        row("d", "chrome", "slack.com", "2.2.2.2", None),
    ];
    let tree = build(&rows);
    assert_eq!(
        tree.pending_row_ids("firefox"),
        vec!["c".to_string(), "a".to_string()],
        "display order: slack.com (inserted first) then github.com, not lexical"
    );
    assert_eq!(tree.pending_row_ids("chrome"), vec!["d".to_string()]);
}

#[test]
fn pending_row_ids_reflects_move_to_front_reordering() {
    // Two pending rows in the same domain group, inserted "z" then "m"
    // (lexically reversed from insertion order) so the base order is
    // insertion order, not sorted — then move_to_front flips it.
    let rows = vec![
        row("z", "firefox", "github.com", "1.1.1.1", None),
        row("m", "firefox", "github.com", "1.1.1.1", None),
    ];
    let mut tree = build(&rows);
    assert_eq!(
        tree.pending_row_ids("firefox"),
        vec!["z".to_string(), "m".to_string()],
        "insertion order before any move"
    );
    tree.move_to_front(&["m".to_string()]);
    assert_eq!(
        tree.pending_row_ids("firefox"),
        vec!["m".to_string(), "z".to_string()],
        "move_to_front must be reflected in the batch-action id order"
    );
}

#[test]
fn pending_row_ids_uses_executable_path_as_the_process_key_when_present() {
    let rows = vec![row_with_process_path(
        "a",
        "curl",
        "/usr/bin/curl",
        "github.com",
        "1.1.1.1",
        None,
    )];
    let tree = build(&rows);
    // The bare process name is not a valid key once an exe path is set
    // (see `process_key`'s doc comment: exe path disambiguates identical
    // process names at different install locations).
    assert!(tree.pending_row_ids("curl").is_empty());
    assert_eq!(tree.pending_row_ids("/usr/bin/curl"), vec!["a".to_string()]);
}

#[test]
fn remove_row_decrements_pending_counts() {
    // Regression: remove_row used to evict `id_meta` before
    // `remove_from_group` read it back to know which counts to
    // subtract, so `counts.pending` (the batch-button/badge source of
    // truth) never actually decremented on removal.
    let rows = vec![
        row("a", "firefox", "github.com", "1.1.1.1", None),
        row("b", "firefox", "github.com", "1.1.1.1", None),
    ];
    let mut tree = build(&rows);
    let filter = ConnectionFilter::default();
    let map = rows_map(&rows);
    match &tree.build_projection(&map, &filter)[0] {
        VisibleEntry::ProcessHeader { counts, .. } => assert_eq!(counts.pending, 2),
        other => panic!("unexpected {other:?}"),
    }

    tree.remove_row("a");
    assert_eq!(
        tree.pending_row_ids("firefox").len(),
        1,
        "pending_row_ids must drop the removed row"
    );
    let remaining = vec![rows[1].clone()];
    let remaining_map = rows_map(&remaining);
    match &tree.build_projection(&remaining_map, &filter)[0] {
        VisibleEntry::ProcessHeader { counts, .. } => {
            assert_eq!(
                counts.pending, 1,
                "counts.pending must decrement on removal"
            )
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn pending_row_ids_len_matches_group_counts_pending_after_mixed_mutations() {
    // Invariant test: pending_row_ids(k).len() must always agree with
    // the aggregate counts.pending for the same group, across a mixed
    // insert/decide/remove sequence — the two are read by different
    // consumers (batch-action buttons vs. the header badge) and must
    // never disagree.
    let mut rows = vec![
        row("a", "firefox", "github.com", "1.1.1.1", None),
        row("b", "firefox", "github.com", "1.1.1.1", None),
        row("c", "firefox", "slack.com", "2.2.2.2", None),
    ];
    let mut tree = build(&rows);
    let assert_invariant = |tree: &GroupTree, rows: &[ConnectionRow]| {
        let filter = ConnectionFilter::default();
        let map = rows_map(rows);
        let proj = tree.build_projection(&map, &filter);
        let counts_pending = match proj.first() {
            Some(VisibleEntry::ProcessHeader { counts, .. }) => counts.pending,
            _ => 0,
        };
        assert_eq!(tree.pending_row_ids("firefox").len(), counts_pending);
    };
    assert_invariant(&tree, &rows);

    // Decide "a".
    rows[0].action = Some("allow".to_string());
    tree.upsert_row(&rows[0]);
    assert_invariant(&tree, &rows);

    // Remove "b" (still pending).
    tree.remove_row("b");
    rows.retain(|r| r.id != "b");
    assert_invariant(&tree, &rows);

    // "c" remains pending; the invariant must still hold.
    assert_eq!(tree.pending_row_ids("firefox"), vec!["c".to_string()]);
}

#[test]
fn pending_row_ids_compose_into_valid_batch_deny_messages() {
    // Pins what `submitBatchVerdict` sends per id for "Deny all":
    // "deny"/"this_host" plus that row's own inline duration (the same
    // `inlineDurationFor` lookup a row's Deny uses) through to a
    // well-formed `SetVerdict`, entirely Qt-free. On a bridge with
    // app-bound rules, a program file it can bind to is denied until
    // restart; no path, once.
    let rows = vec![
        row_with_process_path(
            "a",
            "firefox",
            "/usr/bin/firefox",
            "github.com",
            "1.1.1.1",
            None,
        ),
        row_with_process_path(
            "b",
            "firefox",
            "/usr/bin/firefox",
            "slack.com",
            "2.2.2.2",
            None,
        ),
        row("c", "curl", "github.com", "1.1.1.1", None),
    ];
    let tree = build(&rows);
    let mut store = crate::connections::row_store::RowStore::new();
    store.insert_rows(rows);
    for (process_key, duration) in [("/usr/bin/firefox", "until_restart"), ("curl", "once")] {
        let ids = tree.pending_row_ids(process_key);
        assert!(!ids.is_empty(), "{process_key}");
        for id in ids {
            let token = crate::inline_deny::duration_token_for_row(
                &store,
                &id,
                crate::pending_decision::VerdictChoice::Deny,
                true,
                "forever",
            );
            let msg =
                crate::pending_decision::build_verdict_message(&id, "deny", "this_host", token)
                    .expect("well-formed choice token must build");
            let json = serde_json::to_value(&msg).unwrap();
            assert_eq!(json["rowId"], id);
            assert_eq!(json["verdict"], "deny");
            assert_eq!(json["scope"], "this_host");
            assert_eq!(json["duration"], duration, "{id}");
        }
    }
}

#[test]
fn pending_row_ids_is_empty_for_unknown_or_fully_decided_process() {
    let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
    let tree = build(&rows);
    assert!(tree.pending_row_ids("firefox").is_empty());
    assert!(tree.pending_row_ids("nonexistent").is_empty());
}

#[test]
fn pending_row_ids_is_independent_of_collapse_state() {
    let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
    let tree = build(&rows); // never expanded
    assert_eq!(tree.pending_row_ids("firefox"), vec!["a".to_string()]);
}
