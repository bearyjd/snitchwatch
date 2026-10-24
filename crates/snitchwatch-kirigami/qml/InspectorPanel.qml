// Connections inspector side panel (monitor-tree step 5). Replaces the
// SizedOverlaySheet inspector; the content is the same, only the host moved.
//
// Shows the selected connection's details and every verdict action. Used
// twice by ConnectionsPage: as the right-hand column of a wide window, and
// inside a ConnectionDetailPage pushed onto the page stack when the window is
// too narrow for a column. With nothing selected it shows a placeholder, never
// empty space. Every datum is PlainText.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: panel

    // The owning ConnectionsPage: the panel renders its `inspect*` snapshot
    // and forwards actions to its signals and helpers.
    property var page
    property alias decisionSheet: pendingSheet
    property alias makeRuleSheet: makeRuleSheet
    property alias titleLabel: titleLabel
    // The pushed detail page already has a title bar and a back button.
    property bool showHeader: true
    spacing: Kirigami.Units.largeSpacing

    RowLayout {
        Layout.fillWidth: true
        visible: panel.showHeader && page.inspecting
        Kirigami.Heading {
            id: titleLabel
            objectName: "inspectorTitle"
            Layout.fillWidth: true
            level: 3
            textFormat: Text.PlainText
            text: page.inspectProcess
            elide: Text.ElideMiddle
            HoverTip {
                text: page.inspectProcess
                onlyWhen: titleLabel.truncated
            }
        }
        Controls.ToolButton {
            objectName: "inspectorCloseButton"
            icon.name: "window-close"
            display: Controls.AbstractButton.IconOnly
            text: "Close details"
            HoverTip {
                text: "Close details"
            }
            onClicked: page.dismissInspector()
        }
    }

    Kirigami.PlaceholderMessage {
        objectName: "inspectorPlaceholder"
        Layout.fillWidth: true
        Layout.topMargin: Kirigami.Units.gridUnit * 4
        visible: !page.inspecting
        icon.name: "view-list-details"
        text: "Select a connection to see its details"
    }

    ColumnLayout {
        objectName: "inspectorDetails"
        Layout.fillWidth: true
        visible: page.inspecting
        spacing: Kirigami.Units.largeSpacing

        ConnectionExplanation {
            Layout.fillWidth: true
            snapshot: page.inspectDetails
            recordedVerdict: page.inspectVerdictText
        }
        ConnectionDetailsCopy {
            Layout.fillWidth: true
            snapshot: page.inspectDetails
            available: page.inspecting && page.inspectSameSource
                && (!page.bridgeFeed || page.bridgeFeed.ok !== false)
            validSnapshot: () => page.model && page.inspectSession === page.model.currentSourceSession()
                && (!page.bridgeFeed || page.bridgeFeed.ok !== false)
        }

        Kirigami.FormLayout {
            Layout.fillWidth: true
            Controls.Label {
                Kirigami.FormData.label: "Host"
                objectName: "inspectorHost"
                textFormat: Text.PlainText
                text: page.inspectHost
                Layout.fillWidth: true
                elide: Text.ElideMiddle
                HoverTip {
                    text: page.inspectHost
                    onlyWhen: parent.truncated
                }
            }
            Controls.Label {
                Kirigami.FormData.label: "Destination IP"
                textFormat: Text.PlainText
                text: page.inspectIp.length > 0 ? page.inspectIp : "unavailable"
                elide: Text.ElideMiddle
            }
            Controls.Label {
                Kirigami.FormData.label: "Port"
                textFormat: Text.PlainText
                text: page.inspectPort
            }
            Controls.Label {
                Kirigami.FormData.label: "Protocol"
                textFormat: Text.PlainText
                text: page.inspectProtocol
            }
            Controls.Label {
                Kirigami.FormData.label: "Verdict"
                textFormat: Text.PlainText
                text: page.inspectVerdictText
                color: page.inspectNoLongerPending
                    ? Kirigami.Theme.disabledTextColor
                    : Theme.verdictColor(page.inspectVerdict)
            }
            Controls.Label {
                Kirigami.FormData.label: "Matched rule"
                textFormat: Text.PlainText
                text: page.inspectMatchedRuleDisplay
                elide: Text.ElideMiddle
            }
        }

        // Rule-match diagnostics (Little-Snitch-parity "which rule
        // decided this connection"). Only shown when a specific rule
        // name is known — a pending row ("awaiting decision") or a
        // decided row with no rule name on record ("default action")
        // have nothing to jump to.
        Controls.Button {
            Layout.fillWidth: true
            visible: page.inspectMatchedRule.length > 0
            text: "Show rule"
            icon.name: "view-list-details"
            // Leave the detail page first: the router replaces the topmost page,
            // which would otherwise be this one.
            onClicked: {
                const rule = page.inspectMatchedRule;
                page.dismissInspector();
                page.showRuleRequested(rule);
            }
        }

        Controls.Button {
            objectName: "simulateConnectionButton"
            Layout.fillWidth: true
            text: "Simulate this connection"
            icon.name: "system-run"
            onClicked: {
                const prefill = page.model.simulationPrefillJson(page.inspectId);
                page.dismissInspector();
                page.simulateConnectionRequested(prefill);
            }
        }

        // Issue #49: replaces the decision sheet below once its request is
        // gone, so a late click can't be mistaken for a decision. Worded
        // neutrally: the page can't tell a timeout from a verdict decided
        // elsewhere or a lost connection.
        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: page.inspectNoLongerPending
            type: Kirigami.MessageType.Warning
            text: "This connection is no longer waiting for a decision. It may have "
                + "timed out (opensnitchd then applies its default action), been decided "
                + "elsewhere, or the connection to the background service may have been lost."
        }

        // Pending decision surface (Task 7). The countdown/timeout stays
        // server-side; these buttons call the bridge's verdict path once
        // pending_decision.rs is wired to the injected model.
        PendingDecisionSheet {
            id: pendingSheet
            Layout.fillWidth: true
            visible: page.inspectPending
            rowId: page.inspectId
            process: page.inspectProcess
            host: page.inspectHost
            remoteIp: page.inspectIp
            bindableProcessPath: page.inspectBindableProcessPath
            appBoundRules: page.inspectAppBoundRules
            deadlineMs: page.inspectDeadlineMs
            decideLater: page.inspectDecideLater
            bridgeFeed: page.bridgeFeed
            onDecided: page.dismissInspector()
            onExplained: text => page.showPassiveNotice(text)
        }

        // Part C: a put-off connection can still get a rule, and so can
        // one the firewall's default action decided (E3).
        MakeRuleSheet {
            id: makeRuleSheet
            Layout.fillWidth: true
            visible: page.inspectMakeRuleOffered
            rowId: page.inspectId
            model: page.model
            bindableProcessPath: page.inspectBindableProcessPath
            alsoListedByDefault: page.inspectAlsoListedByDefault
            controller: page.makeRuleController
            blockedForFiveMinutes: page.inspectMatchedRule.length > 0
        }
    }
}
