// Review screenshots: SNITCHWATCH_DEMO=1 + SNITCHWATCH_SCREENSHOT_DIR=<dir>
// (`dir` is empty unless both are set). Walks a few pages, grabs the window
// (and the standalone alert) to <dir>/<name>.png after each has rendered,
// then quits. Never active in a normal session. Extracted from main.qml.
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Item {
    id: runner

    // The Kirigami.ApplicationWindow, `AppInfo.screenshotDir`, the alert
    // window and the page components a shot may replace the stack with.
    property var window: null
    property string dir: ""
    property var alertWindow: null
    property var pages: ({})

    property int step: 0
    property int pending: 0
    readonly property var shots: [
        { name: "main" },
        { name: "alert", alert: true },
        { name: "connections-expanded", expand: 0 },
        { name: "connections-flat", flat: true },
        { name: "rules", page: "rules" },
        { name: "blocklists", page: "blocklists" },
        { name: "onboarding-groups", page: "curatedGroups" },
        { name: "onboarding-groups-details", page: "curatedGroups", expandGroups: true },
        { name: "recommended-rules", page: "recommendedRules" },
        // The connections page again, narrowed to one program: the rule
        // button's "Can't add rule" state (no program file known), then the
        // confirm dialog of a program-wide rule.
        { name: "connections-unavailable", page: "connections", filter: "dhclient" },
        { name: "connections-confirm", confirm: true },
        // The map band collapsed (header only), then a narrow, short window
        // where the band must give way before the list does.
        { name: "connections-map-collapsed", page: "connections", mapCollapsed: true },
        { name: "connections-map-narrow", page: "connections", mapCollapsed: false, size: [440, 520] }
    ]
    property string sizedFor: ""

    function capture(name, item) {
        runner.pending++;
        item.grabToImage(function (result) {
            const ok = result.saveToFile(runner.dir + "/" + name + ".png");
            console.log("screenshot " + name + (ok ? " saved" : " FAILED"));
            runner.pending--;
        });
    }

    // Expand every collapsed group header at `depth` (0 = process,
    // 1 = domain, 2 = host; expanding a process already opens its domains). Keys
    // are collected first: each toggle reshapes the list.
    function expandDepth(depth) {
        const m = runner.window.connectionsModelRef;
        const list = runner.window.pageStack.currentItem.connectionList;
        list.forceLayout();
        const todo = [];
        for (let i = 0; i < list.count; i++) {
            const d = list.itemAtIndex(i);
            if (d && d.isGroupHeader && d.groupLevel === depth && !d.expanded) {
                todo.push({ key: d.groupKey, parent: d.groupParentKey, process: d.groupProcessKey });
            }
        }
        for (const t of todo) {
            if (depth === 0) {
                m.toggleProcessGroup(t.key);
            } else if (depth === 2) {
                m.toggleHostGroup(t.process, t.parent, t.key);
            } else {
                m.toggleDomainGroup(t.parent, t.key);
            }
        }
    }

    Timer {
        interval: 700
        running: runner.dir !== ""
        repeat: true
        onTriggered: {
            if (runner.pending > 0) {
                return;
            }
            if (!runner.window.visible) {
                runner.window.show();
                return;
            }
            if (runner.step >= runner.shots.length) {
                stop();
                Qt.quit();
                return;
            }
            const shot = runner.shots[runner.step];
            if (shot.alert) {
                // The alert needs a pending demo row and its input delay
                // elapsed (it dims until then); wait for both.
                if (!runner.alertWindow.shown || !runner.alertWindow.armed) {
                    return;
                }
                runner.step++;
                Qt.callLater(function () { runner.capture(shot.name, runner.alertWindow.contentItem); });
                return;
            }
            if (shot.confirm) {
                // The dialog opens on one tick and is grabbed on the next, once
                // it has been laid out. It is a popup, so grab its own content.
                const actions = runner.window.pageStack.currentItem.levelRules;
                if (!actions.confirmDialog.opened) {
                    actions.create(0, "/usr/bin/curl", "", "curl", "allow");
                    return;
                }
                runner.step++;
                runner.capture(shot.name, actions.confirmDialog.contentItem);
                return;
            }
            if (shot.size && runner.sizedFor !== shot.name) {
                // Resize now, grab on the next tick once it has laid out.
                runner.sizedFor = shot.name;
                runner.window.width = shot.size[0];
                runner.window.height = shot.size[1];
                if (shot.page) {
                    runner.window.pageStack.replace(runner.pages[shot.page]);
                }
                MapBandState.collapsed = shot.mapCollapsed === true;
                return;
            }
            runner.step++;
            if (shot.mapCollapsed !== undefined) {
                MapBandState.collapsed = shot.mapCollapsed;
            }
            if (shot.page && !shot.size) {
                runner.window.pageStack.replace(runner.pages[shot.page]);
            }
            if (shot.expandGroups) {
                // The page was just created; its groups appear on the next frame.
                Qt.callLater(function () {
                    const groups = runner.window.pageStack.currentItem.step.groupsRepeater;
                    for (let i = 0; i < groups.count; i++) {
                        groups.itemAt(i).expanded = true;
                    }
                });
            }
            if (shot.filter !== undefined) {
                runner.window.connectionsModelRef.setGroupedMode(true);
                runner.window.connectionsModelRef.setFilterQuery(shot.filter);
            }
            if (shot.expand !== undefined) {
                runner.expandDepth(shot.expand);
            }
            if (shot.flat) {
                runner.window.connectionsModelRef.setGroupedMode(false);
            }
            // Give the page a frame to lay out before grabbing it.
            Qt.callLater(function () { runner.capture(shot.name, runner.window.pageStack); });
        }
    }
}
