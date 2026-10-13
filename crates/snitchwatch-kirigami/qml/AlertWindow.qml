// The standalone connection alert (LS redesign phase 1): a small always-on-top
// dialog, separate from the main window, for the OLDEST connection still
// waiting for an answer. It follows the Little Snitch interaction model
// (layout and behaviour only): identity first (icon, program, path), then the
// destination, then scope and duration as segmented choices, then Deny/Allow.
//
// * Scope: Any host / Any host on this domain / This host only (default). As
//   in PendingDecisionSheet, a remembered answer is offered only where the
//   bridge can bind a rule to the program (#44) and, for the host scopes, its
//   session advertised app-bound rules (#72); otherwise only "Once".
// * Duration: `Durations.all`, preselected from the Settings default.
// * Return = Allow, Esc = Deny. Every key and click is ignored for
//   `armDelayMs` after the alert appears, after it moves on to the next row,
//   and when what it offers changes under the pointer (the durations appear
//   or vanish with the scope), so a keystroke or click meant for another
//   window or layout cannot answer it. A key held down across the arming
//   moment, and any auto-repeat, never answers: only a fresh press does.
// * Closing the window ignores the prompt (the firewall's default applies at
//   the deadline); only an answer sets a rule duration.
// * The answer goes through `BridgeFeed.submitVerdict`, whose Rust gate
//   (`limit_to_bridge`) sends a remembered verdict once-only when the program
//   cannot be bound. The countdown only displays the bridge's 30 s deadline
//   (`answerDeadlineMs`); the bridge, not this window, answers at the end.
//
// Shown while `active` (AlertHost.qml: silent mode off, "Ask me"), whether the
// main window is visible or hidden to the tray: the alert is its own window.
// In silent Allow/Deny `silent_mode.rs` answers new prompts instead. Program,
// path and host come from outside: every such text is PlainText (issue #51).
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Window {
    id: alert

    // Injected by main.qml (null in isolated tests).
    property var model: null
    property var bridgeFeed: null
    // Whether the alert may be shown at all.
    property bool active: true
    // Keys and clicks are ignored for this long after the alert (re)appears.
    property int armDelayMs: 1000

    // Why an answer wasn't sent, for the host to show (fixed text).
    signal explained(string text)

    // The row shown (`ConnectionsModel.alertRowJson`) and what is derived.
    property var row: ({})
    readonly property string rowId: alert.row.rowId !== undefined ? alert.row.rowId : ""
    readonly property bool bindable: alert.row.bindableProcessPath === true
    property bool appBoundRules: false
    readonly property bool remembers: alert.bindable
        && (alert.appBoundRules || scopeChoice.effectiveToken === "any_host")
    readonly property string scopeToken: scopeChoice.effectiveToken
    readonly property string durationToken: alert.remembers
        ? durationChoice.effectiveToken : "this_time"
    // False for `armDelayMs` after the row appears; nothing is answerable then.
    property bool armed: false
    // Keys pressed while not armed and not yet released: a press of one of
    // these (some platforms do not flag the repeats) is not a fresh press.
    property var heldKeys: ({})
    // A row answered or dismissed here and not yet resolved by the bridge is
    // not shown again.
    property string handledRowId: ""
    // When the bridge answers by itself (Unix ms, -1 never); `nowMs` is
    // refreshed by the display-only tick.
    property real nowMs: Date.now()
    readonly property real deadlineMs: alert.row.answerDeadlineMs !== undefined
        ? alert.row.answerDeadlineMs : -1
    readonly property int remainingSeconds: alert.deadlineMs > 0
        ? Math.max(0, Math.ceil((alert.deadlineMs - alert.nowMs) / 1000)) : -1

    // Exposed for the headless probes.
    property alias scopeSelector: scopeChoice
    property alias durationSelector: durationChoice
    property alias allowButton: allowButton
    property alias denyButton: denyButton
    property alias keyCatcher: keys

    // What the destination line says. An outgoing connection is the program
    // reaching out; an incoming one is a peer reaching in, so it is not worded
    // as the program connecting.
    readonly property string destinationText: {
        if (alert.rowId === "") {
            return "";
        }
        const addr = alert.row.host ? alert.row.host : alert.row.dstIp;
        if (alert.row.direction === "incoming") {
            return "Incoming connection from " + addr + " (" + alert.row.protocol + ")";
        }
        return "wants to connect to " + addr + ":" + alert.row.port + " (" + alert.row.protocol + ")";
    }

    readonly property bool shown: alert.active && alert.rowId !== ""
        && alert.rowId !== alert.handledRowId

    visible: alert.shown
    flags: Qt.Dialog | Qt.WindowStaysOnTopHint
    title: "Snitchwatch: connection request"
    color: Kirigami.Theme.backgroundColor
    width: Kirigami.Units.gridUnit * 34
    // Fixed size: the content's own height, not the window's.
    readonly property int fixedHeight: content.implicitHeight + Theme.spacingLarge * 2
    height: alert.fixedHeight
    minimumWidth: width
    maximumWidth: width
    minimumHeight: alert.fixedHeight
    maximumHeight: alert.fixedHeight

    // Closing the alert (window manager) ignores this prompt: the bridge
    // applies its default at the deadline. It must not veto the close: an
    // application quit (`Qt.quit()`) closes every window first.
    onClosing: alert.handledRowId = alert.rowId

    // Re-read the oldest pending row. A different row (or none) re-arms the
    // delay and resets the choices to the Settings default.
    function refresh() {
        let next = ({});
        if (alert.model !== null) {
            try {
                next = JSON.parse(alert.model.alertRowJson());
            } catch (e) {
                console.warn("AlertWindow.refresh failed:", e);
            }
        }
        const changed = (next.rowId || "") !== alert.rowId;
        alert.row = next;
        if (alert.handledRowId !== "" && alert.rowId !== alert.handledRowId) {
            alert.handledRowId = "";
        }
        alert.appBoundRules = alert.rowId !== "" && alert.bridgeFeed !== null
            && typeof alert.bridgeFeed.appBoundRulesFor === "function"
            && alert.bridgeFeed.appBoundRulesFor(alert.rowId) === true;
        if (changed) {
            alert.nowMs = Date.now();
            scopeChoice.currentToken = "this_host";
            durationChoice.currentToken = Durations.defaultToken;
            alert.arm();
        }
    }

    function arm() {
        alert.armed = false;
        if (alert.rowId !== "") {
            armTimer.restart();
        }
    }

    // What is offered changed (a remembered duration appears or goes), so the
    // buttons may have moved under the pointer: wait again.
    onRemembersChanged: alert.arm()

    onShownChanged: if (alert.shown) {
        alert.heldKeys = ({});
        alert.arm();
        alert.raise();
        // Return/Esc need the keyboard focus; a compositor may still refuse.
        alert.requestActivate();
    }

    // Sends `choice` ("allow" or "deny") for the row shown. Ignored until
    // armed. Returns whether a verdict was queued.
    function answer(choice) {
        if (!alert.armed || !alert.shown || alert.bridgeFeed === null) {
            return false;
        }
        const rowId = alert.rowId;
        const queued = alert.bridgeFeed.submitVerdict(rowId, choice, alert.scopeToken,
                                                      alert.durationToken, alert.bindable);
        if (queued === false) {
            alert.explained("The connection to the background service was lost, so this answer wasn't sent.");
            alert.arm();
            return false;
        }
        alert.handledRowId = rowId;
        return true;
    }

    // Return = Allow, Esc = Deny; every other key, any key before the alert
    // is armed, an auto-repeat, and a key still held from before it was armed,
    // does nothing (and is not passed on).
    function handleKey(key, autoRepeat) {
        if (!alert.armed) {
            alert.heldKeys[key] = true;
            return;
        }
        if (autoRepeat === true || alert.heldKeys[key] === true) {
            return;
        }
        if (key === Qt.Key_Return || key === Qt.Key_Enter) {
            alert.answer("allow");
        } else if (key === Qt.Key_Escape) {
            alert.answer("deny");
        }
    }

    // A released key is no longer "held since before arming".
    function keyReleased(key) {
        delete alert.heldKeys[key];
    }

    Timer {
        id: armTimer
        interval: alert.armDelayMs
        repeat: false
        onTriggered: alert.armed = true
    }

    Connections {
        target: alert.model
        ignoreUnknownSignals: true
        function onPendingCountChanged() { alert.refresh(); }
    }

    // Display-only: the countdown, and a refresh for the cases a count change
    // does not signal (the oldest row resolved as another arrived).
    Timer {
        interval: 1000
        repeat: true
        running: alert.active
        triggeredOnStart: true
        onTriggered: {
            alert.nowMs = Date.now();
            alert.refresh();
        }
    }

    FocusScope {
        id: keys
        anchors.fill: parent
        focus: true

        Keys.onPressed: event => {
            event.accepted = true;
            alert.handleKey(event.key, event.isAutoRepeat);
        }
        Keys.onReleased: event => {
            event.accepted = true;
            alert.keyReleased(event.key);
        }

        ColumnLayout {
            id: content
            anchors.fill: parent
            anchors.margins: Theme.spacingLarge
            spacing: Theme.spacingMedium
            // Clicks, like keys, do nothing until armed.
            enabled: alert.armed

            // Identity first: icon, program, path.
            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingMedium

                Kirigami.Icon {
                    objectName: "alertIcon"
                    source: alert.row.appIcon !== undefined ? alert.row.appIcon : ""
                    Layout.preferredWidth: Kirigami.Units.iconSizes.large
                    Layout.preferredHeight: Kirigami.Units.iconSizes.large
                    Layout.alignment: Qt.AlignTop
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        objectName: "alertProgram"
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        font.bold: true
                        font.pixelSize: Math.round(Theme.basePixelSize * Theme.scaleTitle)
                        textFormat: Text.PlainText
                        text: alert.row.process !== undefined ? alert.row.process : ""
                    }
                    Controls.Label {
                        objectName: "alertPath"
                        Layout.fillWidth: true
                        elide: Text.ElideMiddle
                        opacity: 0.7
                        font.pixelSize: Math.round(Theme.basePixelSize * Theme.scaleSmall)
                        textFormat: Text.PlainText
                        text: alert.row.processPath ? alert.row.processPath : "Program file unknown"
                    }
                }
            }

            // Then the destination.
            Controls.Label {
                objectName: "alertDestination"
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                text: alert.destinationText
            }

            Controls.Label {
                text: "Applies to"
                opacity: 0.7
            }
            SegmentedChoice {
                id: scopeChoice
                Layout.fillWidth: true
                currentToken: "this_host"
                model: [
                    { label: "Any host", token: "any_host" },
                    { label: "Any host on this domain", token: "any_host_on_domain" },
                    { label: "This host only", token: "this_host" }
                ]
            }

            Controls.Label {
                text: "For how long"
                opacity: 0.7
            }
            SegmentedChoice {
                id: durationChoice
                Layout.fillWidth: true
                currentToken: Durations.defaultToken
                model: alert.remembers ? Durations.all : [Durations.all[0]]
            }

            // Why only "Once" is offered (the sheet's sentences).
            Controls.Label {
                objectName: "alertOnceOnlyNote"
                Layout.fillWidth: true
                visible: !alert.remembers
                wrapMode: Text.Wrap
                opacity: 0.7
                font.pixelSize: Math.round(Theme.basePixelSize * Theme.scaleSmall)
                textFormat: Text.PlainText
                text: !alert.bindable
                    ? "Snitchwatch couldn't identify this program's file, so this answer applies only to this connection."
                    : "This firewall bridge is too old to limit a rule to just this program, so with this scope it can only answer this connection. Update Snitchwatch's background service to remember answers."
            }

            Controls.Label {
                objectName: "alertCountdown"
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                opacity: 0.7
                visible: alert.remainingSeconds >= 0
                textFormat: Text.PlainText
                text: "If nobody answers within " + alert.remainingSeconds
                    + " s, the firewall's default action applies."
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingSmall

                Controls.Button {
                    id: denyButton
                    objectName: "alertDeny"
                    Layout.fillWidth: true
                    text: "Deny"
                    icon.name: "edit-delete-remove"
                    palette.buttonText: Theme.deny
                    onClicked: alert.answer("deny")
                }
                Controls.Button {
                    id: allowButton
                    objectName: "alertAllow"
                    Layout.fillWidth: true
                    text: "Allow"
                    icon.name: "dialog-ok-apply"
                    palette.buttonText: Theme.allow
                    onClicked: alert.answer("allow")
                }
            }
        }
    }
}
