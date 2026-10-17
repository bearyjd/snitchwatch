use super::tests::*;
use super::*;

// --- host level --------------------------------------------------------

fn host_rows() -> Vec<ConnectionRow> {
    vec![
        row("a", "firefox", "www.github.com", "1.1.1.1", Some("allow")),
        row("b", "firefox", "api.github.com", "1.1.1.2", None),
        row("c", "firefox", "www.github.com", "1.1.1.1", Some("deny")),
        row("d", "firefox", "slack.com", "2.2.2.2", Some("allow")),
    ]
}

fn open_all(tree: &mut GroupTree) {
    tree.set_process_expanded("firefox", true);
    tree.set_domain_expanded("firefox", "github.com", true);
    tree.set_host_expanded("firefox", "github.com", "www.github.com", true);
    tree.set_host_expanded("firefox", "github.com", "api.github.com", true);
}

fn shape(proj: &[VisibleEntry]) -> Vec<String> {
    proj.iter()
        .map(|e| match e {
            VisibleEntry::ProcessHeader { key, .. } => format!("P:{key}"),
            VisibleEntry::DomainHeader { key, .. } => format!("D:{key}"),
            VisibleEntry::HostHeader { key, .. } => format!("H:{key}"),
            VisibleEntry::Row { id, host_key, .. } => {
                format!("R:{id}:{}", host_key.is_some())
            }
        })
        .collect()
}

#[test]
fn host_projection_nests_hosts_between_domain_and_connection() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    assert_eq!(
        shape(&proj),
        vec![
            "P:firefox",
            "D:github.com",
            "H:www.github.com",
            "R:a:true",
            "R:c:true",
            "H:api.github.com",
            "R:b:true",
            "D:slack.com",
        ]
    );
    match &proj[2] {
        VisibleEntry::HostHeader {
            counts, expanded, ..
        } => {
            assert_eq!((counts.total, counts.allowed, counts.denied), (2, 1, 1));
            assert!(expanded);
        }
        other => panic!("expected host header, got {other:?}"),
    }
}

#[test]
fn entry_levels_run_program_domain_host_connection() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    let levels: Vec<i32> = proj.iter().map(VisibleEntry::level).collect();
    assert_eq!(levels, vec![0, 1, 2, 3, 3, 2, 3, 1]);
}

#[test]
fn hosts_are_materialised_only_under_an_expanded_domain() {
    let rows = host_rows();
    let mut tree = build(&rows);
    tree.set_process_expanded("firefox", true);
    // A host flagged open under a collapsed domain stays hidden.
    tree.set_host_expanded("firefox", "github.com", "www.github.com", true);
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    assert_eq!(
        shape(&proj),
        vec!["P:firefox", "D:github.com", "D:slack.com"]
    );
}

#[test]
fn domain_and_program_counts_are_the_same_with_or_without_hosts() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    let map = rows_map(&rows);
    let filter = ConnectionFilter::default();
    let headers = |proj: Vec<VisibleEntry>| -> Vec<(String, GroupCounts)> {
        proj.into_iter()
            .filter_map(|e| match e {
                VisibleEntry::ProcessHeader { key, counts, .. }
                | VisibleEntry::DomainHeader { key, counts, .. } => Some((key, counts)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        headers(tree.build_projection(&map, &filter)),
        headers(tree.build_host_projection(&map, &filter))
    );
}

#[test]
fn host_counts_follow_incremental_updates() {
    let mut rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    // The pending row on api.github.com gets decided.
    rows[1].action = Some("deny".to_string());
    tree.upsert_row(&rows[1]);
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    let api = proj.iter().find_map(|e| match e {
        VisibleEntry::HostHeader { key, counts, .. } if key == "api.github.com" => Some(*counts),
        _ => None,
    });
    let api = api.expect("api.github.com host header");
    assert_eq!((api.total, api.pending, api.denied), (1, 0, 1));
}

#[test]
fn the_plain_projection_never_shows_hosts() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    let proj = tree.build_projection(&rows_map(&rows), &ConnectionFilter::default());
    assert!(proj
        .iter()
        .all(|e| !matches!(e, VisibleEntry::HostHeader { .. })));
    assert!(proj
        .iter()
        .any(|e| matches!(e, VisibleEntry::Row { host_key: None, .. })));
}

#[test]
fn a_collapsed_host_hides_its_rows_and_keeps_its_counts() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    tree.toggle_host("firefox", "github.com", "www.github.com");
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    let s = shape(&proj);
    assert!(s.contains(&"H:www.github.com".to_string()));
    assert!(!s.contains(&"R:a:true".to_string()));
    assert!(s.contains(&"R:b:true".to_string()));
}

#[test]
fn pending_host_rows_are_signalled_not_forced_open() {
    let rows = host_rows();
    let mut tree = build(&rows);
    tree.set_process_expanded("firefox", true);
    tree.set_domain_expanded("firefox", "github.com", true);
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    // api.github.com holds the pending row b but stays collapsed.
    let api = proj
        .iter()
        .find(|e| matches!(e, VisibleEntry::HostHeader { key, .. } if key == "api.github.com"));
    match api {
        Some(VisibleEntry::HostHeader {
            expanded, counts, ..
        }) => {
            assert!(!expanded);
            assert_eq!(counts.pending, 1);
        }
        other => panic!("expected api.github.com host header, got {other:?}"),
    }
    assert!(!shape(&proj).iter().any(|s| s.starts_with("R:")));
}

#[test]
fn an_active_filter_forces_the_host_path_open_and_hides_other_hosts() {
    let rows = host_rows();
    let tree = build(&rows);
    let filter = ConnectionFilter::new("api.github", false);
    let proj = tree.build_host_projection(&rows_map(&rows), &filter);
    assert_eq!(
        shape(&proj),
        vec!["P:firefox", "D:github.com", "H:api.github.com", "R:b:true"]
    );
}

#[test]
fn denied_only_and_from_now_force_the_host_path_open_and_hide_the_rest() {
    let mut rows = vec![
        row("a", "firefox", "api.github.com", "1.1.1.1", Some("deny")),
        row("b", "firefox", "www.github.com", "1.1.1.1", Some("allow")),
        row("c", "firefox", "www.github.com", "1.1.1.1", Some("deny")),
    ];
    rows[0].started_at_ms = 500;
    rows[1].started_at_ms = 500;
    rows[2].started_at_ms = 50;
    let tree = build(&rows);

    let mut filter = ConnectionFilter::default();
    filter.set_denied_only(true);
    let proj = tree.build_host_projection(&rows_map(&rows), &filter);
    let s = shape(&proj);
    assert!(s.contains(&"H:api.github.com".to_string()), "{s:?}");
    assert!(s.contains(&"H:www.github.com".to_string()), "{s:?}");
    assert!(!s.iter().any(|e| e.starts_with("R:b:")), "{s:?}");

    // From now at 100 leaves only row a; www.github.com has nothing left.
    filter.set_since_ms(Some(100));
    let proj = tree.build_host_projection(&rows_map(&rows), &filter);
    assert_eq!(
        shape(&proj),
        vec!["P:firefox", "D:github.com", "H:api.github.com", "R:a:true"]
    );
}

#[test]
fn hosts_are_pruned_and_counted_down_as_rows_leave() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    tree.remove_row("b");
    tree.remove_row("a");
    let remaining: Vec<ConnectionRow> = rows
        .iter()
        .filter(|r| r.id != "a" && r.id != "b")
        .cloned()
        .collect();
    let proj = tree.build_host_projection(&rows_map(&remaining), &ConnectionFilter::default());
    let s = shape(&proj);
    assert!(!s.contains(&"H:api.github.com".to_string()), "{s:?}");
    match proj
        .iter()
        .find(|e| matches!(e, VisibleEntry::HostHeader { key, .. } if key == "www.github.com"))
    {
        Some(VisibleEntry::HostHeader { counts, .. }) => assert_eq!(counts.total, 1),
        other => panic!("expected www.github.com, got {other:?}"),
    }
}

#[test]
fn an_update_that_changes_verdict_moves_the_host_counts() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    let mut decided = rows.clone();
    decided[1].action = Some("allow".to_string());
    tree.upsert_row(&decided[1]);
    let proj = tree.build_host_projection(&rows_map(&decided), &ConnectionFilter::default());
    match proj
        .iter()
        .find(|e| matches!(e, VisibleEntry::HostHeader { key, .. } if key == "api.github.com"))
    {
        Some(VisibleEntry::HostHeader { counts, .. }) => {
            assert_eq!((counts.pending, counts.allowed), (0, 1));
        }
        other => panic!("expected api.github.com, got {other:?}"),
    }
}

#[test]
fn an_update_to_another_host_moves_the_row_between_hosts() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    let mut moved = rows.clone();
    moved[1].dst_host = "www.github.com".to_string();
    tree.upsert_row(&moved[1]);
    let proj = tree.build_host_projection(&rows_map(&moved), &ConnectionFilter::default());
    let s = shape(&proj);
    assert!(!s.contains(&"H:api.github.com".to_string()), "{s:?}");
}

#[test]
fn bare_ip_hosts_are_keyed_by_the_ip() {
    let rows = vec![row("a", "curl", "", "10.0.0.5", Some("allow"))];
    let mut tree = build(&rows);
    tree.set_process_expanded("curl", true);
    tree.set_domain_expanded("curl", "10.0.0.5", true);
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    assert_eq!(shape(&proj), vec!["P:curl", "D:10.0.0.5", "H:10.0.0.5"]);
}

#[test]
fn move_to_front_reorders_hosts_within_a_domain() {
    let rows = host_rows();
    let mut tree = build(&rows);
    open_all(&mut tree);
    tree.move_to_front(&["b".to_string()]);
    let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
    let hosts: Vec<String> = shape(&proj)
        .into_iter()
        .filter(|s| s.starts_with("H:"))
        .collect();
    assert_eq!(hosts, vec!["H:api.github.com", "H:www.github.com"]);
}
