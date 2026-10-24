// One menu for right-click and the visible group Actions button. Copy the
// target before opening: ListView delegates can be recycled while it is open.
import QtQuick
import QtQuick.Controls as Controls

Item {
    id: actions
    property var page
    property var target: null
    property alias menu: menu
    property bool rulesOnly: false
    property bool submitted: false
    readonly property bool connected: actions.page.bridgeFeed !== null
        && actions.page.bridgeFeed.ok !== false
    readonly property string scope: !target ? "group"
        : target.level === 0 ? "app" : target.level === 1 ? "domain" : "host"

    ClipboardCopy { id: clipboard }
    RuleScopePreview { id: scopePreview }

    function snapshot(row) {
        let details = ({});
        try { details = JSON.parse(actions.page.model.rowDetailsJson(row.rowId)); } catch (e) {}
        const duration = actions.page.levelRules.model.levelRuleDuration();
        return { details: details, durationToken: duration,
            scopeText: row.isGroupHeader ? scopePreview.describe(actions.page.levelRules.model, row.groupLevel, row.ruleProgram, row.ruleKey, duration) : "This connection only; this attempt only; no remembered rule",
            rowId: row.rowId, pending: row.pending, header: row.isGroupHeader,
            level: row.groupLevel, program: row.ruleProgram, key: row.ruleKey, domain: row.groupParentKey,
            label: row.isGroupHeader ? row.groupLabel : row.process + " → " + row.host, count: row.groupPending, session: row.sourceSession,
            ruleState: row.levelRuleState, ruleName: row.levelRuleName, ruleReason: row.levelRuleReason };
    }

    function show(target, anchor, x, y, rulesOnly) {
        menu.close();
        actions.target = target;
        actions.rulesOnly = rulesOnly;
        actions.submitted = false;
        // Anchor to the page, never the delegate: model resets destroy rows.
        const point = anchor.mapToItem(actions.page, x, y);
        menu.popup(actions.page, point.x, point.y);
    }

    function open(row, x, y) {
        actions.show(actions.snapshot(row), row, x, y, false);
    }

    function openRule(row, anchor) {
        actions.show(actions.snapshot(row), anchor, 0, anchor.height, true);
    }

    function close() {
        menu.close();
        actions.target = null;
    }

    function valid() {
        const n = actions.target;
        return !!n && actions.connected && actions.page.model
            && n.session === actions.page.model.currentSourceSession()
            && (n.header || actions.page.model.rowDetailsJson(n.rowId) !== "{}");
    }

    function recheck() {
        const n = actions.target;
        if (n && (!actions.valid() || (!n.header && n.pending
                && !actions.page.model.isPendingRow(n.rowId)))) actions.close();
    }

    function decide(choice) {
        const n = actions.target;
        if (!actions.valid() || actions.submitted) return;
        if (!n.header) {
            // Flush grouped updates only on dispatch, never from model callbacks.
            actions.page.model.pendingRowIdsForProcess("");
            if (!actions.valid() || !actions.page.model.isPendingRow(n.rowId)) return;
        }
        actions.submitted = true;
        if (n.header) {
            actions.page.inlineVerdicts.submitGroupBatch(n.program, n.level, n.key,
                n.domain, choice, n.session, true);
        } else {
            actions.page.inlineVerdicts.submit(n.rowId, choice, true);
        }
    }

    function create(choice) {
        const n = actions.target;
        if (actions.valid() && n.session !== "") actions.page.levelRules.create(n.level, n.program, n.key, n.label, choice, n.session, n.durationToken);
    }

    Connections {
        target: actions.page.model
        function onModelReset() { actions.recheck(); }
        function onDataChanged() { actions.recheck(); }
        function onRowsRemoved() { actions.recheck(); }
    }
    Connections {
        target: actions.page
        function onBridgeFeedChanged() { actions.close(); }
        function onModelChanged() { actions.close(); }
    }
    onConnectedChanged: if (!connected) actions.close()

    Controls.Menu {
        id: menu
        popupType: Controls.Popup.Item
        Controls.MenuItem {
            enabled: false
            text: actions.target ? actions.target.label || actions.target.rowId : ""
            contentItem: Controls.Label { text: parent.text; textFormat: Text.PlainText; elide: Text.ElideMiddle }
        }
        Controls.MenuItem {
            enabled: false
            objectName: "connectionScopePreview"
            text: !actions.target ? "" : !actions.target.header ? actions.target.scopeText
                : (actions.target.ruleState === "none" ? actions.target.scopeText
                   : actions.target.ruleName ? "Existing rule: " + actions.target.ruleName + ". Removal deletes this captured rule."
                   : "Captured " + actions.scope + " group.")
                  + " Pending decisions affect currently matching requests at dispatch, this attempt only; no remembered rule."
            contentItem: Controls.Label { text: parent.text; textFormat: Text.PlainText; wrapMode: Text.WrapAnywhere }
        }
        Repeater {
            model: [
                { name: "copyExecutable", label: "Copy executable path", key: "processPath" },
                { name: "copyHost", label: "Copy host", key: "dstHost" },
                { name: "copyIp", label: "Copy destination IP", key: "dstIp" },
                { name: "copyPort", label: "Copy port", key: "dstPort" }
            ]
            Controls.MenuItem {
                required property var modelData
                readonly property var value: actions.target && actions.target.details ? actions.target.details[modelData.key] : undefined
                objectName: modelData.name
                text: modelData.label
                visible: !actions.rulesOnly && !!actions.target && !actions.target.header
                height: visible ? implicitHeight : 0
                enabled: actions.valid() && value !== undefined && value !== null && String(value) !== "" && (modelData.key !== "dstPort" || String(value) !== "0")
                onTriggered: if (actions.valid()) clipboard.copy(String(value))
            }
        }
        Controls.MenuItem {
            objectName: "allowPendingAction"
            visible: !actions.rulesOnly && !!actions.target && (actions.target.header ? actions.target.count > 0 : actions.target.pending)
            height: visible ? implicitHeight : 0
            enabled: actions.connected && !actions.submitted
            text: actions.target && actions.target.header
                ? "Allow all pending in this " + actions.scope
                : "Allow this connection"
            onTriggered: actions.decide("allow")
        }
        Controls.MenuItem {
            objectName: "denyPendingAction"
            visible: !actions.rulesOnly && !!actions.target && (actions.target.header ? actions.target.count > 0 : actions.target.pending)
            height: visible ? implicitHeight : 0
            enabled: actions.connected && !actions.submitted
            text: actions.target && actions.target.header
                ? "Deny all pending in this " + actions.scope
                : "Deny this connection"
            onTriggered: actions.decide("deny")
        }
        Controls.MenuSeparator {
            visible: !actions.rulesOnly && !!actions.target && actions.target.header && actions.target.count > 0
            height: visible ? implicitHeight : 0
        }
        Controls.MenuItem {
            objectName: "allowGroupRuleAction"
            visible: !!actions.target && actions.target.header && actions.target.ruleState === "none"
            height: visible ? implicitHeight : 0
            enabled: actions.connected
            text: actions.scope === "app" ? "Allow this app to all hosts…"
                : "Allow this " + actions.scope + " for this app"
            onTriggered: actions.create("allow")
        }
        Controls.MenuItem {
            objectName: "denyGroupRuleAction"
            visible: !!actions.target && actions.target.header && actions.target.ruleState === "none"
            height: visible ? implicitHeight : 0
            enabled: actions.connected
            text: actions.scope === "app" ? "Deny this app to all hosts…"
                : "Deny this " + actions.scope + " for this app"
            onTriggered: actions.create("deny")
        }
        Controls.MenuItem {
            objectName: "removeGroupRuleAction"
            visible: !!actions.target && actions.target.header
                && (actions.target.ruleState === "allow" || actions.target.ruleState === "deny")
            height: visible ? implicitHeight : 0
            enabled: actions.connected
            text: "Remove rule for this " + actions.scope
            onTriggered: {
                const n = actions.target;
                if (actions.valid() && n.session !== "") actions.page.levelRules.remove(n.level, n.program, n.key, n.label, n.session, n.ruleName, n.ruleState);
            }
        }
        Controls.MenuItem {
            visible: !!actions.target && actions.target.header
                && ["partial", "pending", "unavailable"].indexOf(actions.target.ruleState) >= 0
            height: visible ? implicitHeight : 0
            enabled: false
            text: !actions.target ? "" : actions.target.ruleState === "partial"
                ? "Mixed rules — manage on the Rules page"
                : actions.target.ruleState === "pending" ? "Waiting for the firewall"
                : actions.target.ruleReason || "A rule cannot be added for this group"
        }
        Controls.MenuItem {
            visible: !actions.connected
            height: visible ? implicitHeight : 0
            enabled: false
            text: "Background service disconnected"
        }
        Controls.MenuItem {
            objectName: "groupConnectionsAction"
            visible: !actions.rulesOnly
            height: visible ? implicitHeight : 0
            text: actions.target && actions.target.header ? "Expand / collapse group" : "Group connections by app"
            onTriggered: {
                const n = actions.target;
                if (!actions.valid()) return;
                if (!n.header) actions.page.model.setGroupedMode(true);
                else if (n.level === 0) actions.page.model.toggleProcessGroup(n.program);
                else if (n.level === 1) actions.page.model.toggleDomainGroup(n.program, n.key);
                else actions.page.model.toggleHostGroup(n.program, n.domain, n.key);
            }
        }
    }
}
