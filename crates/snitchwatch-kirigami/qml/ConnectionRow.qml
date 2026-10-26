// A Connections list delegate: connection or group header. Rust model roles
// arrive as required properties; clicks and verdicts go to the owning page.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: row

    // The owning ConnectionsPage (set by its delegate declaration).
    property var page
    readonly property var verdictHelper: row.page.inlineVerdicts
    width: ListView.view ? ListView.view.width : implicitWidth
    implicitHeight: content.implicitHeight + verdictCounts.height + Kirigami.Units.smallSpacing * 2

    required property int index
    required property string rowId
    required property string sourceSession
    required property string process
    required property string host
    required property int port
    required property string protocol
    required property string verdict
    required property bool pending

    // depth controls indentation; groupLevel: program=0, domain=1, host=2,
    // connection=3. Flat connections have depth 0 and isGroupHeader=false.
    required property int depth
    required property int groupLevel
    required property string groupProcessKey
    required property bool isGroupHeader
    required property bool expanded
    required property string groupKey
    required property string groupParentKey
    required property string groupLabel
    required property int groupTotal
    required property int groupPending
    required property int groupAllowed
    required property int groupDenied
    required property int groupBlocklisted
    required property string groupLastHour
    required property string groupToday
    // Rule button roles (headers only; "none" and "" elsewhere).
    required property string levelRuleState
    required property string levelRuleName
    required property string levelRuleReason

    // Rule-match diagnostics roles. Header rows report empty strings
    // for both (see `ConnectionsModel::grouped_entry_data`) since
    // they aren't a single decided connection.
    required property string matchedRule
    required property string matchedRuleDisplay
    // Allowed once because filtering was paused (issue #78); false on headers.
    required property bool answeredWhilePaused
    // Prompt-slot plan Part C: a put-off or default-decided row's
    // label (empty otherwise), when the bridge answers a pending one
    // (-1: never), and whether "Make a rule…" is offered (put off,
    // or decided by the firewall's default action).
    required property string outcomeText
    required property real answerDeadlineMs
    required property bool makeRuleOffered
    // Icon from the process's .desktop file (empty on headers); answered
    // silently once-only, so it needs a rule (`silent_mode.rs`).
    required property string appIcon
    required property bool needsRule
    // Issue #55: daemon events this row stands for, and the last one's time.
    required property int repeatCount
    required property int retriedCount // 0d part 2: notices held back, newest pending row only
    required property real lastSeenMs
    required property string programPathFull
    required property string programLabel
    required property bool programLabelUnverified
    required property string programPathTail
    // Issue #18 double-submit guard: the buttons stay until `pending` flips,
    // so a second click would send a second SetVerdict. Reset when the
    // delegate shows another row (reuseItems: `rowId` changes) or the row's
    // pending state changes.
    property bool submitted: false
    onRowIdChanged: row.submitted = false
    onPendingChanged: row.submitted = false
    onGroupPendingChanged: row.submitted = false
    onGroupKeyChanged: row.submitted = false
    onGroupLevelChanged: row.submitted = false
    onGroupParentKeyChanged: row.submitted = false
    onGroupProcessKeyChanged: row.submitted = false
    onSourceSessionChanged: row.submitted = false

    // What this row's Deny, or this header's "Deny all", does: the
    // buttons' tooltip and accessible description. Empty where the
    // button is hidden, so other delegates never look it up.
    readonly property string denyText: !row.isGroupHeader && row.pending
        ? verdictHelper.denyText(row.rowId) : ""
    readonly property string denyAllText: row.isGroupHeader
        && row.groupPending > 0 ? verdictHelper.denyAllText(row.ruleProgram) : ""
    // The program a header's rule is about (only a host row carries it as
    // `groupProcessKey`) and the key below it ("" for a program).
    readonly property string ruleProgram: row.groupLevel === 0 ? row.groupKey
        : row.groupLevel === 1 ? row.groupParentKey : row.groupProcessKey
    readonly property string ruleKey: row.groupLevel === 0 ? "" : row.groupKey
    Connections {
        target: row.ListView
        function onPooled() {
            row.submitted = false;
        }
    }
    // Exposed for the headless probes.
    property alias ruleButton: headerRuleButton
    property alias denyButton: rowDenyButton
    property alias denyAllButton: batchDenyButton
    readonly property var actions: row.page.connectionActions

    // Share one submission latch across every verdict button. Plain onClicked
    // receives nested button clicks; adding TapHandler dispatches twice, as
    // measured by QtTest across Basic/Fusion/Material/Universal styles.
    readonly property bool decideLaterOffered: !row.isGroupHeader && row.pending
        && verdictHelper.rowDecideLater(row.rowId)

    function decideOnce(choice, batch, target) {
        if (row.submitted) {
            return;
        }
        const n = target || { rowId: row.rowId, program: row.ruleProgram,
            level: row.groupLevel, key: row.ruleKey, domain: row.groupParentKey,
            session: row.sourceSession };
        row.submitted = true;
        if (batch) {
            if (!page.submitGroupVerdict(n.program, n.level, n.key, n.domain,
                                         choice, n.session, true)) row.submitted = false;
        } else if (choice === "later") {
            // Part C "Decide later": not a verdict QML builds.
            verdictHelper.decideLater(n.rowId);
        } else {
            if (!page.submitInlineVerdict(n.rowId, choice, true)) row.submitted = false;
        }
    }

    // Deliberately behind `content`: leaf/header row clicks work in
    // the remaining space, while nested Buttons receive their own
    // pointer events instead of being swallowed by ItemDelegate.
    MouseArea {
        anchors.fill: parent
        z: 0
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        onClicked: mouse => {
            if (mouse.button === Qt.RightButton) {
                row.actions.open(row, mouse.x, mouse.y);
                return;
            }
            if (row.isGroupHeader) {
                if (row.groupLevel === 0) {
                    page.model.toggleProcessGroup(row.groupKey);
                } else if (row.groupLevel === 2) {
                    page.model.toggleHostGroup(row.groupProcessKey, row.groupParentKey, row.groupKey);
                } else {
                    page.model.toggleDomainGroup(row.groupParentKey, row.groupKey);
                }
                return;
            }
            row.ListView.view.currentIndex = row.index;
            page.openInspector(row);
        }
    }

    GroupVerdictCounts { id: verdictCounts; src: row }
    RowLayout {
        id: content
        anchors.fill: parent
        anchors.margins: Kirigami.Units.smallSpacing
        anchors.bottomMargin: Kirigami.Units.smallSpacing + verdictCounts.height
        anchors.rightMargin: Kirigami.Units.largeSpacing
        z: 1
        spacing: Kirigami.Units.largeSpacing

        Item {
            // Indent nested rows/headers under their ancestors.
            Layout.preferredWidth: row.depth * Kirigami.Units.gridUnit
        }

        Kirigami.Icon {
            visible: row.isGroupHeader
            source: row.expanded ? "arrow-down" : "arrow-right"
            Layout.preferredWidth: Kirigami.Units.iconSizes.small
            Layout.preferredHeight: Kirigami.Units.iconSizes.small
            Layout.alignment: Qt.AlignVCenter
        }

        Controls.Label {
            visible: !row.isGroupHeader
            textFormat: Text.PlainText
            text: page.verdictGlyph(row.verdict, row.pending)
            color: row.needsRule ? Theme.needsRule : Theme.verdictColor(row.verdict)
            Layout.alignment: Qt.AlignVCenter
        }

        Kirigami.Icon {
            visible: !row.isGroupHeader
            source: row.appIcon
            fallback: "application-x-executable"
            Layout.preferredWidth: Theme.iconRow
            Layout.preferredHeight: Theme.iconRow
            Layout.alignment: Qt.AlignVCenter
        }

        ColumnLayout {
            visible: !row.isGroupHeader
            Layout.fillWidth: true
            spacing: 0
            ProgramLabel {
                Layout.fillWidth: true
                src: row
                bold: row.pending
            }
            Controls.Label {
                textFormat: Text.PlainText
                objectName: "hostLabel"
                text: (row.repeatCount > 1 ? "×" + row.repeatCount + " · last seen " + Qt.formatTime(
                    new Date(row.lastSeenMs), "hh:mm:ss") + " · " : "") + row.host + ":" + row.port + "  " + row.protocol
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                elide: Text.ElideMiddle
                Layout.fillWidth: true
            }
            Controls.Label { objectName: "heldBackLabel"; visible: row.pending && row.retriedCount > 0; textFormat: Text.PlainText; opacity: 0.7; font: Kirigami.Theme.smallFont
                text: "notice held back " + row.retriedCount + (row.retriedCount === 1 ? " time" : " times") + " in the last hour" }
        }

        Controls.Label {
            objectName: "verdictLabel"
            visible: !row.isGroupHeader
            textFormat: Text.PlainText
            text: row.pending ? "pending"
                : row.outcomeText !== "" ? row.outcomeText
                : row.answeredWhilePaused ? "Allowed once (filtering was paused)"
                : row.needsRule ? row.verdict + " once, needs a rule"
                : row.verdict
            color: row.needsRule ? Theme.needsRule : Theme.verdictColor(row.verdict)
            Layout.alignment: Qt.AlignVCenter
        }

        ProgramLabel {
            visible: row.isGroupHeader
            Layout.fillWidth: true
            src: row
            bold: row.groupLevel < 2
            labelColor: row.groupPending > 0 ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.textColor
        }

        Controls.Label {
            visible: row.isGroupHeader
            text: row.groupTotal + " connection" + (row.groupTotal === 1 ? "" : "s")
            color: Kirigami.Theme.disabledTextColor
            Layout.alignment: Qt.AlignVCenter
        }

        Rectangle {
            // Collapsed groups expose pending connections through this badge.
            // Use a border: Fusion/Basic can give neutral background and text
            // the same color, making a filled badge unreadable (issue #31).
            visible: row.isGroupHeader && row.groupPending > 0
            color: "transparent"
            border.color: Kirigami.Theme.neutralTextColor
            border.width: 1
            radius: height / 2
            implicitWidth: pendingBadgeLabel.implicitWidth + Kirigami.Units.largeSpacing
            implicitHeight: pendingBadgeLabel.implicitHeight + Kirigami.Units.smallSpacing
            Layout.alignment: Qt.AlignVCenter

            Controls.Label {
                id: pendingBadgeLabel
                anchors.centerIn: parent
                text: row.groupPending + " pending"
                color: Kirigami.Theme.neutralTextColor
                font: Kirigami.Theme.smallFont
            }
        }

        // Batch pending decisions at every tree level. The subtree includes
        // collapsed and filtered descendants, always within this program.
        RowLayout {
            visible: row.isGroupHeader && row.groupPending > 0
            spacing: Kirigami.Units.smallSpacing

            Controls.Button {
                flat: true
                enabled: !row.submitted
                text: "Allow all (" + row.groupPending + ")"
                icon.name: "dialog-ok-apply"
                onClicked: row.decideOnce("allow", true)
            }
            Controls.Button {
                id: batchDenyButton
                flat: true
                enabled: !row.submitted
                text: "Deny all (" + row.groupPending + ")"
                icon.name: "edit-delete-remove"
                onClicked: row.decideOnce("deny", true)
                Accessible.description: row.denyAllText

                // How long these Denies last (see the row Deny's
                // tooltip below).
                Controls.ToolTip {
                    visible: batchDenyButton.hovered
                    delay: Kirigami.Units.toolTipDelay
                    contentItem: Controls.Label {
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        color: palette.toolTipText
                        text: row.denyAllText
                    }
                }
            }
        }

        // Issue #18: inline Allow/Deny on pending leaf rows, so a
        // decision no longer requires opening the inspector sheet.
        // Allow submits the Settings duration; Deny remembers until the
        // firewall restarts where it can (see InlineVerdicts.qml). Disabled
        // after one click (see `row.submitted`'s doc comment) until the round
        // trip flips `pending` and resets the guard.
        RowLayout {
            visible: !row.isGroupHeader && row.pending
            spacing: Kirigami.Units.smallSpacing

            Controls.Button {
                flat: true
                enabled: !row.submitted
                text: !row.isGroupHeader && row.pending ? verdictHelper.allowLabel(row.rowId) : "Allow"
                icon.name: "dialog-ok-apply"
                onClicked: row.decideOnce("allow", false)
            }
            Controls.Button {
                id: rowDenyButton
                flat: true
                enabled: !row.submitted
                text: "Deny"
                icon.name: "edit-delete-remove"
                onClicked: row.decideOnce("deny", false)
                Accessible.description: row.denyText

                // How long this Deny lasts. An explicit PlainText
                // contentItem, never the attached `ToolTip.text`
                // (issue #51).
                Controls.ToolTip {
                    visible: rowDenyButton.hovered
                    delay: Kirigami.Units.toolTipDelay
                    contentItem: Controls.Label {
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        // The style's own tooltip text uses the
                        // tooltip palette, not the window one.
                        color: palette.toolTipText
                        text: row.denyText
                    }
                }
            }
            DecideLaterButton {
                objectName: "decideLaterButton"
                visible: row.decideLaterOffered
                enabled: !row.submitted
                onClicked: row.decideOnce("later", false)
            }
        }

        // Last on the row, at a fixed width, so the rule-state column lines
        // up; the badge and Allow all / Deny all sit to its left. Its picks
        // carry the node as copied when its menu opened (this delegate is
        // reused for other rows). The page owns the popup through live updates.
        RuleButton {
            id: headerRuleButton
            objectName: "headerRuleButton"
            visible: row.isGroupHeader
            ruleState: row.levelRuleState
            ruleName: row.levelRuleName
            ruleReason: row.levelRuleReason
            level: row.groupLevel
            program: row.ruleProgram
            nodeKey: row.ruleKey
            nodeLabel: row.groupLabel
            menuActions: row.actions
            menuRow: row
            Layout.preferredWidth: implicitWidth
            Layout.alignment: Qt.AlignVCenter
            onCreateRequested: (level, program, key, label, choice) => row.page.levelRules.create(
                level, program, key, label, choice)
            onRemoveRequested: (level, program, key, label) => row.page.levelRules.remove(
                level, program, key, label)
        }
        Controls.ToolButton {
            objectName: "groupActionsButton"
            visible: row.isGroupHeader
            text: "Actions"
            icon.name: "application-menu"
            onClicked: {
                const point = mapToItem(row, 0, height);
                row.actions.open(row, point.x, point.y);
            }
            Accessible.name: "Actions for this group"
        }
    }
}
