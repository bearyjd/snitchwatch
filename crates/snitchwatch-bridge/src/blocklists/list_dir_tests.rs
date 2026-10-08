//! Tests for [`super`]: the files opensnitchd reads, their permissions, and
//! every path that must not be followed.

use super::*;
use std::os::unix::fs::{symlink, MetadataExt};

/// Rust port of opensnitchd's `filterDomains` + `readTupleList`
/// (`vendor:daemon/rule/operator_lists.go`): the keys a `lists.domains` rule
/// loads from one file.
fn daemon_domains(raw: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for line in raw.split('\n') {
        if line.len() < 9 {
            continue;
        }
        if &line[..7] != "0.0.0.0" && &line[..9] != "127.0.0.1" {
            continue;
        }
        let host = if &line[..9] == "127.0.0.1" {
            &line[10..]
        } else {
            &line[8..]
        };
        if matches!(
            host,
            "local" | "localhost" | "localhost.localdomain" | "broadcasthost"
        ) {
            continue;
        }
        hosts.push(host.trim().to_string());
    }
    hosts
}

/// `readSimpleList`: every non-empty, non-comment line.
fn daemon_simple(raw: &str) -> Vec<String> {
    raw.split('\n')
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim().to_string())
        .collect()
}

/// What the daemon's `readLists` would see: `<dir>/*.*`, hidden skipped.
fn daemon_visible_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains('.') && !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

fn state() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    (dir, canonical)
}

fn hosts(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

#[test]
fn a_written_list_round_trips_through_the_daemons_parsers() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads-0123456789abcdef");
    let entries = classify(hosts(&[
        "ads.example",
        "tracker.example",
        "203.0.113.7",
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "255.255.255.255",
    ]));
    assert_eq!(entries.domains, hosts(&["ads.example", "tracker.example"]));
    assert_eq!(entries.ips, hosts(&["203.0.113.7"]));
    dir.write_list(&id, ListKind::Domains, &entries.domains)
        .unwrap();
    dir.write_list(&id, ListKind::Ips, &entries.ips).unwrap();

    let domains_dir = dir.kind_dir(&id, ListKind::Domains);
    assert_eq!(daemon_visible_files(&domains_dir), vec!["domains.list"]);
    let raw = std::fs::read_to_string(domains_dir.join("domains.list")).unwrap();
    assert_eq!(raw, "0.0.0.0 ads.example\n0.0.0.0 tracker.example\n");
    assert_eq!(daemon_domains(&raw), entries.domains);

    let ips_dir = dir.kind_dir(&id, ListKind::Ips);
    assert_eq!(daemon_visible_files(&ips_dir), vec!["ips.list"]);
    let raw = std::fs::read_to_string(ips_dir.join("ips.list")).unwrap();
    assert_eq!(daemon_simple(&raw), entries.ips);
}

#[test]
fn directories_are_0700_files_0600_and_paths_canonical() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads");
    dir.write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .unwrap();
    let kind_dir = dir.kind_dir(&id, ListKind::Domains);
    assert_eq!(kind_dir, state.join("blocklists/ads/domains"));
    assert_eq!(kind_dir.canonicalize().unwrap(), kind_dir);
    assert!(!kind_dir.to_string_lossy().ends_with('/'));
    for path in [
        state.join("blocklists"),
        state.join("blocklists/ads"),
        kind_dir.clone(),
    ] {
        assert_eq!(mode(&path), 0o700, "{}", path.display());
    }
    assert_eq!(mode(&kind_dir.join("domains.list")), 0o600);
}

#[test]
fn a_rewrite_replaces_the_file_and_leaves_no_temp_file() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads");
    dir.write_list(&id, ListKind::Domains, &hosts(&["a.example", "b.example"]))
        .unwrap();
    // A temp file left by a crash is replaced, never appended to.
    let kind_dir = dir.kind_dir(&id, ListKind::Domains);
    std::fs::write(
        kind_dir.join(".domains.list.tmp"),
        "0.0.0.0 stale.example\n",
    )
    .unwrap();
    dir.write_list(&id, ListKind::Domains, &hosts(&["c.example"]))
        .unwrap();
    let raw = std::fs::read_to_string(kind_dir.join("domains.list")).unwrap();
    assert_eq!(raw, "0.0.0.0 c.example\n");
    assert_eq!(
        std::fs::read_dir(&kind_dir).unwrap().count(),
        1,
        "only the list file remains"
    );
}

#[test]
fn ids_become_safe_injective_path_components() {
    assert_eq!(
        IdComponent::from_id("ads-0123456789abcdef").as_str(),
        "ads-0123456789abcdef"
    );
    let mut seen = std::collections::BTreeSet::new();
    for raw in [
        "../etc",
        "a/b",
        "a:b",
        "a_b",
        ".hidden",
        "",
        "x".repeat(82).as_str(),
        "x".repeat(81).as_str(),
        "évil",
        "a\nb",
    ] {
        let id = IdComponent::from_id(raw);
        let s = id.as_str();
        assert!(!s.is_empty() && !s.starts_with('.'), "{raw:?} -> {s:?}");
        assert!(s.len() <= MAX_ID_COMPONENT_BYTES, "{raw:?} -> {s:?}");
        assert!(
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'),
            "{raw:?} -> {s:?}"
        );
        assert_eq!(IdComponent::parse(s), Some(id.clone()), "{raw:?}");
        assert!(seen.insert(s.to_string()), "{raw:?} collides as {s:?}");
    }
    assert_eq!(IdComponent::parse("../x"), None);
    assert_eq!(IdComponent::parse(".x"), None);
    assert_eq!(IdComponent::parse("x.notahash"), None);
}

#[test]
fn a_symlinked_root_is_refused() {
    let (_t, state) = state();
    let elsewhere = tempfile::tempdir().unwrap();
    symlink(elsewhere.path(), state.join("blocklists")).unwrap();
    assert!(ListDir::open(&state).is_err());
}

#[test]
fn a_symlinked_list_or_kind_directory_is_refused_and_its_target_untouched() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads");
    let elsewhere = tempfile::tempdir().unwrap();

    symlink(elsewhere.path(), state.join("blocklists/ads")).unwrap();
    assert!(dir
        .write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .is_err());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);

    std::fs::remove_file(state.join("blocklists/ads")).unwrap();
    std::fs::create_dir(state.join("blocklists/ads")).unwrap();
    symlink(elsewhere.path(), state.join("blocklists/ads/domains")).unwrap();
    assert!(dir
        .write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .is_err());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn a_symlinked_list_or_temp_file_is_replaced_not_followed() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads");
    dir.write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .unwrap();
    let kind_dir = dir.kind_dir(&id, ListKind::Domains);
    let victim_dir = tempfile::tempdir().unwrap();
    let victim = victim_dir.path().join("victim");
    std::fs::write(&victim, "keep").unwrap();
    std::fs::remove_file(kind_dir.join("domains.list")).unwrap();
    symlink(&victim, kind_dir.join("domains.list")).unwrap();
    symlink(&victim, kind_dir.join(".domains.list.tmp")).unwrap();

    dir.write_list(&id, ListKind::Domains, &hosts(&["b.example"]))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    let meta = std::fs::symlink_metadata(kind_dir.join("domains.list")).unwrap();
    assert!(meta.is_file(), "the link was replaced by a regular file");
    assert!(dir.has_list(&id, ListKind::Domains));
}

#[test]
fn has_list_is_false_for_a_missing_or_symlinked_file() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads");
    assert!(!dir.has_list(&id, ListKind::Domains));
    dir.write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .unwrap();
    assert!(dir.has_list(&id, ListKind::Domains));
    let file = dir.kind_dir(&id, ListKind::Domains).join("domains.list");
    std::fs::remove_file(&file).unwrap();
    symlink("/etc/hostname", &file).unwrap();
    assert!(!dir.has_list(&id, ListKind::Domains));
}

#[test]
fn caps_refuse_an_oversized_list_and_keep_the_previous_file() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap().with_caps(2, 1024);
    let id = IdComponent::from_id("ads");
    dir.write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .unwrap();
    assert!(dir
        .write_list(
            &id,
            ListKind::Domains,
            &hosts(&["a.example", "b.example", "c.example"])
        )
        .is_err());
    let kind_dir = dir.kind_dir(&id, ListKind::Domains);
    assert_eq!(
        std::fs::read_to_string(kind_dir.join("domains.list")).unwrap(),
        "0.0.0.0 a.example\n"
    );
    assert_eq!(
        std::fs::read_dir(&kind_dir).unwrap().count(),
        1,
        "no temp file left"
    );
    let small = ListDir::open(&state).unwrap().with_caps(10, 20);
    assert!(small
        .write_list(&id, ListKind::Domains, &hosts(&["a.example", "b.example"]))
        .is_err());
    const { assert!(MAX_LIST_FILE_BYTES >= crate::blocklists::fetcher::MAX_BODY_BYTES) };
    assert_eq!(MAX_LIST_LINES, crate::blocklists::format::MAX_ENTRIES);
}

#[test]
fn an_entry_that_would_break_the_line_format_is_refused() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads");
    for bad in [
        "a.example\n0.0.0.0 x",
        "a b",
        "",
        "A.EXAMPLE",
        "a.example\r",
    ] {
        assert!(
            dir.write_list(&id, ListKind::Domains, &hosts(&[bad]))
                .is_err(),
            "{bad:?}"
        );
    }
    assert!(dir
        .write_list(&id, ListKind::Ips, &hosts(&["1.2.3"]))
        .is_err());
}

#[test]
fn removing_a_list_or_kind_deletes_only_that_directory() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let ads = IdComponent::from_id("ads");
    let other = IdComponent::from_id("other");
    for id in [&ads, &other] {
        dir.write_list(id, ListKind::Domains, &hosts(&["a.example"]))
            .unwrap();
        dir.write_list(id, ListKind::Ips, &hosts(&["203.0.113.7"]))
            .unwrap();
    }
    dir.remove_kind(&ads, ListKind::Ips).unwrap();
    assert!(!dir.kind_dir(&ads, ListKind::Ips).exists());
    assert!(dir.has_list(&ads, ListKind::Domains));
    dir.remove_list(&ads).unwrap();
    assert!(!state.join("blocklists/ads").exists());
    assert!(dir.has_list(&other, ListKind::Ips));
    // Removing what isn't there is fine.
    dir.remove_list(&ads).unwrap();
    dir.remove_kind(&ads, ListKind::Domains).unwrap();
    assert_eq!(dir.lists().unwrap(), vec![other]);
}

#[test]
fn removing_a_symlinked_list_removes_the_link_only() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("keep.list"), "x").unwrap();
    symlink(elsewhere.path(), state.join("blocklists/ads")).unwrap();
    dir.remove_list(&IdComponent::from_id("ads")).unwrap();
    assert!(elsewhere.path().join("keep.list").exists());
    assert!(std::fs::symlink_metadata(state.join("blocklists/ads")).is_err());
}

/// Review L5: a hostile list must not be able to block the gateway, local
/// DNS or anything else on the machine's own networks.
#[test]
fn ips_on_local_special_or_reserved_networks_are_dropped() {
    let dropped = [
        "10.0.0.1",
        "172.16.5.4",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.1.1",
        "100.64.0.1",
        "100.127.255.254",
        "224.0.0.251",
        "239.255.255.250",
        "240.0.0.1",
        "255.255.255.255",
        "0.0.0.0",
        "0.1.2.3",
        "127.0.0.53",
    ];
    let kept = [
        "203.0.113.7",
        "8.8.8.8",
        "100.63.255.255",
        "100.128.0.1",
        "172.15.0.1",
        "172.32.0.1",
    ];
    let entries = classify(
        dropped
            .iter()
            .chain(kept.iter())
            .map(|s| s.to_string())
            .collect(),
    );
    assert_eq!(entries.ips, hosts(&kept));
    assert!(entries.domains.is_empty(), "{:?}", entries.domains);
}

/// Review L1: the daemon uses `data` as a glob (`<data>/*.*`), so a state
/// path with glob metacharacters (an unclosed `[` can hang it) or that isn't
/// UTF-8 is refused.
#[test]
fn a_state_path_the_daemon_would_read_as_a_glob_is_refused() {
    let (_t, state) = state();
    for name in ["a[b", "a*b", "a?b", "a\\b", "a]b"] {
        let dir = state.join(name);
        std::fs::create_dir(&dir).unwrap();
        assert!(ListDir::open(&dir).is_err(), "{name}");
        assert!(!dir.join("blocklists").exists(), "{name}");
    }
    use std::os::unix::ffi::OsStrExt;
    let odd = state.join(std::ffi::OsStr::from_bytes(b"a\xffb"));
    std::fs::create_dir(&odd).unwrap();
    assert!(ListDir::open(&odd).is_err(), "non-UTF-8");
}

/// Review H1: an unchanged list is not rewritten (the daemon would re-read
/// every list on any change), a changed one is, atomically.
#[test]
fn an_unchanged_list_is_not_rewritten() {
    let (_t, state) = state();
    let dir = ListDir::open(&state).unwrap();
    let id = IdComponent::from_id("ads");
    let file = dir.kind_dir(&id, ListKind::Domains).join("domains.list");
    let inode = |p: &Path| std::fs::metadata(p).unwrap().ino();
    assert!(dir
        .write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .unwrap());
    let first = inode(&file);
    assert!(!dir
        .write_list(&id, ListKind::Domains, &hosts(&["a.example"]))
        .unwrap());
    assert_eq!(inode(&file), first, "an identical list was rewritten");
    assert!(dir
        .write_list(&id, ListKind::Domains, &hosts(&["b.example"]))
        .unwrap());
    assert_ne!(inode(&file), first);
    // Same length, different bytes: still rewritten.
    assert!(dir
        .write_list(&id, ListKind::Domains, &hosts(&["c.example"]))
        .unwrap());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "0.0.0.0 c.example\n"
    );
}
