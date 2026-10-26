import QtQuick

// Keeps ConnectionsModel.oldestPendingAgeSecs (and therefore main.qml's
// pendingExposureBanner) live: that property only changes automatically on
// the next bridge message, so elapsed wall-clock time otherwise needs an
// explicit poke. Only runs while something is *actively* pending (Codex
// review: gating on pendingCount > 0 alone ticks forever once a
// stuck/expired row exists — see oldest_pending_started_at_ms's doc comment
// in row_store.rs — since a stuck row never resolves and never expires from
// pendingCount either; oldestPendingAgeSecs is already -1 in exactly that
// case, so gating on it stops the wakeups once the real exposure window
// closes, regardless of whether the stuck row itself is ever cleaned up).
// It also runs while a held-back notice count is shown (heldBackShown, 0d
// part 2), which can sit on such a stuck row: the count's hour must still
// end, after which the ticker stops.
Timer {
    id: ticker
    required property var connections
    interval: 1000
    running: ticker.connections.oldestPendingAgeSecs >= 0 || ticker.connections.heldBackShown
    repeat: true
    onTriggered: ticker.connections.refreshPendingAge()
}
