// One row of the Connections list: a leaf connection, or a process/domain
// group header in the grouped projection. Split out of ConnectionsPage.qml
// (monitor-tree step 1) with no behaviour change.
//
// Instantiated only as the ListView delegate, so the Rust model's roles
// arrive as the required properties below. `page` is the ConnectionsPage that
// owns the list: the row forwards clicks and verdicts to it and never holds
// state of its own beyond the double-submit latch.
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
    implicitHeight: content.implicitHeight + Kirigami.Units.smallSpacing * 2

    required property int index
    required property string rowId
    required property string sourceSession
    required property string process
    required property string host
    required property int port
    required property string protocol
    required property string verdict
    required property bool pending

    // Grouping roles (Little-Snitch-parity Process->Domain view).
    // depth is the indent step. groupLevel is the tree tier: 0 = program
    // header, 1 = domain header, 2 = host header, 3 = connection row. Flat
    // mode always reports depth 0 / isGroupHeader false (level 3), so this
    // delegate renders identically to before grouping was added.
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
    // Issue #18 double-submit guard: the inline/batch buttons stay
    // visible until the round trip flips `pending` to false, so a
    // second click before that arrives would otherwise send a
    // second (safe but ERROR-logged, feedback-less) SetVerdict.
    // Reset whenever this delegate instance starts representing a
    // different row (reuseItems recycles delegates, so `rowId`
    // changing is the reliable "this is a new row now" signal) or
    // when the row's own pending state changes (a fresh pending
    // arrival on the same id, or a decision landing).
    property bool submitted: false
    onRowIdChanged: row.submitted = false
    onPendingChanged: row.submitted = false

    // What this row's Deny, or this header's "Deny all", does: the
    // buttons' tooltip and accessible description. Empty where the
    // button is hidden, so other delegates never look it up.
    readonly property string denyText: !row.isGroupHeader && row.pending
        ? verdictHelper.denyText(row.rowId) : ""
    readonly property string denyAllText: row.isGroupHeader && row.groupLevel === 0
        && row.groupPending > 0 ? verdictHelper.denyAllText(row.groupKey) : ""
    // Exposed for the headless probe (tests/inline_verdict_qml.rs).
    property alias denyButton: rowDenyButton
    property alias denyAllButton: batchDenyButton

    // Single latch-and-dispatch for all 4 verdict buttons. Kept as one
    // function so the re-entry guard can't be present on some sites
    // and missing on others — it already was once, leaving "Allow
    // all" able to double-submit every pending row under a process.
    // `batch` picks the process-group action over the single-row one.
    //
    // These buttons used to ALSO carry a `TapHandler`, added on the
    // theory that a nested action needs its own pointer grab. Measured
    // with QtTest `mouseClick()` against this exact delegate shape
    // (Item > MouseArea z:0 + content z:1 > Button): one real click
    // fires `onClicked` AND `onTapped` — 1 each, identical across the
    // Basic/Fusion/Material/Universal styles — while the underlying
    // MouseArea correctly stays silent. So the TapHandler was pure
    // double-dispatch and is gone; plain `onClicked` is sufficient.
    readonly property bool decideLaterOffered: !row.isGroupHeader && row.pending
        && verdictHelper.rowDecideLater(row.rowId)

    function decideOnce(choice, batch) {
        if (row.submitted) {
            return;
        }
        row.submitted = true;
        if (batch) {
            page.submitBatchVerdict(row.groupKey, choice, row.sourceSession);
        } else if (choice === "later") {
            // Part C "Decide later": not a verdict QML builds.
            verdictHelper.decideLater(row.rowId);
        } else {
            page.submitInlineVerdict(row.rowId, choice);
        }
    }

    // Deliberately behind `content`: leaf/header row clicks work in
    // the remaining space, while nested Buttons receive their own
    // pointer events instead of being swallowed by ItemDelegate.
    MouseArea {
        anchors.fill: parent
        z: 0
        onClicked: {
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

    RowLayout {
        id: content
        anchors.fill: parent
        anchors.margins: Kirigami.Units.smallSpacing
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
            Controls.Label {
                textFormat: Text.PlainText
                text: row.process
                font.bold: row.pending
                elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Controls.Label {
                textFormat: Text.PlainText
                text: row.host + ":" + row.port + "  " + row.protocol
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                elide: Text.ElideMiddle
                Layout.fillWidth: true
            }
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

        Controls.Label {
            visible: row.isGroupHeader
            textFormat: Text.PlainText
            text: row.groupLabel
            font.bold: row.groupLevel < 2
            color: row.groupPending > 0 ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.textColor
            elide: Text.ElideRight
            Layout.fillWidth: true
        }

        Controls.Label {
            visible: row.isGroupHeader
            text: row.groupTotal + " connection" + (row.groupTotal === 1 ? "" : "s")
            color: Kirigami.Theme.disabledTextColor
            Layout.alignment: Qt.AlignVCenter
        }

        Rectangle {
            // Pending-count badge. Groups are never force-expanded
            // for pending rows (grouping.rs module doc), so this
            // badge is the only signal that a collapsed group hides
            // undecided connections — it must read as a badge, not
            // as body text.
            //
            // Issue #31: this used to fill with neutralBackgroundColor
            // under a neutralTextColor label. Under Fusion/Basic those
            // two roles resolve to the identical orange, so the label
            // vanished into its own fill. Border-only sidesteps the
            // pairing entirely — neutralTextColor is already used bare
            // (no fill) elsewhere on this page, e.g. the verdict Label
            // above, so it's proven to read against the row background.
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

        // Issue #18: per-process batch actions. Only on top-level
        // program headers (groupLevel 0) with at least one pending
        // descendant — a domain header or a fully-decided process has
        // nothing to batch-act on.
        RowLayout {
            visible: row.isGroupHeader && row.groupLevel === 0 && row.groupPending > 0
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
    }
}
