// One program of the review (learning mode, phase L4b). The 0a label with its
// "?" marker and real path comes from ProgramLabel; everything else is facts.
// Every recorded string (name, path, host, flag) is data from outside, so
// every label is PlainText and every CheckBox/Button carries fixed text only
// (a CheckBox's own text would render as markup): the choice's title sits in
// a PlainText label beside a text-less CheckBox.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Rectangle {
    id: row

    // One element of the controller's rowsJson.
    property var info: ({})
    // The kind ticked on this row (a token), or "".
    property string selected: ""
    // Sets a tick: (key, kind or "") and reads a row's destinations.
    property var controller: null
    property bool locked: false
    // The busiest destinations of a hidden list, fetched on demand.
    property var more: []
    readonly property bool hasChoices: row.info.choices !== undefined && row.info.choices.length > 0
    property alias notSelectableLabel: notSelectable

    implicitHeight: body.implicitHeight + Kirigami.Units.largeSpacing * 2
    color: row.selected !== "" ? Qt.alpha(Kirigami.Theme.highlightColor, 0.12) : "transparent"
    border.color: Kirigami.Theme.disabledTextColor
    border.width: 1
    radius: 3

    readonly property string countsText: row.info.connections
        + (row.info.connections === 1 ? " connection to " : " connections to ")
        + row.info.distinct + (row.info.distinctExact ? "" : " or more")
        + (row.info.distinct === 1 ? " destination" : " destinations")
        + ". Last seen " + new Date(row.info.lastSeenMs).toLocaleString() + "."

    // The warnings and per-choice lines, shared by their Labels and the row's
    // Copy text so the two cannot drift. "" = not shown.
    readonly property string dualUseText: row.info.dualUse === true
        ? "This program can run other programs or scripts." : ""
    readonly property string highFanOutText: row.info.highFanOut === true
        ? "It connected to " + row.info.distinct + " or more destinations, so no destination list is chosen for it." : ""
    readonly property string alreadyPresentText: row.info.alreadyPresent !== undefined && row.info.alreadyPresent.length > 0
        ? "A rule you already have makes: " + row.present(row.info.alreadyPresent) + "." : ""

    function hostLine(h) {
        return h.host + "  (" + h.count + ")";
    }
    function onlyText(c) {
        return c.hosts.length > 0 ? "Only: " + c.hosts.join(", ") : "";
    }
    function noteText(c) {
        return c.note === null || c.note === undefined ? "" : c.note;
    }

    function present(kinds) {
        const names = {
            "allow-any": "any destination",
            "allow-hosts": "only these destinations",
            "block": "block this program",
            "directory": "any program file in a folder"
        };
        return kinds.map(k => names[k] || "a choice").join(", ");
    }

    // Right-click Copy of the whole program (mouse only: no focus, no Tab
    // stop). The labels stay Labels: a second per-label menu would open
    // beside this one, and a drag-select would fight the list's flick.
    RowCopyMenu {
        copyText: {
            const i = row.info;
            const lines = [i.programLabel + (i.programLabelUnverified ? " (name unverified)" : ""),
                           i.programPathFull === undefined ? "" : i.programPathFull,
                           row.countsText];
            lines.push(...(i.flags === undefined ? [] : i.flags));
            const hosts = row.more.length > 0 ? row.more : (i.top === undefined ? [] : i.top);
            lines.push(row.dualUseText, row.highFanOutText);
            lines.push(...hosts.map(h => row.hostLine(h)));
            if (i.notSelectable !== undefined && i.notSelectable !== null) {
                lines.push(i.notSelectable);
            }
            lines.push(row.alreadyPresentText);
            for (const c of (i.choices === undefined ? [] : i.choices)) {
                lines.push(c.title, row.onlyText(c), row.noteText(c));
            }
            return lines.filter(l => l !== "").join("\n");
        }
    }

    ColumnLayout {
        id: body
        anchors {
            left: parent.left
            right: parent.right
            top: parent.top
            margins: Kirigami.Units.largeSpacing
        }
        spacing: Kirigami.Units.smallSpacing

        ProgramLabel {
            src: row.info
            bold: true
            Layout.fillWidth: true
        }
        Controls.Label {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WrapAnywhere
            opacity: 0.7
            font: Kirigami.Theme.smallFont
            text: row.info.programPathFull === undefined ? "" : row.info.programPathFull
        }
        Controls.Label {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: row.countsText
        }

        Repeater {
            model: row.info.flags === undefined ? [] : row.info.flags
            delegate: Controls.Label {
                required property string modelData
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.WrapAnywhere
                font.bold: true
                text: modelData
            }
        }
        Controls.Label {
            Layout.fillWidth: true
            visible: row.dualUseText !== ""
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: row.dualUseText
        }
        Controls.Label {
            Layout.fillWidth: true
            visible: row.highFanOutText !== ""
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: row.highFanOutText
        }

        Repeater {
            model: row.more.length > 0 ? row.more : (row.info.top === undefined ? [] : row.info.top)
            delegate: Controls.Label {
                required property var modelData
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.WrapAnywhere
                text: row.hostLine(modelData)
            }
        }
        Controls.Button {
            visible: row.info.destinationsHidden === true && row.more.length === 0
            text: "Show the busiest destinations"
            onClicked: row.more = JSON.parse(row.controller.destinations(row.info.key)).slice(0, 25)
        }

        Controls.Label {
            id: notSelectable
            Layout.fillWidth: true
            visible: row.info.notSelectable !== undefined && row.info.notSelectable !== null
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: visible ? row.info.notSelectable : ""
        }
        Controls.Label {
            Layout.fillWidth: true
            visible: row.alreadyPresentText !== ""
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: row.alreadyPresentText
        }

        Repeater {
            model: row.info.choices === undefined ? [] : row.info.choices
            delegate: ColumnLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: 0

                RowLayout {
                    Layout.fillWidth: true
                    Controls.CheckBox {
                        id: tickBox
                        objectName: "tick-" + modelData.kind
                        enabled: !row.locked
                        checked: row.selected === modelData.kind
                        Accessible.name: modelData.title + ": " + row.info.programLabel + " (" + row.info.programPathFull + ")"
                            + (row.info.programLabelUnverified ? ", name unverified" : "")
                        onToggled: {
                            row.controller.setTick(row.info.key, checked ? modelData.kind : "");
                            // The box shows the controller's state only: a
                            // refused tick must not stay drawn as ticked.
                            checked = Qt.binding(() => row.selected === modelData.kind);
                        }
                    }
                    Controls.Label {
                        Layout.fillWidth: true
                        textFormat: Text.PlainText
                        wrapMode: Text.WrapAnywhere
                        font.bold: modelData.kind === "block"
                        text: modelData.title
                        // Clicking the words ticks the box.
                        TapHandler {
                            enabled: !row.locked
                            onTapped: {
                                tickBox.toggle();
                                tickBox.toggled();
                            }
                        }
                    }
                }
                Controls.Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: Kirigami.Units.gridUnit * 2
                    visible: row.onlyText(modelData) !== ""
                    textFormat: Text.PlainText
                    wrapMode: Text.WrapAnywhere
                    text: row.onlyText(modelData)
                }
                Controls.Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: Kirigami.Units.gridUnit * 2
                    visible: modelData.note !== null && modelData.note !== undefined
                    textFormat: Text.PlainText
                    wrapMode: Text.WrapAnywhere
                    opacity: 0.8
                    text: row.noteText(modelData)
                }
            }
        }
    }
}
