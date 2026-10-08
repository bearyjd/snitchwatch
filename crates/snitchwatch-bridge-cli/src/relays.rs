//! Small background tasks `run` starts that only forward one internal source
//! onto the outbound broadcast: tray and desktop notices, the filtering
//! pause's expiry, profile events and the traffic pump. Moved out of `lib.rs`
//! unchanged; each takes exactly what it forwards.

use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::cache::traffic_tracker::TrafficTracker;
use snitchwatch_bridge::filter_pause::FilterPause;
use snitchwatch_bridge::notice::{Notice, NoticeBus};
use snitchwatch_bridge::profiles::ProfilesManager;
use snitchwatch_bridge::tray_state::TrayStatePublisher;
use snitchwatch_bridge::ws_messages::ServerMessage;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tracing::warn;

/// Rolling window kept by the traffic pump's [`TrafficTracker`], matching
/// `snitchwatch-kirigami::traffic::ring_store::DEFAULT_WINDOW_SECONDS` (the
/// consumer side of the same underlying `TrafficBinner`).
const TRAFFIC_WINDOW_SECONDS: usize = 300;

/// After any pause change, show the current pause state everywhere: on the
/// tray (through the cache, which publishes `FilterOff` while a pause is
/// active) and to every GUI (`FilterPauseState`, issue #47). Read and sent
/// under the cache lock, so announcements from the pump, the expiry task and
/// the last-loss clear reach GUIs in the order they happened, and the last
/// one always matches the last change.
pub(crate) async fn announce_pause_state(
    filter_pause: &FilterPause,
    cache: &Mutex<ConnectionCache>,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
) {
    let cache = cache.lock().await;
    cache.resync_tray_state();
    let _ = broadcast_tx.send(filter_pause.state().to_message());
}

/// External shells receive the same tray and desktop-notice inputs as
/// in-process shells. These are additive WebSocket actions, so older
/// clients remain compatible by ignoring actions they do not understand.
/// Subscribe before spawning the pumps; snapshots cover the current
/// tray value for a client that connects after a state transition.
pub(crate) fn spawn_tray_and_notice_relays(
    tray_pub: &TrayStatePublisher,
    notice_bus: &NoticeBus,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
) {
    let mut tray_events = tray_pub.subscribe();
    let tray_events_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        while tray_events.changed().await.is_ok() {
            let _ = tray_events_tx.send(ServerMessage::TrayState {
                state: tray_events.borrow().clone(),
            });
        }
    });
    let mut notice_events = notice_bus.subscribe();
    let notice_events_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        loop {
            match notice_events.recv().await {
                Ok(notice) => {
                    let _ = notice_events_tx.send(ServerMessage::Notice { notice });
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "notice relay lagged behind bridge");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// The filtering pause (issue #47) resets to unpaused on every bridge
/// start, matching every other in-memory bridge state. It ends at the
/// earliest of its deadline, an explicit resume, or the last
/// authenticated GUI session ending. Returns the (last-session-loss clear,
/// deadline expiry) task handles.
pub(crate) fn spawn_pause_tasks(
    client_presence: &snitchwatch_bridge::client_presence::ClientPresence,
    filter_pause: &Arc<FilterPause>,
    cache: &Arc<Mutex<ConnectionCache>>,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
    notice_bus: &Arc<NoticeBus>,
) -> (JoinHandle<()>, JoinHandle<()>) {
    let clear_handle = {
        let filter_pause = filter_pause.clone();
        let cache = cache.clone();
        let broadcast_tx = broadcast_tx.clone();
        tokio::spawn(
            snitchwatch_bridge::client_presence::clear_pause_on_last_session_loss(
                client_presence.session_losses(),
                filter_pause.clone(),
                move || {
                    let filter_pause = filter_pause.clone();
                    let cache = cache.clone();
                    let broadcast_tx = broadcast_tx.clone();
                    async move { announce_pause_state(&filter_pause, &cache, &broadcast_tx).await }
                },
            ),
        )
    };
    let expiry_handle = {
        let filter_pause = filter_pause.clone();
        let cache = cache.clone();
        let broadcast_tx = broadcast_tx.clone();
        let notice_bus = notice_bus.clone();
        tokio::spawn(snitchwatch_bridge::filter_pause::expire_pause_on_deadline(
            filter_pause.clone(),
            move || {
                notice_bus.send(Notice::FilterPauseExpired);
                let filter_pause = filter_pause.clone();
                let cache = cache.clone();
                let broadcast_tx = broadcast_tx.clone();
                async move { announce_pause_state(&filter_pause, &cache, &broadcast_tx).await }
            },
        ))
    };
    (clear_handle, expiry_handle)
}

/// Profile events → `SetProfiles` / `ProfileChanged` broadcasts. Mirrors
/// `snitchwatch_bridge::blocklists::spawn_event_pump`: the manager owns no
/// knowledge of the WS wire format, so this is where its internal
/// `ProfileEvent`s become the typed `ServerMessage`s every consumer (WS
/// clients, the in-process Kirigami shell) sees.
pub(crate) fn spawn_profile_event_relay(
    profiles_mgr: &Arc<ProfilesManager>,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
) {
    let profiles_for_events = profiles_mgr.clone();
    let mut profile_rx = profiles_mgr.subscribe();
    let bc_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        use snitchwatch_bridge::profiles::ProfileEvent as Evt;
        use snitchwatch_bridge::translator::downstream::{
            build_profile_changed, build_set_profiles,
        };
        while let Ok(evt) = profile_rx.recv().await {
            match evt {
                Evt::ProfilesChanged => {
                    if let Ok(m) = build_set_profiles(&profiles_for_events).await {
                        let _ = bc_tx.send(m);
                    }
                }
                Evt::ActiveProfileChanged { profile_id } => {
                    let _ = bc_tx.send(build_profile_changed(profile_id));
                    if let Ok(m) = build_set_profiles(&profiles_for_events).await {
                        let _ = bc_tx.send(m);
                    }
                }
            }
        }
    });
}

/// Traffic pump: connection-row byte counters → binned `TrafficEvents`.
/// Additive: subscribes to the same outbound broadcast every other
/// consumer uses and folds each connection-row batch's byte counters
/// through `TrafficTracker` (wrapping the existing, already-tested
/// `TrafficBinner`), re-broadcasting the result as `TrafficEvents` — the
/// one typed traffic variant the native Kirigami shell's `TrafficModel`
/// consumes (`bridge_dispatch::interests_traffic`). Never touches the
/// legacy `SetTrafficData`/`UpdateTrafficData` variants.
pub(crate) fn spawn_traffic_pump(broadcast_tx: &broadcast::Sender<ServerMessage>) {
    let mut traffic_rx = broadcast_tx.subscribe();
    let traffic_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        let mut tracker = TrafficTracker::new(TRAFFIC_WINDOW_SECONDS);
        loop {
            let msg = match traffic_rx.recv().await {
                Ok(msg) => msg,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "traffic pump lagged behind broadcast");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let rows = match &msg {
                ServerMessage::InsertConnectionRows { rows } => rows,
                ServerMessage::UpdateConnectionRows { rows } => rows,
                _ => continue,
            };
            if rows.is_empty() {
                continue;
            }
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let events = tracker.record_rows(now_ms, rows);
            if traffic_tx.receiver_count() > 0 {
                if let Err(e) = traffic_tx.send(ServerMessage::TrafficEvents { events }) {
                    warn!(error = %e, "traffic pump: broadcast send failed");
                }
            }
        }
    });
}
