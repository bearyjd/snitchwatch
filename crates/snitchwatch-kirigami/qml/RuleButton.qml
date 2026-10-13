// The rule button on a tree header row (Phase 2 step 4): shows whether a
// rule covers this program, domain or host, and offers to make or remove one.
//
// It holds no state of its own and sends nothing. `ruleState` is the
// `levelRuleState` role (`none`, `allow`, `deny`, `partial`, `pending`); a
// click only emits `createRequested(choice)` or `removeRequested()` for the
// owner (LevelRuleActions.qml) to carry out and report on. Colour is never the
// only cue: every state has its own glyph and a text label, and the tooltip and
// Accessible.name spell out the state and the level.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: root

    // `levelRuleState` role.
    property string ruleState: "none"
    // `levelRuleName` role: the rule's name (data), allow/deny/pending only.
    property string ruleName: ""
    // `groupLevel` of the header: 0 program, 1 domain, 2 host.
    property int level: 0

    signal createRequested(string choice)
    signal removeRequested

    readonly property string levelName: root.level === 0 ? "program"
        : root.level === 1 ? "domain" : "host"

    // The state table: one row per `levelRuleState`.
    readonly property var table: ({
        allow: { color: Theme.allow, glyph: "✓", label: "Allowed", enabled: true,
                 tip: "A rule allows this " + root.levelName + ". Click to remove it." },
        deny: { color: Theme.deny, glyph: "✕", label: "Denied", enabled: true,
                tip: "A rule denies this " + root.levelName + ". Click to remove it." },
        partial: { color: Theme.partial, glyph: "◐", label: "Partial", enabled: false,
                   tip: "Rules cover only part of this " + root.levelName
                        + ". Manage them on the Rules page." },
        none: { color: Theme.needsRule, glyph: "+", label: "Needs rule", enabled: true,
                tip: "No rule for this " + root.levelName + " yet. Click to allow or deny." },
        pending: { color: Theme.pending, glyph: "◷", label: "Waiting for the firewall",
                   enabled: false, tip: "Waiting for the firewall" }
    })
    readonly property var entry: root.table[root.ruleState] !== undefined
        ? root.table[root.ruleState] : root.table.none
    readonly property color stateColor: root.entry.color
    readonly property string glyph: root.entry.glyph
    readonly property string stateLabel: root.entry.label
    readonly property string tipText: root.entry.tip
    readonly property bool actionable: root.entry.enabled

    // Exposed for the headless probes.
    property alias button: button
    property alias chooseMenu: chooseMenu
    property alias removeMenu: removeMenu

    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight

    Controls.Button {
        id: button
        objectName: "ruleButton"
        flat: true
        anchors.fill: parent
        enabled: root.actionable
        Accessible.name: "Rule at " + root.levelName + " level: " + root.stateLabel
        Accessible.description: root.tipText
        onClicked: (root.ruleState === "none" ? chooseMenu : removeMenu).popup(button, 0, button.height)

        contentItem: RowLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label {
                objectName: "ruleGlyph"
                textFormat: Text.PlainText
                text: root.glyph
                color: root.stateColor
                font.bold: true
            }
            Controls.Label {
                objectName: "ruleLabel"
                textFormat: Text.PlainText
                text: root.stateLabel
                color: root.stateColor
                font: Kirigami.Theme.smallFont
            }
        }
    }

    Controls.Menu {
        id: chooseMenu
        Controls.MenuItem {
            text: "Allow"
            icon.name: "dialog-ok-apply"
            onTriggered: root.createRequested("allow")
        }
        Controls.MenuItem {
            text: "Deny"
            icon.name: "edit-delete-remove"
            onTriggered: root.createRequested("deny")
        }
    }
    Controls.Menu {
        id: removeMenu
        Controls.MenuItem {
            text: "Remove rule"
            icon.name: "edit-delete"
            onTriggered: root.removeRequested()
        }
    }

    // On this wrapper, not the button: a disabled button gets no hover, and
    // the disabled states are the ones whose reason the tooltip gives.
    HoverHandler { id: hover }
    Controls.ToolTip {
        visible: hover.hovered
        delay: Kirigami.Units.toolTipDelay
        contentItem: Controls.Label {
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: palette.toolTipText
            text: root.tipText
        }
    }
}
