// Pending-decision surface (Task 7; Parity 2 — durations, insight, sparkline).
//
// The safety-critical allow/deny controls for a novel connection. Embedded in
// the Connections inspector for a pending row; reusable as a standalone content
// block. The two verdict buttons (allow/deny) plus the host-match scope and
// rule-duration selectors map — in Rust (`pending_decision.rs`), not here —
// onto the bridge's typed `ClientMessage::SetVerdict`. This QML only collects
// the choice.
//
// Timeout ownership: `remainingSeconds` is *displayed* only; the auto-action
// countdown is owned server-side by the bridge's AskRule machinery. This
// component never runs its own timer.
//
// Insight panel + sparkline (Parity 2) are strictly decorative side-channels:
// a lookup failure/timeout or missing traffic model NEVER disables or delays
// the Allow/Deny buttons below. See `insight_model.rs` and
// `connections::row_store::row_by_id` for where their data comes from.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: sheet
    spacing: Kirigami.Units.largeSpacing

    // Populated by the caller from the selected pending row.
    property string rowId: ""
    property string process: ""
    property string host: ""
    // Remote IP (Parity 2 insight panel target) and this connection's
    // cumulative byte counters, both from `ConnectionsModel.rowDetailsJson`.
    property string remoteIp: ""
    property real bytesSent: 0
    property real bytesReceived: 0
    // Server-owned countdown to the automatic fallback action. Negative hides it.
    property int remainingSeconds: -1

    // Global last-60-second traffic feed (Parity 2 mini sparkline). Injected
    // by ConnectionsPage from the same TrafficModel instance the Traffic tab
    // uses — this is genuinely global, all-processes traffic (see the label
    // below), not per-connection; there is no per-process binning in the
    // bridge today. Null in isolated component tests, in which case the
    // sparkline area is simply empty.
    property var trafficModel: null

    // Live-wiring hub (Task 13), injected from ConnectionsPage. When set, a
    // submitted verdict's JSON is routed to the bridge's inbound pump. Null in
    // isolated component tests, in which case the verdict is a no-op sink.
    property var bridgeFeed: null

    // Emitted after a verdict is submitted so the container can close/advance.
    signal decided()

    // Rust submission surface (pure verdict->ClientMessage mapping lives there).
    PendingDecision {
        id: decision
    }

    // Reverse-DNS + RDAP lookup surface (Parity 2). Qt-free fetch/cache logic
    // lives in `insight::client`; this QObject only dispatches it async and
    // never blocks `lookup()`'s caller.
    PendingInsight {
        id: insight
    }

    // Route the emitted SetVerdict JSON to the bridge inbound (Task 13). Kept as
    // a signal->dispatcher hop (not a direct call in submit()) so the pure
    // verdict->ClientMessage mapping stays the single source of the JSON.
    Connections {
        target: decision
        enabled: sheet.bridgeFeed !== null
        function onVerdictSubmitted(json) {
            sheet.bridgeFeed.sendClientJson(json);
        }
    }

    // Kick off the best-effort insight lookup whenever the target IP changes
    // (a fresh pending row, or the sheet initially populating). A no-op for
    // an empty IP.
    onRemoteIpChanged: insight.lookup(sheet.remoteIp)
    Component.onCompleted: {
        insight.lookup(sheet.remoteIp);
        sheet.parseSparkline();
    }

    onTrafficModelChanged: sheet.parseSparkline()

    Connections {
        target: sheet.trafficModel
        enabled: sheet.trafficModel !== null
        function onSeriesJsonChanged() {
            sheet.parseSparkline();
        }
    }

    // Last-60-point (~60s) slice of the global in/out series, refreshed from
    // `trafficModel.seriesJson` — mirrors TrafficPage.qml's Canvas approach in
    // miniature. Malformed/missing data degrades to an empty sparkline rather
    // than throwing.
    property var sparkBytesIn: []
    property var sparkBytesOut: []

    function parseSparkline() {
        if (!sheet.trafficModel) {
            sheet.sparkBytesIn = [];
            sheet.sparkBytesOut = [];
            return;
        }
        try {
            const parsed = JSON.parse(sheet.trafficModel.seriesJson);
            const allIn = parsed.bytesIn || [];
            const allOut = parsed.bytesOut || [];
            const take = Math.min(60, allIn.length);
            sheet.sparkBytesIn = allIn.slice(allIn.length - take);
            sheet.sparkBytesOut = allOut.slice(allOut.length - take);
        } catch (e) {
            sheet.sparkBytesIn = [];
            sheet.sparkBytesOut = [];
        }
        sparkCanvas.requestPaint();
    }

    function formatBytes(n) {
        if (!n || n <= 0) return "0 B";
        if (n < 1024) return n.toFixed(0) + " B";
        if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
        if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MB";
        return (n / (1024 * 1024 * 1024)).toFixed(2) + " GB";
    }

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        visible: true
        type: Kirigami.MessageType.Warning
        text: sheet.process + " wants to connect to " + sheet.host
    }

    RowLayout {
        Layout.fillWidth: true
        Controls.Label {
            text: "Scope"
            Layout.alignment: Qt.AlignVCenter
        }
        Controls.ComboBox {
            id: scopeBox
            Layout.fillWidth: true
            textRole: "label"
            valueRole: "token"
            model: [
                { label: "This host only", token: "this_host" },
                { label: "Any host on this domain", token: "any_host_on_domain" },
                { label: "Any host", token: "any_host" }
            ]
        }
    }

    // Granular rule scopes (Parity 2): how long the resulting rule should
    // live. Maps onto the bridge's `VerdictDuration` — see
    // `pending_decision.rs`'s doc comment for the full duration-mapping
    // table, including the one lossy case ("Until quit" -> daemon
    // "until restart").
    RowLayout {
        Layout.fillWidth: true
        Controls.Label {
            text: "Duration"
            Layout.alignment: Qt.AlignVCenter
        }
        Controls.ComboBox {
            id: durationBox
            Layout.fillWidth: true
            textRole: "label"
            valueRole: "token"
            model: [
                { label: "This time", token: "this_time" },
                { label: "For 5 minutes", token: "for_5_minutes" },
                { label: "Until quit", token: "until_quit" },
                { label: "Forever", token: "forever" }
            ]
        }
    }

    // Countdown display only — never a client-side timer.
    Controls.Label {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        opacity: 0.7
        visible: sheet.remainingSeconds >= 0
        text: "Auto-action in " + sheet.remainingSeconds + "s"
    }

    // Insight panel (Parity 2) — best-effort research on the remote host.
    // Never gates the verdict buttons below: a hung/offline lookup shows
    // "Looking up..."/"unavailable (offline?)" forever, nothing more.
    Kirigami.FormLayout {
        Layout.fillWidth: true
        visible: sheet.remoteIp.length > 0

        Controls.Label {
            Kirigami.FormData.label: "Reverse DNS"
            text: insight.loading
                  ? "Looking up…"
                  : (insight.hostname.length > 0 ? insight.hostname : "unavailable")
        }
        Controls.Label {
            Kirigami.FormData.label: "Organization"
            visible: insight.org.length > 0
            text: insight.org
        }
        Controls.Label {
            Kirigami.FormData.label: "Registrar"
            visible: insight.registrar.length > 0
            text: insight.registrar
        }
        Controls.Label {
            Kirigami.FormData.label: "Country"
            visible: insight.country.length > 0
            text: insight.country
        }
        Controls.Label {
            Kirigami.FormData.label: "Registration info"
            visible: !insight.loading && !insight.available && insight.rdapEnabled
            opacity: 0.7
            text: "unavailable (offline?)"
        }
        Controls.Label {
            Kirigami.FormData.label: "Registration info"
            visible: !insight.rdapEnabled
            opacity: 0.7
            text: "Online research disabled — enable in Settings"
        }
    }

    // Mini traffic sparkline (Parity 2). Global, all-processes activity —
    // labeled honestly since there is no per-process traffic binning in the
    // bridge today (see the module docs above).
    ColumnLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing
        visible: sheet.trafficModel !== null

        Controls.Label {
            opacity: 0.7
            text: "Network activity (all processes, last 60s)"
        }

        Canvas {
            id: sparkCanvas
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 3
            antialiasing: true

            onWidthChanged: requestPaint()
            onHeightChanged: requestPaint()

            onPaint: {
                const ctx = getContext("2d");
                ctx.clearRect(0, 0, width, height);

                const n = sheet.sparkBytesIn.length;
                if (n === 0) {
                    return;
                }

                let maxVal = 1;
                for (let i = 0; i < n; i++) {
                    maxVal = Math.max(maxVal, sheet.sparkBytesIn[i], sheet.sparkBytesOut[i]);
                }

                function plot(series, color) {
                    ctx.strokeStyle = color;
                    ctx.lineWidth = 1.5;
                    ctx.beginPath();
                    for (let i = 0; i < n; i++) {
                        const x = n === 1 ? width : (width * i) / (n - 1);
                        const y = height * (1 - Math.min(1, series[i] / maxVal));
                        if (i === 0) {
                            ctx.moveTo(x, y);
                        } else {
                            ctx.lineTo(x, y);
                        }
                    }
                    ctx.stroke();
                }

                plot(sheet.sparkBytesIn, Kirigami.Theme.highlightColor);
                plot(sheet.sparkBytesOut, Kirigami.Theme.neutralTextColor);
            }
        }

        Controls.Label {
            opacity: 0.8
            text: "This connection: " + sheet.formatBytes(sheet.bytesSent) + " sent / "
                  + sheet.formatBytes(sheet.bytesReceived) + " received"
        }
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        Controls.Button {
            Layout.fillWidth: true
            text: "Allow"
            icon.name: "dialog-ok-apply"
            onClicked: sheet.submit("allow")
        }
        Controls.Button {
            Layout.fillWidth: true
            text: "Deny"
            icon.name: "edit-delete-remove"
            onClicked: sheet.submit("deny")
        }
    }

    function submit(action) {
        decision.submit(sheet.rowId, action, scopeBox.currentValue, durationBox.currentValue);
        sheet.decided();
    }
}
