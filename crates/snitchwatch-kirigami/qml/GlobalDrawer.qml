// The window's global drawer: one action per top-level page, in this order.
// Extracted from main.qml (its 800-line cap) without changing what an entry
// does: it routes through `StackNav.replaceMain` to the page component main.qml
// registers under the entry's key in `pages`.
//
// Keyboard: after an entry fires, keyboard focus moves to the NEW page's first
// control (or the page itself when it has none), never left on the page that
// was just destroyed. A held key (auto-repeat) cannot fire an entry or open
// the drawer (see `guardKeys`).
import QtQuick
import org.kde.kirigami as Kirigami

Kirigami.GlobalDrawer {
    id: drawer

    // main.qml's StackNav, and its page Components by key.
    property var nav
    property var pages: ({})

    titleIcon: "security-high"
    handleClosedToolTip: "Open navigation menu"
    isMenu: false

    // The window (main.qml's).
    property var window

    focus: true

    // Show page `key`, close a modal drawer and put focus inside the new page.
    function openPage(key) {
        const page = drawer.nav.replaceMain(drawer.pages[key]);
        if (drawer.modal) drawer.close();
        drawer.focusFirst(page);
        return page;
    }

    // Focus the first control under `item` (the item itself when it has none).
    function focusFirst(item) {
        if (!item) return;
        item.forceActiveFocus(Qt.TabFocusReason);
        const first = item.nextItemInFocusChain(true);
        for (let it = first; it; it = it.parent) {
            if (it === item) {
                first.forceActiveFocus(Qt.TabFocusReason);
                return;
            }
        }
    }

    // Kirigami's own controls need two fixes, applied to each as it appears
    // (the opener is rebuilt with every page header, the entries once):
    //  * the opener is an icon-only ToolButton with a tooltip but no
    //    accessible name: give it one;
    //  * an entry triggers on the PRESS of Return/Enter, and a button clicks
    //    on the release of a Space, so a key still held while focus arrives
    //    (auto-repeat) would switch pages or open the drawer. Kirigami's
    //    entry handles Return/Enter in its own `Keys.onReturnPressed`, which
    //    a `Keys.onPressed` could not pre-empt, so key events are FORWARDED
    //    first (`Keys.forwardTo`) to `repeatGuard`, which swallows the
    //    auto-repeat of those keys (GuardedButton does the same).
    property var guarded: []
    property var repeatGuard: null
    readonly property var actionKeys: [Qt.Key_Return, Qt.Key_Enter, Qt.Key_Space]
    Component {
        id: repeatGuardComponent
        Item {
            width: 0
            height: 0
            Keys.onPressed: event => drawer.swallowRepeat(event)
            Keys.onReleased: event => drawer.swallowRepeat(event)
        }
    }
    function swallowRepeat(event) {
        if (event.isAutoRepeat && drawer.actionKeys.indexOf(event.key) >= 0) event.accepted = true;
    }
    function guardKeys(item) {
        if (drawer.guarded.indexOf(item) >= 0) return;
        if (!drawer.repeatGuard) drawer.repeatGuard = repeatGuardComponent.createObject(drawer.window.contentItem);
        drawer.guarded = drawer.guarded.concat([item]);
        item.Keys.forwardTo = [drawer.repeatGuard];
    }
    function fixControls(item) {
        if (!item) return;
        if (item.drawer === drawer && item.action !== undefined) {
            item.Accessible.name = drawer.handleClosedToolTip;
            drawer.guardKeys(item);
        } else if (item.tAction !== undefined && item.activeFocusOnTab) {
            drawer.guardKeys(item);
        }
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) drawer.fixControls(kids[i]);
    }
    function fixControlsLater() {
        Qt.callLater(() => {
            if (!drawer.window) return;
            drawer.fixControls(drawer.window.contentItem);
            drawer.fixControls(drawer.contentItem);
        });
    }
    Component.onCompleted: fixControlsLater()
    Connections {
        target: drawer.window ? drawer.window.pageStack : null
        function onCurrentItemChanged() { drawer.fixControlsLater(); }
    }

    // Focus enters the drawer on its first entry. Closing it without a page
    // switch (Escape) gives focus back to what had it, which Qt does itself.
    onDrawerOpenChanged: {
        if (!drawer.drawerOpen) return;
        drawer.fixControls(drawer.contentItem);
        Qt.callLater(() => drawer.focusFirst(drawer.contentItem));
    }

    actions: [
        Kirigami.Action {
            text: "Connections"
            icon.name: "network-connect"
            checked: true
            onTriggered: drawer.openPage("connections")
        },
        Kirigami.Action {
            text: "Geography"
            icon.name: "map-globe"
            onTriggered: drawer.openPage("geo")
        },
        Kirigami.Action {
            text: "Rules"
            icon.name: "view-list-details"
            onTriggered: drawer.openPage("rules")
        },
        Kirigami.Action {
            text: "Blocklists"
            icon.name: "edit-delete"
            onTriggered: drawer.openPage("blocklists")
        },
        Kirigami.Action {
            text: "Traffic"
            icon.name: "office-chart-line"
            onTriggered: drawer.openPage("traffic")
        },
        Kirigami.Action {
            text: "Profiles"
            icon.name: "preferences-system-network"
            onTriggered: drawer.openPage("profiles")
        },
        Kirigami.Action {
            text: "Recommended background-service rules"
            icon.name: "checkmark"
            onTriggered: drawer.openPage("recommendedRules")
        },
        Kirigami.Action {
            text: "Make rules from what I saw"
            icon.name: "view-list-details"
            onTriggered: drawer.openPage("learningReview")
        },
        Kirigami.Action {
            text: "Security Scan"
            icon.name: "security-low"
            onTriggered: drawer.openPage("scanner")
        },
        Kirigami.Action {
            text: "Settings & Diagnostics"
            icon.name: "settings-configure"
            onTriggered: drawer.openPage("diagnostics")
        },
        Kirigami.Action {
            text: "Daemon Health"
            icon.name: "dialog-warning"
            onTriggered: drawer.openPage("daemonHealth")
        },
        Kirigami.Action {
            text: "About"
            icon.name: "help-about"
            onTriggered: drawer.openPage("about")
        }
    ]
}
