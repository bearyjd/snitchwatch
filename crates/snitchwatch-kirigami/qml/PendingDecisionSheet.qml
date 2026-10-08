// Pending-decision surface (Task 7; Parity 2 — durations, insight).
//
// The safety-critical allow/deny controls for a novel connection. Embedded in
// the Connections inspector for a pending row; reusable as a standalone content
// block. The two verdict buttons (allow/deny) plus the host-match scope and
// rule-duration selectors map — in Rust (`pending_decision.rs`), not here —
// onto the bridge's typed `ClientMessage::SetVerdict`. This QML only collects
// the choice.
//
// Timeout ownership: the bridge answers a prompt nobody answers (prompt-slot
// plan Part C) and reports when as the row's `answerDeadlineMs`. This sheet
// only shows the time left, with a display-only tick; it never acts on it.
//
// The insight panel (Parity 2) is a strictly decorative side-channel: a lookup
// failure/timeout NEVER disables or delays the Allow/Deny buttons below. See
// `insight_model.rs` and `connections::row_store::row_by_id` for where its
// data comes from.
//
// Deliberately no per-connection traffic readout or sparkline (issue #49): the
// bridge hardcodes every connection's byte counters to 0 and opensnitchd
// v1.8.0 has no per-connection counters, so anything drawn from them would be
// a fabricated "0 B" / flat line. Restore it only once a real byte source
// exists.
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
    // Remote IP (Parity 2 insight panel target), from
    // `ConnectionsModel.rowDetailsJson`.
    property string remoteIp: ""
    // When the bridge answers this prompt itself (Unix ms); -1 if it never
    // will. `nowMs` is refreshed by the display-only tick below.
    property real deadlineMs: -1
    property real nowMs: Date.now()
    // A new row's countdown starts from now, not from the last tick.
    onDeadlineMsChanged: sheet.nowMs = Date.now()
    readonly property int remainingSeconds: sheet.deadlineMs > 0
        ? Math.max(0, Math.ceil((sheet.deadlineMs - sheet.nowMs) / 1000)) : -1
    // Whether the row's bridge session takes "Decide later"
    // (`InlineVerdicts.rowDecideLater`); false until known.
    property bool decideLater: false
    // Issue #44: whether the bridge can remember an answer for this program
    // (`ConnectionsModel.rowDetailsJson`'s `bindableProcessPath`, the bridge's
    // own absolute-path rule). Without it only "This time" is offered: the
    // bridge would answer anything longer once anyway. False by default, so a
    // caller that never sets it can't offer a duration that won't be kept.
    // `submit()` also sends it to `BridgeFeed.submitVerdict`, whose Rust gate
    // sends a remembered verdict once-only without it.
    property bool bindableProcessPath: false
    // Exposed for the headless probe (tests/verdict_not_remembered_qml.rs).
    property alias durationSelector: durationBox
    // Whether the row's bridge advertised app-bound rules
    // (`InlineVerdicts.rowAppBoundRules`, the inline Deny's per-session
    // check). Without them a remembered "This host only" or "Any host on this
    // domain" answer covers every app (issue #72).
    property bool appBoundRules: false
    // Exposed for the headless probe (tests/sheet_old_bridge_qml.rs).
    property alias scopeSelector: scopeBox
    property alias oldBridgeNote: oldBridgeNoteLabel
    // Whether an answer can be remembered for this row and scope: the program
    // must be identifiable (#44), and unless the scope is "Any host" (a rule
    // on the program alone, even on older bridges), its bridge must bind host
    // rules to the program (#72). Otherwise only "This time" is offered, and
    // `submit()` sends it whatever the selector holds.
    readonly property bool remembers: sheet.bindableProcessPath
        && (sheet.appBoundRules || scopeBox.currentValue === "any_host")
    // The #44 note covers an unidentifiable program; this one an old bridge.
    readonly property bool showOldBridgeNote: sheet.bindableProcessPath && !sheet.remembers
    // Whether the one-time-Deny hint under the Duration box shows: only where
    // a longer Deny is offered (`remembers`). The Allow/Deny buttons submit at
    // once, so there is no selected action to key on: it follows the
    // duration, and its text names Deny.
    readonly property bool showDenyOnceHint: sheet.remembers
        && durationBox.currentValue === "this_time"
    // Exposed for the headless probe (tests/inline_verdict_qml.rs).
    property alias denyOnceHint: denyOnceHintLabel

    // Live-wiring hub (Task 13), injected from ConnectionsPage. When set, a
    // submitted verdict's JSON is routed to the bridge's inbound pump. Null in
    // isolated component tests, in which case the verdict is a no-op sink.
    property var bridgeFeed: null

    // Emitted after a verdict is submitted so the container can close/advance.
    signal decided()
    // Why an answer wasn't sent, for the container to show (fixed text).
    signal explained(string text)

    // Reverse-DNS + RDAP lookup surface (Parity 2). Qt-free fetch/cache logic
    // lives in `insight::client`; this QObject only dispatches it async and
    // never blocks `lookup()`'s caller.
    PendingInsight {
        id: insight
    }

    // Kick off the best-effort insight lookup whenever the target IP changes
    // (a fresh pending row, or the sheet initially populating). A no-op for
    // an empty IP.
    onRemoteIpChanged: insight.lookup(sheet.remoteIp)
    Component.onCompleted: insight.lookup(sheet.remoteIp)

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        visible: true
        type: Kirigami.MessageType.Warning
        text: "A new connection is waiting for your decision"
    }

    // The program name and destination are attacker-influenced, and
    // InlineMessage renders its text as AutoText with no textFormat hook (and
    // HTML-escaping wouldn't help — AutoText shows `&lt;` literally unless it
    // already judges the string to be markup). So they live in a PlainText
    // Label rather than in the message above (issue #51).
    Controls.Label {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        font.bold: true
        textFormat: Text.PlainText
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
            // Back-reference: `model[0]`'s token ("this_host") is the scope
            // the Connections page's inline Allow/Deny buttons hardcode
            // (`InlineVerdicts.send`) so an inline decision has the scope this
            // sheet would submit unchanged. Reordering this model or changing
            // its first entry's token changes that default too.
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
    // table. The `until_quit` token is daemon "until restart", so it is
    // labelled "Until firewall restarts": opensnitchd has no notion of an app
    // quitting.
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
            // Back-reference: `model[0]`'s token ("this_time") is what
            // `ConnectionsPage.qml`'s inline Allow sends, and `until_quit` is
            // what its inline Deny sends for a program it can bind a rule to,
            // on a bridge that advertised app-bound rules (`inline_deny.rs`).
            model: sheet.remembers
                ? [
                    { label: "This time", token: "this_time" },
                    { label: "For 5 minutes", token: "for_5_minutes" },
                    { label: "Until firewall restarts", token: "until_quit" },
                    { label: "Forever", token: "forever" }
                ]
                : [{ label: "This time", token: "this_time" }]
        }
    }

    // A one-time Deny isn't stored by the daemon: it drops only the packet
    // that asked, and most apps retry within a second or two (see
    // `showDenyOnceHint`).
    Controls.Label {
        id: denyOnceHintLabel
        Layout.fillWidth: true
        visible: sheet.showDenyOnceHint
        wrapMode: Text.Wrap
        opacity: 0.7
        font: Kirigami.Theme.smallFont
        textFormat: Text.PlainText
        text: "A one-time Deny blocks only this attempt; most apps retry within seconds."
    }

    // Issue #44: why only "This time" is offered. The bridge's
    // `RuleRefusal::describe` sentence, verbatim (a test keeps them equal).
    Controls.Label {
        Layout.fillWidth: true
        visible: !sheet.bindableProcessPath
        wrapMode: Text.Wrap
        opacity: 0.7
        font: Kirigami.Theme.smallFont
        textFormat: Text.PlainText
        text: "Snitchwatch couldn't identify this program's file, so this answer applies only to this connection."
    }

    // Issue #72: why a host-scoped answer can't be remembered on this bridge.
    Controls.Label {
        id: oldBridgeNoteLabel
        Layout.fillWidth: true
        visible: sheet.showOldBridgeNote
        wrapMode: Text.Wrap
        opacity: 0.7
        font: Kirigami.Theme.smallFont
        textFormat: Text.PlainText
        text: "This firewall bridge is too old to limit a rule to just this program, so with this scope it can only answer this connection. Update Snitchwatch's background service to remember answers."
    }

    Timer {
        interval: 1000
        repeat: true
        triggeredOnStart: true
        running: sheet.visible && sheet.deadlineMs > 0
        onTriggered: sheet.nowMs = Date.now()
    }

    // The time left; the bridge, not this sheet, answers at the deadline.
    Controls.Label {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        opacity: 0.7
        visible: sheet.remainingSeconds >= 0
        textFormat: Text.PlainText
        text: "If nobody answers within " + sheet.remainingSeconds
            + " s, the firewall's default action applies."
    }

    // Part C on a bridge without "Decide later": nothing can put this off.
    Controls.Label {
        Layout.fillWidth: true
        visible: !sheet.decideLater
        wrapMode: Text.Wrap
        opacity: 0.7
        font: Kirigami.Theme.smallFont
        textFormat: Text.PlainText
        text: "This background service can't put a prompt off. If nobody answers, the firewall applies its default action."
    }

    // Insight panel (Parity 2) — best-effort research on the remote host.
    // Never gates the verdict buttons below: a hung/offline lookup shows
    // "Looking up..."/"unavailable (offline?)" forever, nothing more.
    Kirigami.FormLayout {
        Layout.fillWidth: true
        visible: sheet.remoteIp.length > 0

        Controls.Label {
            Kirigami.FormData.label: "Reverse DNS"
            textFormat: Text.PlainText
            text: insight.loading
                  ? "Looking up…"
                  : (insight.hostname.length > 0
                     ? insight.hostname
                     : "No PTR result for " + sheet.remoteIp)
        }
        Controls.Label {
            Kirigami.FormData.label: "Organization"
            visible: insight.org.length > 0
            textFormat: Text.PlainText
            text: insight.org
        }
        Controls.Label {
            Kirigami.FormData.label: "Registrar"
            visible: insight.registrar.length > 0
            textFormat: Text.PlainText
            text: insight.registrar
        }
        Controls.Label {
            Kirigami.FormData.label: "Country"
            visible: insight.country.length > 0
            textFormat: Text.PlainText
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
        DecideLaterButton {
            Layout.fillWidth: true
            flat: false
            visible: sheet.decideLater
            onClicked: sheet.putOff()
        }
    }

    function putOff() {
        if (sheet.bridgeFeed !== null) {
            if (sheet.bridgeFeed.decideLater(sheet.rowId) === false) {
                sheet.explained("The connection to the background service was lost, so Decide later wasn't sent.");
            }
        } else {
            console.warn("PendingDecisionSheet: no bridgeFeed; Decide later dropped for", sheet.rowId);
        }
        sheet.decided();
    }

    function submit(action) {
        // Issues #44 and #72: never ask to remember an answer the bridge can't
        // bind to this program, whatever the selector holds. `remembers`
        // reads the scope sent below.
        const duration = sheet.remembers ? durationBox.currentValue : "this_time";
        if (sheet.bridgeFeed !== null) {
            // `bindableProcessPath`, not `remembers`: Rust gates on this flag
            // again, and must not depend on this sheet's own gate.
            const queued = sheet.bridgeFeed.submitVerdict(sheet.rowId, action,
                                                          scopeBox.currentValue, duration,
                                                          sheet.bindableProcessPath);
            if (queued === false) {
                sheet.explained("The connection to the background service was lost, so this answer wasn't sent.");
            }
        } else {
            // Unreachable in the running app; logged rather than dropped
            // silently so a mis-wired container can't lose a decision without
            // a trace. `decided()` still fires so the sheet closes either way.
            console.warn("PendingDecisionSheet: no bridgeFeed; verdict dropped for", sheet.rowId);
        }
        sheet.decided();
    }
}
