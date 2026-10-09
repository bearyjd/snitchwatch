//! The opt-in DNS entry end to end (owner decision S6, issue #117): a system
//! bridge with a persistent state directory, driven over its inbound channel
//! like a GUI, and the mock daemon, which checks every rule the way
//! opensnitchd compiles it (`validate_rule_shape`). The other entries'
//! scenarios are in `curated_defaults.rs`; both use `curated_support`.

use mock_opensnitchd::round_trip::as_daemon_reports;
use snitchwatch_bridge::curated::reconcile::EntryStatus;
use snitchwatch_bridge::curated::wire::CuratedDefaultSummary;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::BridgeMode;
use snitchwatch_proto::protocol::Action;
use tokio::sync::broadcast;

mod curated_support;
use curated_support::{connect_daemon, entry_until_id, next_command, nothing_sent, send, start};

const DNS: &str = "dns-resolved";
const DNS_RULE: &str = "snitchwatch-default-dns-resolved";
const FLATPAK: &str = "flatpak-flathub";

/// Watch `SetCuratedDefaults` until the entry `id` satisfies `done`.
async fn entry_until(
    rx: &mut broadcast::Receiver<ServerMessage>,
    id: &str,
    what: &str,
    done: impl Fn(&CuratedDefaultSummary) -> bool,
) -> CuratedDefaultSummary {
    entry_until_id(rx, id, what, |entry, _| done(entry)).await
}

fn turn(id: &str, on: bool) -> ClientMessage {
    ClientMessage::SetCuratedDefaults {
        ids: vec![id.into()],
        on,
    }
}

/// The DNS entry is off until chosen, installs exactly the resolver's rule
/// (the mock daemon compiles it like opensnitchd, and the install counts only
/// after its `OK`), pinned to the resolver's account, and no other entry
/// comes with it. Only it is marked broad on the wire.
#[tokio::test]
async fn the_dns_entry_is_off_until_chosen_and_installs_exactly_the_resolver_rule() {
    let mut s = start(BridgeMode::System).await;
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    nothing_sent(&mut seen).await;

    // Listed with the bridge's own words, off, before any choice.
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    let listed = entry_until(&mut s.rx, DNS, "listed", |_| true).await;
    assert!(!listed.on && listed.broad);
    assert_eq!(listed.status, EntryStatus::Off);
    assert_eq!(listed.program, "/usr/lib/systemd/systemd-resolved");
    assert_eq!(
        listed.allows,
        "/usr/lib/systemd/systemd-resolved may connect to any address on TCP and UDP port 53, \
         over IPv4 and IPv6, but only while it runs as user ID 193 (the systemd-resolve account)."
    );
    assert!(listed
        .why
        .contains("any program can send data out inside lookups"));
    assert!(listed.why.contains("every app's lookups use this rule"));
    nothing_sent(&mut seen).await;
    // The named-place entries aren't broad, so "Turn all on" keeps asking
    // for them.
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    let flatpak = entry_until(&mut s.rx, FLATPAK, "flatpak listed", |_| true).await;
    assert!(!flatpak.broad);

    // Turning it on sends one CHANGE_RULE, of exactly that rule.
    send(&s.bridge, turn(DNS, true)).await;
    let change = next_command(&mut seen).await;
    assert_eq!(change.r#type, Action::ChangeRule as i32);
    assert_eq!(change.rules.len(), 1);
    let rule = &change.rules[0];
    assert_eq!(rule.name, DNS_RULE);
    assert_eq!(
        (
            rule.action.as_str(),
            rule.duration.as_str(),
            rule.precedence
        ),
        ("allow", "always", false)
    );
    let leaves: Vec<(&str, &str, &str, bool)> = rule
        .operator
        .as_ref()
        .unwrap()
        .list
        .iter()
        .map(|op| {
            (
                op.r#type.as_str(),
                op.operand.as_str(),
                op.data.as_str(),
                op.sensitive,
            )
        })
        .collect();
    assert_eq!(
        leaves,
        [
            (
                "simple",
                "process.path",
                "/usr/lib/systemd/systemd-resolved",
                true
            ),
            ("simple", "user.id", "193", false),
            ("simple", "dest.port", "53", false),
            ("regexp", "protocol", "^(tcp|udp)6?$", false),
        ]
    );
    let installed = entry_until(&mut s.rx, DNS, "installed", |e| {
        e.status == EntryStatus::Installed
    })
    .await;
    assert!(installed.on);
    // The other entries stay off, and nothing else is sent.
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    let flatpak = entry_until(&mut s.rx, FLATPAK, "flatpak", |_| true).await;
    assert_eq!((flatpak.on, flatpak.status), (false, EntryStatus::Off));
    nothing_sent(&mut seen).await;

    // Opt out: one DELETE_RULE of our own rule.
    send(&s.bridge, turn(DNS, false)).await;
    let delete = next_command(&mut seen).await;
    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    assert_eq!(delete.rules[0].name, DNS_RULE);
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// The user's side of the DNS entry, as for every entry: an edited copy
/// (its port widened to 5353 here) is left alone, even on opt-out, and a copy
/// deleted outside Snitchwatch stays deleted across a restart.
#[tokio::test]
async fn a_deleted_or_edited_dns_rule_is_left_as_the_user_made_it() {
    let mut s = start(BridgeMode::System).await;
    let (daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    send(&s.bridge, turn(DNS, true)).await;
    let installed = next_command(&mut seen).await.rules[0].clone();
    entry_until(&mut s.rx, DNS, "installed", |e| {
        e.status == EntryStatus::Installed
    })
    .await;
    drop((daemon, seen));

    // The daemon reports our copy back in its own shape: nothing to do.
    let (daemon, mut seen) =
        connect_daemon(&s.bridge, 2, vec![as_daemon_reports(&installed)]).await;
    nothing_sent(&mut seen).await;
    while s.rx.try_recv().is_ok() {}
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    entry_until(&mut s.rx, DNS, "still installed", |e| {
        e.status == EntryStatus::Installed
    })
    .await;
    drop((daemon, seen));

    // Edited outside Snitchwatch: left alone, never overwritten or deleted.
    let mut edited = as_daemon_reports(&installed);
    edited.operator.as_mut().unwrap().list[2].data = "5353".into();
    let (daemon, mut seen) = connect_daemon(&s.bridge, 3, vec![edited]).await;
    entry_until(&mut s.rx, DNS, "edited by you", |e| {
        e.status == EntryStatus::EditedByYou
    })
    .await;
    send(&s.bridge, turn(DNS, false)).await;
    entry_until(&mut s.rx, DNS, "off, edit kept", |e| {
        !e.on && e.status == EntryStatus::EditedByYou
    })
    .await;
    nothing_sent(&mut seen).await;
    drop((daemon, seen));

    // Deleted outside (turn it on again, then the daemon comes back
    // without it): never reinstalled.
    send(&s.bridge, turn(DNS, true)).await;
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 4, Vec::new()).await;
    entry_until(&mut s.rx, DNS, "deleted outside", |e| {
        e.status == EntryStatus::DeletedOutside
    })
    .await;
    nothing_sent(&mut seen).await;
    let saved = std::fs::read_to_string(s.state.join("curated-defaults.json")).unwrap();
    assert!(
        saved.contains(r#""deletedByUser":["dns-resolved"]"#),
        "{saved}"
    );
    s.bridge.shutdown();
}

/// A copy that lost its sender pin would allow any user's `LD_PRELOAD`ed
/// resolver: it reads as an edit, is left alone (it still applies, so the
/// page flags it and offers Remove), and is neither overwritten nor deleted.
#[tokio::test]
async fn a_dns_copy_without_its_sender_pin_is_an_edit_left_alone() {
    let mut s = start(BridgeMode::System).await;
    let (daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    send(&s.bridge, turn(DNS, true)).await;
    let installed = next_command(&mut seen).await.rules[0].clone();
    entry_until(&mut s.rx, DNS, "installed", |e| {
        e.status == EntryStatus::Installed
    })
    .await;
    drop((daemon, seen));

    let mut unpinned = as_daemon_reports(&installed);
    assert_eq!(
        unpinned.operator.as_ref().unwrap().list[1].operand,
        "user.id"
    );
    unpinned.operator.as_mut().unwrap().list.remove(1);
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 2, vec![unpinned]).await;
    entry_until(&mut s.rx, DNS, "edited by you", |e| {
        e.status == EntryStatus::EditedByYou
    })
    .await;
    nothing_sent(&mut seen).await;
    send(&s.bridge, turn(DNS, false)).await;
    entry_until(&mut s.rx, DNS, "off, edit kept", |e| {
        !e.on && e.status == EntryStatus::EditedByYou
    })
    .await;
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}
