// "Make a rule…" for a connection whose prompt was put off (prompt-slot plan
// Part C, item 9): the decision sheet's scope and remembered durations, sent
// as one rule (ConnectionsModel.makeRule, Rust `make_rule.rs`) until the rule
// editor exists. A once-only answer can't be given afterwards, so "This time"
// isn't offered. Every text here is fixed.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: sheet
    spacing: Kirigami.Units.smallSpacing

    property string rowId: ""
    property var model: null
    // Whether the row's program has a file a rule can be bound to
    // (`rowDetailsJson`'s `bindableProcessPath`, issue #44).
    property bool bindableProcessPath: false
    // What the last click did, for the label below.
    property string result: ""
    // Exposed for the headless probe (tests/deferred_rows_qml.rs).
    property alias openButton: openButton
    property alias form: form

    onRowIdChanged: {
        form.visible = false;
        sheet.result = "";
    }

    Controls.Button {
        id: openButton
        Layout.fillWidth: true
        visible: sheet.bindableProcessPath && !form.visible
        text: "Make a rule…"
        icon.name: "list-add"
        onClicked: form.visible = true
    }

    Controls.Label {
        Layout.fillWidth: true
        visible: !sheet.bindableProcessPath
        wrapMode: Text.Wrap
        opacity: 0.7
        textFormat: Text.PlainText
        text: "Snitchwatch couldn't identify this program's file, so it can't make a rule for it."
    }

    ColumnLayout {
        id: form
        Layout.fillWidth: true
        visible: false

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
                    { label: "For 5 minutes", token: "for_5_minutes" },
                    { label: "Until firewall restarts", token: "until_quit" },
                    { label: "Forever", token: "forever" }
                ]
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Controls.Button {
                Layout.fillWidth: true
                text: "Allow"
                icon.name: "dialog-ok-apply"
                onClicked: sheet.make("allow")
            }
            Controls.Button {
                Layout.fillWidth: true
                text: "Deny"
                icon.name: "edit-delete-remove"
                onClicked: sheet.make("deny")
            }
        }
    }

    Controls.Label {
        Layout.fillWidth: true
        visible: sheet.result.length > 0
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: sheet.result
    }

    function make(choice) {
        const sent = sheet.model !== null
            && sheet.model.makeRule(sheet.rowId, choice, scopeBox.currentValue,
                                    durationBox.currentValue) === true;
        sheet.result = sent
            ? "The rule was sent to the firewall service."
            : "The rule couldn't be sent.";
        if (sent) {
            form.visible = false;
        }
    }
}
