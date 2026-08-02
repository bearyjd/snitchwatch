// Traffic tab (Task 11 — view layer over TrafficModel).
//
// Issue #19: opensnitchd's `Connection` proto carries no byte counters, so
// the per-connection byte-rate chart this page used to render was always
// zero-valued and could never populate. The daemon does report rich
// aggregate `Statistics` on every `Ping` (connections/accepted/dropped/rule
// hits/etc.) — the bridge now forwards those as `DaemonStatistics` messages
// (see `snitchwatch_bridge::ws_messages::ServerMessage::DaemonStatistics`),
// and this page is rebuilt around them: a grid of stat tiles instead of the
// old Canvas chart. `TrafficModel`'s series/rate plumbing (feeding a
// `Canvas`-based byte-rate chart) is left in place on the Rust side for a
// future per-connection byte source — only this page's presentation drops it.
//
// Staleness note: the daemon only pings when it has new connection activity
// — `Statistics.Serialize()` returns nil with no new events since the last
// ping (`vendor/opensnitch/daemon/statistics/stats.go:266`), and the client
// skips the Ping RPC entirely in that case
// (`vendor/opensnitch/daemon/client.go:337-341`). So on an idle system these
// counters (uptime included) reflect the *last* report, not "now" — the
// placeholder and uptime label below say so explicitly rather than implying
// a live/fixed-cadence feed.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.Page {
    id: page
    title: "Traffic"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property TrafficModel model

    // Static tile definitions: `key` names the TrafficModel qproperty each
    // tile reads. This array is built once (it never reads `page.model`
    // itself) so the Repeater's delegate set is stable across every
    // DaemonStatistics update — only each delegate's own `text` binding
    // (below) re-evaluates when its one property changes, not all six.
    readonly property var tiles: [
        { label: "Connections", key: "statConnections" },
        { label: "Accepted", key: "statAccepted" },
        { label: "Dropped", key: "statDropped" },
        { label: "Rule hits", key: "statRuleHits" },
        { label: "Rule misses", key: "statRuleMisses" },
        { label: "Rules", key: "statRules" },
    ]

    // i32 qproperties saturate at i32::MAX on the Rust side (see
    // `traffic_model::u64_to_display_i32`) rather than wrapping — flag that
    // display cap here too instead of silently showing a suspiciously round
    // number.
    readonly property int i32Max: 2147483647

    function formatStatValue(key) {
        if (!page.model) {
            return "0";
        }
        const raw = page.model[key];
        const formatted = Number(raw).toLocaleString(Qt.locale());
        return raw === page.i32Max ? formatted + "+" : formatted;
    }

    // Format a seconds count as "1h 2m 3s", dropping leading zero units.
    function formatUptime(totalSeconds) {
        if (totalSeconds <= 0) {
            return "0s";
        }
        const h = Math.floor(totalSeconds / 3600);
        const m = Math.floor((totalSeconds % 3600) / 60);
        const s = totalSeconds % 60;
        let parts = [];
        if (h > 0) parts.push(h + "h");
        if (h > 0 || m > 0) parts.push(m + "m");
        parts.push(s + "s");
        return parts.join(" ");
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || !page.model.statsReceived
        icon.name: "office-chart-line"
        text: "Waiting for daemon statistics"
        explanation: "opensnitchd reports aggregate connection statistics when it has new \
                      connection activity to report, not on a fixed schedule — this page \
                      populates once the first report arrives."
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Kirigami.Units.largeSpacing
        spacing: Kirigami.Units.largeSpacing
        visible: page.model && page.model.statsReceived

        GridLayout {
            Layout.fillWidth: true
            columns: 3
            columnSpacing: Kirigami.Units.largeSpacing
            rowSpacing: Kirigami.Units.largeSpacing

            Repeater {
                model: page.tiles

                delegate: Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 5
                    radius: Kirigami.Units.smallSpacing
                    color: Kirigami.Theme.alternateBackgroundColor
                    border.color: Kirigami.Theme.disabledTextColor
                    border.width: 1

                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: Kirigami.Units.smallSpacing

                        Controls.Label {
                            Layout.alignment: Qt.AlignHCenter
                            textFormat: Text.PlainText
                            text: page.formatStatValue(modelData.key)
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.8
                            font.bold: true
                        }
                        Controls.Label {
                            Layout.alignment: Qt.AlignHCenter
                            textFormat: Text.PlainText
                            text: modelData.label
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }
                }
            }
        }

        Item {
            Layout.fillHeight: true
        }

        Controls.Label {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignHCenter
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            color: Kirigami.Theme.disabledTextColor
            // `daemonVersion` is daemon-supplied (and, transitively,
            // attacker-influenced on a compromised/malicious daemon) text —
            // the bridge already sanitizes it (`sanitize_for_display`)
            // before it reaches the wire, and `textFormat: Text.PlainText`
            // above additionally forecloses Qt's `AutoText` rich-text
            // heuristic from ever interpreting it as markup.
            text: page.model
                ? "opensnitchd " + page.model.daemonVersion
                    + " — uptime at last report: " + page.formatUptime(page.model.uptimeSecs)
                : ""
        }
    }
}
