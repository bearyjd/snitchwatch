// The rule button on a tree header row (Phase 2 step 4): shows whether a
// rule covers this program, domain or host, and offers to make or remove one.
//
// It sends nothing. `ruleState` is the `levelRuleState` role (`none`, `allow`,
// `deny`, `partial`, `pending`, `unavailable`; `ruleReason` is the last one's
// why). A click opens a menu; picking from it only emits `createRequested` or
// `removeRequested` for the owner (LevelRuleActions.qml) to carry out and
// report on. Both carry a copy of the node (level, program, key, label) taken
// when the menu opened: the list reuses its delegates, so what the row shows
// later can be another node, and the pick must act on the one that was
// clicked. Colour is never the only cue: every state has its own glyph and a
// text label, and the tooltip and Accessible.name spell out the state and the
// level.
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
    // `levelRuleReason` role: why `unavailable` (fixed text).
    property string ruleReason: ""
    // `groupLevel` of the header: 0 program, 1 domain, 2 host.
    property int level: 0
    // The node this button is on: the program path, the domain or host key
    // ("" for a program) and the label to name it by.
    property string program: ""
    property string nodeKey: ""
    property string nodeLabel: ""

    signal createRequested(int level, string program, string key, string label, string choice)
    signal removeRequested(int level, string program, string key, string label)

    // The node as it was when the open menu was opened, or null.
    property var node: null
    // Rows use a page-owned popup; standalone buttons keep their local menu.
    property var menuActions: null
    property var menuRow: null

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
                   enabled: false, tip: "Waiting for the firewall" },
        unavailable: { color: Kirigami.Theme.disabledTextColor, glyph: "⊘", label: "Can't add rule",
                       enabled: false,
                       tip: "A rule can't be added for this " + root.levelName
                            + (root.ruleReason !== "" ? ": " + root.ruleReason : ".") }
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

    // Sized to its widest steady label ("Can't add rule"), the same for every
    // state so a column of buttons lines up. "Waiting for the firewall" is
    // longer and elides; its tooltip and Accessible.name say it in full.
    TextMetrics {
        id: widest
        font: Kirigami.Theme.smallFont
        text: "Can't add rule"
    }
    TextMetrics {
        id: glyphMetrics
        font.bold: true
        text: "◷"
    }
    implicitWidth: button.leftPadding + button.rightPadding + glyphMetrics.width
        + Kirigami.Units.smallSpacing + Math.ceil(widest.width)
    implicitHeight: button.implicitHeight

    function openMenu(menu) {
        if (root.menuActions) {
            root.menuActions.openRule(root.menuRow, button);
            return;
        }
        root.node = { level: root.level, program: root.program, key: root.nodeKey, label: root.nodeLabel };
        menu.popup(button, 0, button.height);
    }

    // For a row that is being reused: nothing may be picked afterwards.
    function closeMenus() {
        chooseMenu.close();
        removeMenu.close();
        root.node = null;
    }

    function pick(choice) {
        const n = root.node;
        if (n !== null) {
            root.createRequested(n.level, n.program, n.key, n.label, choice);
        }
    }

    Controls.Button {
        id: button
        objectName: "ruleButton"
        flat: true
        anchors.fill: parent
        enabled: root.actionable
        Accessible.name: "Rule at " + root.levelName + " level: " + root.stateLabel
        Accessible.description: root.tipText
        onClicked: root.openMenu(root.ruleState === "none" ? chooseMenu : removeMenu)

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
                Layout.fillWidth: true
                elide: Text.ElideRight
                textFormat: Text.PlainText
                text: root.stateLabel
                color: root.stateColor
                font: Kirigami.Theme.smallFont
            }
        }
    }

    Controls.Menu {
        id: chooseMenu
        popupType: Controls.Popup.Item
        Controls.MenuItem {
            text: "Allow"
            icon.name: "dialog-ok-apply"
            onTriggered: root.pick("allow")
        }
        Controls.MenuItem {
            text: "Deny"
            icon.name: "edit-delete-remove"
            onTriggered: root.pick("deny")
        }
    }
    Controls.Menu {
        id: removeMenu
        popupType: Controls.Popup.Item
        Controls.MenuItem {
            text: "Remove rule"
            icon.name: "edit-delete"
            onTriggered: {
                const n = root.node;
                if (n !== null) {
                    root.removeRequested(n.level, n.program, n.key, n.label);
                }
            }
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
