// Who holds opensnitchd's single prompt slot (plan
// docs/superpowers/plans/2026-10-08-prompt-slot-ux.md, part A, with the
// honesty half of issue #78). main.qml floats it over every page while the
// live bridge session reports a holder.
//
// The heading is fixed text: InlineMessage renders its text as markup (issue
// #51). The program, host, wait and count come from PromptSlotStatus.text, in
// a PlainText label.
//
// "Allow once" and "Deny" answer the holder with the Connections page's
// inline semantics (InlineVerdicts.qml). They are enabled only while the model
// holds the row as pending (the bridge announces the row before the holder,
// and grouped mode can buffer it), only once a holder has been on screen for
// `armDelayMs` (so a double-click meant for one prompt can't answer the next
// one that takes its place), and each holder is answered at most once.
// "Review" opens the Connections page, whose auto-select picks the row.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: banner
    spacing: 0

    // PromptSlotStatus, or a stand-in with supported/held/rowId/text.
    property var status: null
    property var model: null
    property var bridgeFeed: null

    // A once-only Deny's explanation, for the window to show.
    signal explained(string text)
    signal reviewRequested()

    // The holder this banner answered; another holder can be answered again.
    property string answeredRowId: ""
    // How long a holder must be shown before it can be answered. Restarted
    // whenever the holder changes or the banner reappears.
    property int armDelayMs: 750
    property bool armed: false
    readonly property string rowId: banner.status ? banner.status.rowId : ""
    readonly property bool shown: banner.status !== null && banner.status.supported === true
        && banner.status.held === true
        && !(banner.bridgeFeed !== null && banner.bridgeFeed.ok === false)
    // Reading `pendingCount` re-evaluates this as rows come and go.
    readonly property bool actionable: banner.shown && banner.armed
        && banner.answeredRowId !== banner.rowId
        && banner.model !== null && banner.model.pendingCount >= 0
        && banner.model.isPendingRow(banner.rowId)
    // Exposed for the headless probe (tests/prompt_slot_banner_qml.rs).
    property alias label: holderLabel

    visible: banner.shown

    onRowIdChanged: banner.rearm()
    onShownChanged: banner.rearm()
    Component.onCompleted: banner.rearm()

    function rearm() {
        banner.armed = false;
        if (banner.shown) {
            armTimer.restart();
        } else {
            armTimer.stop();
        }
    }

    Timer {
        id: armTimer
        interval: banner.armDelayMs
        repeat: false
        onTriggered: banner.armed = true
    }

    InlineVerdicts {
        id: verdicts
        model: banner.model
        bridgeFeed: banner.bridgeFeed
        onExplained: text => banner.explained(text)
    }

    function answer(choice) {
        if (!banner.actionable) {
            return;
        }
        banner.answeredRowId = banner.rowId;
        verdicts.submit(banner.rowId, choice);
    }

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        visible: true
        type: Kirigami.MessageType.Warning
        text: "A connection is waiting for your answer."
        actions: [
            Kirigami.Action {
                text: "Allow once"
                icon.name: "dialog-ok-apply"
                enabled: banner.actionable
                onTriggered: banner.answer("allow")
            },
            Kirigami.Action {
                text: "Deny"
                icon.name: "edit-delete-remove"
                enabled: banner.actionable
                onTriggered: banner.answer("deny")
            },
            Kirigami.Action {
                text: "Review"
                icon.name: "view-list-details"
                onTriggered: banner.reviewRequested()
            }
        ]
    }

    Rectangle {
        Layout.fillWidth: true
        implicitHeight: holderLabel.implicitHeight + Kirigami.Units.smallSpacing * 2
        color: Kirigami.Theme.backgroundColor

        Controls.Label {
            id: holderLabel
            anchors.fill: parent
            anchors.margins: Kirigami.Units.smallSpacing
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            text: banner.status ? banner.status.text : ""
        }
    }
}
