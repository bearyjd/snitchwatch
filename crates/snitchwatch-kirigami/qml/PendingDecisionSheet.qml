// Pending-decision surface (Task 7).
//
// The safety-critical allow/deny controls for a novel connection. Embedded in
// the Connections inspector for a pending row; reusable as a standalone content
// block. The four verdict buttons (allow/deny × once/always) and the scope
// selector map — in Rust (`pending_decision.rs`), not here — onto the bridge's
// typed `ClientMessage::SetVerdict`. This QML only collects the choice.
//
// Timeout ownership: `remainingSeconds` is *displayed* only; the auto-action
// countdown is owned server-side by the bridge's AskRule machinery. This
// component never runs its own timer.
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
    // Server-owned countdown to the automatic fallback action. Negative hides it.
    property int remainingSeconds: -1

    // Emitted after a verdict is submitted so the container can close/advance.
    signal decided()

    // Rust submission surface (pure verdict→ClientMessage mapping lives there).
    PendingDecision {
        id: decision
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

    // Countdown display only — never a client-side timer.
    Controls.Label {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        opacity: 0.7
        visible: sheet.remainingSeconds >= 0
        text: "Auto-action in " + sheet.remainingSeconds + "s"
    }

    GridLayout {
        Layout.fillWidth: true
        columns: 2
        rowSpacing: Kirigami.Units.smallSpacing
        columnSpacing: Kirigami.Units.smallSpacing

        Controls.Button {
            Layout.fillWidth: true
            text: "Allow once"
            icon.name: "dialog-ok"
            onClicked: sheet.submit("allow_once")
        }
        Controls.Button {
            Layout.fillWidth: true
            text: "Deny once"
            icon.name: "dialog-cancel"
            onClicked: sheet.submit("deny_once")
        }
        Controls.Button {
            Layout.fillWidth: true
            text: "Allow always"
            icon.name: "dialog-ok-apply"
            onClicked: sheet.submit("allow_always")
        }
        Controls.Button {
            Layout.fillWidth: true
            text: "Deny always"
            icon.name: "edit-delete-remove"
            onClicked: sheet.submit("deny_always")
        }
    }

    function submit(choice) {
        decision.submit(sheet.rowId, choice, scopeBox.currentValue);
        sheet.decided();
    }
}
