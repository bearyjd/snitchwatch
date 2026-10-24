import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000
    height: 500
    property int phase: 0
    property real retainedY: 0
    property bool positiveControl: POSITIVE_CONTROL

    Component.onCompleted: {
        MapBandState.collapsed = true;
        if (positiveControl) page.connectionList.cacheBuffer = 320;
    }
    QtObject {
        id: feedStub
        property bool ok: true
        function appBoundRulesFor() { return true; }
        function submitVerdict() { throw new Error("fixture must never submit a verdict"); }
    }
    ConnectionsModel { id: model }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        model: model
        bridgeFeed: feedStub
    }
    function row(i, generation) {
        return { id: "g" + generation + "-" + i, process: "app" + (i % 5),
            processPath: "/usr/bin/app" + (i % 5), dstHost: "host" + i + ".example.com",
            dstIp: "1.1.1." + i, dstPort: 443, protocol: "tcp", direction: "outgoing",
            action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 + i };
    }
    function expect(ok, text) { if (!ok) throw new Error(text); }
    function send(action, key, values) {
        const message = {action: action};
        if (key) message[key] = values;
        model.applyServerMessageJson(JSON.stringify(message));
    }
    Timer {
        interval: 20
        running: true
        repeat: true
        onTriggered: {
            const n = probe.phase++;
            const list = page.connectionList;
            try {
                if (n === 0) {
                    model.setGroupedMode(false);
                    let rows = [];
                    for (let i = 0; i < 30; ++i) rows.push(probe.row(i, 0));
                    probe.send("insertConnectionRows", "rows", rows);
                } else if (n === 1) {
                    probe.expect(list.count === 30, "initial rows were not rendered");
                    list.currentIndex = 3;
                    list.contentY = 80;
                    probe.retainedY = list.contentY;
                } else if (n === 2) {
                    probe.expect(list.currentItem && list.currentItem.rowId === "g0-3", "selection is not the real leaf");
                    let ids = [];
                    for (let i = 15; i < 30; ++i) ids.push("g0-" + i);
                    probe.send("removeConnectionRows", "ids", ids);
                } else if (n === 3) {
                    probe.expect(list.count === 15, "ranged removal has wrong count");
                    probe.expect(list.currentIndex === 3 && list.currentItem.rowId === "g0-3", "ranged removal lost selection");
                    probe.expect(Math.abs(list.contentY - probe.retainedY) < 1, "ranged removal lost scroll");
                    model.setGroupedMode(true);
                } else if (n === 4) {
                    probe.expect(list.count === 5, "group reset has wrong visible count");
                    // Grouping hides the selected leaf beneath its collapsed
                    // header; Qt may clear that selection. A surviving flat
                    // leaf's identity and scroll were checked above.
                    probe.expect(list.currentIndex >= -1 && list.currentIndex < list.count, "group reset left invalid selection");
                    list.currentIndex = 3;
                    probe.expect(list.currentItem !== null, "group reset left no visible current item");
                    model.setGroupedMode(false);
                } else {
                    // Repeated snapshots, grouping, filtering, ranged shrinks,
                    // scrolling and selection over the shipped row delegate.
                    const step = n % 8;
                    if (step === 0) {
                        probe.send("clearConnectionRows", "", []);
                        let rows = [];
                        for (let i = 0; i < 30; ++i) rows.push(probe.row(i, n));
                        probe.send("insertConnectionRows", "rows", rows);
                    } else if (step === 1) model.setGroupedMode(false);
                    else if (step === 2) { list.positionViewAtIndex(18, ListView.Beginning); list.currentIndex = 3; }
                    else if (step === 3) {
                        let ids = [];
                        for (let i = 15; i < 30; ++i) ids.push("g" + (n - 3) + "-" + i);
                        probe.send("removeConnectionRows", "ids", ids);
                    } else if (step === 4) model.setGroupedMode(true);
                    else if (step === 5) model.setPendingOnly(true);
                    else if (step === 6) { model.setPendingOnly(false); list.positionViewAtEnd(); }
                    else model.setGroupedMode(false);
                    if (n >= 104) {
                        probe.expect(list.count > 0, "stress lost all rows");
                        stop();
                        console.warn("DELEGATE_RESET_DONE");
                        Qt.quit();
                    }
                }
            } catch (error) {
                stop();
                console.warn("DELEGATE_RESET_FAILURE " + error);
                Qt.quit();
            }
        }
    }
}
