// One menu for right-click and the visible group Actions button. Copy the
// target before opening: ListView delegates can be recycled while it is open.
import QtQuick
import QtQuick.Controls as Controls

Item {
    id: actions
    property var page
    property var owner
    property var target: null
    property alias menu: menu
    readonly property bool connected: actions.page.bridgeFeed !== null
        && actions.page.bridgeFeed.ok !== false
    readonly property string scope: !target ? "group"
        : target.level === 0 ? "app" : target.level === 1 ? "domain" : "host"

    function open(row, x, y) {
        actions.target = {
            rowId: row.rowId, pending: row.pending, header: row.isGroupHeader,
            level: row.groupLevel, program: row.ruleProgram, key: row.ruleKey, domain: row.groupParentKey,
            label: row.groupLabel, count: row.groupPending, session: row.sourceSession,
            ruleState: row.levelRuleState, ruleReason: row.levelRuleReason
        };
        menu.popup(row, x, y);
    }

    function close() {
        menu.close();
        actions.target = null;
    }

    function decide(choice) {
        const n = actions.target;
        if (!n) return;
        if (!n.header && !actions.page.model.isPendingRow(n.rowId)) {
            actions.page.showPassiveNotice("This connection is no longer waiting for an answer.");
            return;
        }
        actions.owner.decideOnce(choice, n.header, n);
    }

    function create(choice) {
        const n = actions.target;
        if (n) actions.page.levelRules.create(n.level, n.program, n.key, n.label, choice);
    }

    Controls.Menu {
        id: menu
        popupType: Controls.Popup.Item
        Controls.MenuItem {
            objectName: "allowPendingAction"
            visible: !!actions.target && (actions.target.header ? actions.target.count > 0 : actions.target.pending)
            height: visible ? implicitHeight : 0
            enabled: actions.connected && !actions.owner.submitted
            text: actions.target && actions.target.header
                ? "Allow all pending in this " + actions.scope
                : "Allow this connection"
            onTriggered: actions.decide("allow")
        }
        Controls.MenuItem {
            objectName: "denyPendingAction"
            visible: !!actions.target && (actions.target.header ? actions.target.count > 0 : actions.target.pending)
            height: visible ? implicitHeight : 0
            enabled: actions.connected && !actions.owner.submitted
            text: actions.target && actions.target.header
                ? "Deny all pending in this " + actions.scope
                : "Deny this connection"
            onTriggered: actions.decide("deny")
        }
        Controls.MenuSeparator {
            visible: !!actions.target && actions.target.header && actions.target.count > 0
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
                if (n) actions.page.levelRules.remove(n.level, n.program, n.key, n.label);
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
            text: actions.target && actions.target.header ? "Expand / collapse group" : "Group connections by app"
            onTriggered: {
                const n = actions.target;
                if (!n) return;
                if (!n.header) actions.page.model.setGroupedMode(true);
                else if (n.level === 0) actions.page.model.toggleProcessGroup(n.program);
                else if (n.level === 1) actions.page.model.toggleDomainGroup(n.program, n.key);
                else actions.page.model.toggleHostGroup(n.program, n.domain, n.key);
            }
        }
    }
}
