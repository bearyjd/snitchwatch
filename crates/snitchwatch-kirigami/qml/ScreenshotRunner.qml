// Review screenshots: SNITCHWATCH_DEMO=1 + SNITCHWATCH_SCREENSHOT_DIR=<dir>
// (`dir` is empty unless both are set). Walks a few pages, grabs the window
// (and the standalone alert) to <dir>/<name>.png after each has rendered,
// then quits. Never active in a normal session. Extracted from main.qml.
import QtQuick
import QtQuick.Window

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
        { name: "recommended-rules", page: "recommendedRules" }
    ]

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
            if (d && d.isGroupHeader && d.depth === depth && !d.expanded) {
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
            runner.step++;
            if (shot.page) {
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
