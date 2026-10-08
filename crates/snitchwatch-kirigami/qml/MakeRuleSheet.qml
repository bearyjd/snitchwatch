// "Make a rule…" for a connection whose prompt was put off (prompt-slot plan
// Part C, item 9), or that the firewall's default action decided (E3): the
// decision sheet's scope and remembered durations, sent
// as one rule (ConnectionsModel.makeRule, Rust `make_rule.rs`) until the rule
// editor exists. A once-only answer can't be given afterwards, so "This time"
// isn't offered. Every text here is fixed, and what happened comes only from
// MakeRuleController: "created" only once the bridge's result is Ok (PR #108
// security review, M1).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: sheet
    spacing: Kirigami.Units.smallSpacing

    property string rowId: ""
    property var model: null
    // Whether the row's program has a file a rule can be bound to
    // (`rowDetailsJson`'s `bindableProcessPath`, issue #44).
    property bool bindableProcessPath: false
    // Whether "Decide later" blocked the program for 5 minutes. That block
    // stays: a matching deny wins over an allow rule, so the sheet says so
    // rather than delete it (PR #98 review).
    property bool blockedForFiveMinutes: false
    // What the last request for this row says: sending, its outcome, or that
    // it couldn't be sent. Empty for another row.
    readonly property string result: controller.rowId === sheet.rowId ? controller.statusText : ""
    // Exposed for the headless probes (tests/deferred_rows_qml.rs,
    // tests/default_action_rows_qml.rs).
    property alias openButton: openButton
    property alias form: form
    property alias blockNote: blockNoteLabel
    property alias controller: controller

    onRowIdChanged: form.visible = false

    MakeRuleController {
        id: controller
        Component.onCompleted: startBridgeFeed()
    }

    // The bridge's result never came: give up after a silence.
    Timer {
        interval: 1000
        repeat: true
        running: controller.busy
        onTriggered: controller.poll()
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
                enabled: !controller.busy
                text: "Allow"
                icon.name: "dialog-ok-apply"
                onClicked: sheet.make("allow")
            }
            Controls.Button {
                Layout.fillWidth: true
                enabled: !controller.busy
                text: "Deny"
                icon.name: "edit-delete-remove"
                onClicked: sheet.make("deny")
            }
        }
    }

    Controls.Label {
        id: blockNoteLabel
        Layout.fillWidth: true
        visible: form.visible && sheet.blockedForFiveMinutes
        wrapMode: Text.Wrap
        opacity: 0.7
        textFormat: Text.PlainText
        text: "This program's 5-minute block stays until it ends. Until then it wins over an Allow rule."
    }

    Controls.Label {
        objectName: "makeRuleResult"
        Layout.fillWidth: true
        visible: sheet.result.length > 0
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: sheet.result
    }

    function make(choice) {
        const requestId = controller.begin(sheet.rowId);
        if (requestId === "") {
            return;
        }
        const sent = sheet.model !== null
            && sheet.model.makeRule(sheet.rowId, choice, scopeBox.currentValue,
                                    durationBox.currentValue, requestId) === true;
        if (!sent) {
            controller.notSent();
            return;
        }
        form.visible = false;
    }
}
