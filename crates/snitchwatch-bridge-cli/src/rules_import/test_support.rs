//! Import-specific fixtures on top of [`crate::test_daemon`].

use super::apply::Applier;
use crate::replier::Replier;
pub(super) use crate::test_daemon::*;
use snitchwatch_bridge::rule_io::ImportOutcome;
use snitchwatch_bridge::ws_messages::ServerMessage;
use std::time::Duration;
use tokio::sync::broadcast;

pub(super) fn applier(daemon: &Daemon) -> Applier {
    Applier::new(
        daemon.commands.clone(),
        daemon.cache.clone(),
        Replier::broadcast(daemon.broadcast.clone()),
        "p".into(),
        Duration::from_secs(5),
        Duration::from_millis(10),
        crate::busy::BusyNames::default(),
    )
}

pub(super) fn progress(
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> Vec<(String, ImportOutcome)> {
    let mut out = Vec::new();
    while let Ok(message) = rx.try_recv() {
        if let ServerMessage::RulesImportProgress { name, outcome, .. } = message {
            out.push((name, outcome));
        }
    }
    out
}
