// "Make a rule…" for a connection whose prompt was put off (prompt-slot plan
// Part C, item 9), or that the firewall's default action decided (E3): the
// decision sheet's scope and remembered durations, sent
// as one rule (ConnectionsModel.makeRule, Rust `make_rule.rs`) until the rule
// editor exists. A once-only answer can't be given afterwards, so "This time"
// isn't offered. Every text here is fixed, and what happened comes only from
// MakeRuleController: "created" only once the bridge's result is Ok (PR #108
// security review, M1). The controller is the window's (main.qml), so a
// request outlives this page; MakeRuleOutcomes.qml polls it and gives the
// fixed off-screen notice.
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
    // E3: whether the firewall may list this put-off connection again as
    // decided by its default action (`outcome::may_be_listed_again`); the
    // bridge can't match the two rows (plan 2026-10-08-default-applied-events.md).
    property bool alsoListedByDefault: false
    // Whether "Decide later" blocked the program for 5 minutes. That block
    // stays: a matching deny wins over an allow rule, so the sheet says so
    // rather than delete it (PR #98 review).
    property bool blockedForFiveMinutes: false
    // The window's MakeRuleController (main.qml); null in probes without one.
    property var controller: null
    // What the last request for this row says: sending, its outcome, or that
    // it couldn't be sent. Empty for another row.
    readonly property string result: !!sheet.controller && sheet.controller.rowId === sheet.rowId
        ? sheet.controller.statusText : ""
    // Exposed for the headless probes (tests/deferred_rows_qml.rs,
    // tests/default_action_rows_qml.rs).
    property alias openButton: openButton
    property alias form: form
    property alias blockNote: blockNoteLabel
    property alias alsoListedNote: alsoListedNote
    property alias busyElsewhereNote: busyElsewhereNote

    onRowIdChanged: form.visible = false

    // Whether `rowId`'s outcome shows here now: the inspector is open on it.
    function shows(rowId) {
        return sheet.visible && sheet.rowId === rowId;
    }

    Controls.Label {
        id: alsoListedNote
        Layout.fillWidth: true
        visible: sheet.alsoListedByDefault
        wrapMode: Text.Wrap
        opacity: 0.7
        textFormat: Text.PlainText
        text: "The firewall may also list this connection, and its retries, separately "
            + "as decided by its default action."
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
                enabled: !!sheet.controller && !sheet.controller.busy
                text: "Allow"
                icon.name: "dialog-ok-apply"
                onClicked: sheet.make("allow")
            }
            Controls.Button {
                Layout.fillWidth: true
                enabled: !!sheet.controller && !sheet.controller.busy
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
        id: busyElsewhereNote
        objectName: "makeRuleBusyElsewhere"
        Layout.fillWidth: true
        visible: form.visible && sheet.bindableProcessPath && !!sheet.controller && sheet.controller.busy && sheet.controller.rowId !== sheet.rowId
        wrapMode: Text.Wrap
        opacity: 0.7
        textFormat: Text.PlainText
        text: "Another rule is still being sent. Try again in a moment."
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
        if (!sheet.controller) {
            return;
        }
        const requestId = sheet.controller.begin(sheet.rowId);
        if (requestId === "") {
            return;
        }
        if (sheet.model === null) {
            sheet.controller.notSent("");
            return;
        }
        // Empty when sent; otherwise why not.
        const problem = sheet.model.makeRule(sheet.rowId, choice, scopeBox.currentValue,
                                             durationBox.currentValue, requestId);
        if (problem !== "") {
            sheet.controller.notSent(problem);
            return;
        }
        form.visible = false;
    }
}
