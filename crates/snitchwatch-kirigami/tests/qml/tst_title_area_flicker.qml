// Title-area scroll flicker probe (docs/superpowers/research/
// 2026-11-01-title-flicker-3.md). The owner sees one line with "waiting" in
// it flash for a few ms while scrolling with the pointer over the title area
// of Daemon Health and Security Scan. The window-level PromptSlotBanner
// ("A connection is waiting for your answer.") floats over exactly that area
// (main.qml anchors it at the window top, z 999).
//
// This loads the real ScannerPage, DaemonHealthPage and PromptSlotBanner in a
// Kirigami.ApplicationWindow small enough that both pages overflow and wheel
// events really move their Flickable. While the holder text ticks like
// PromptSlotStatus.refresh() and the wheel turns over the banner, the page
// header and the body, it samples every text item in the window on each
// frame (frameSwapped plus a 4 ms timer) and fails on any value that appears
// and reverts within `transientMs`: text, effective visibility, opacity, y or
// height. Such an A-B-A is the flicker's signature.
import QtQuick
import QtQuick.Controls as Controls
import QtTest
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell
import "../../qml" as Production

Item {
    id: host
    width: 10
    height: 10

    readonly property int transientMs: 120
    property var history: ({})
    property var transients: []
    property int samples: 0
    property bool recording: false
    // Proof the run was not vacuous: the wheel moved the page, and the
    // banner heading was sampled on screen.
    property real maxContentY: 0
    property bool sawHeading: false

    QtObject {
        id: statusStub
        property bool supported: true
        property bool held: true
        property string rowId: "1:ask-7"
        property int waited: 3
        property string text: "steam → cdn.example has been waiting for your answer for "
            + waited + " s. Until you answer, other new connections usually get the "
            + "firewall's default action."
    }
    QtObject {
        id: modelStub
        property int pendingCount: 1
        signal dataChanged()
        signal modelReset()
        signal rowsRemoved()
        function isPendingRow(id) { return id === statusStub.rowId; }
        function rowDetailsJson(id) { return "{}"; }
        function inlineDurationFor(id, choice, appBound, token) { return "this_time"; }
        function inlineDenyFor(id, appBound) { return "program_unknown"; }
    }
    QtObject {
        id: feedStub
        property bool ok: true
        property bool decideLater: false
        function decideLaterFor(id) { return feedStub.decideLater; }
        function appBoundRulesFor(id) { return true; }
        function submitVerdict() { return true; }
    }
    ScannerController { id: scannerStub }
    QtObject {
        id: healthStub
        property bool hasProblem: false
        property string statusSummary: "Daemon reachable; firewall checks passed"
        property string troubleshootingText: ""
        property string monitorStatus: "Process monitor: not reported by opensnitchd"
        function recheck() {}
    }

    Component {
        id: scannerPage
        Production.ScannerPage { controller: scannerStub }
    }
    Component {
        id: healthPage
        Production.DaemonHealthPage { model: healthStub }
    }

    Kirigami.ApplicationWindow {
        id: win
        width: 500
        height: 400
        visible: true

        Production.PromptSlotBanner {
            id: banner
            z: 999
            anchors {
                top: parent.top
                left: parent.left
                right: parent.right
                margins: Kirigami.Units.smallSpacing
            }
            status: statusStub
            model: modelStub
            bridgeFeed: feedStub
            armDelayMs: 0
        }

        onFrameSwapped: host.sample()
    }

    // The real refresh() rewrites the holder text once a second.
    Timer {
        interval: 250
        running: host.recording
        repeat: true
        onTriggered: statusStub.waited += 1
    }
    Timer {
        interval: 4
        running: host.recording
        repeat: true
        onTriggered: host.sample()
    }

    // Scrolled page content moves by design, so `y` counts only for the
    // window-level banner, which is anchored and must never jump.
    function walk(item, path, out, opacity, inBanner) {
        const op = opacity * item.opacity;
        const anchored = inBanner || item === banner;
        if (typeof item.text === "string" && item.lineCount !== undefined) {
            const fields = {
                vis: item.visible && op > 0.01 ? "V" : "H",
                op: String(Math.round(op * 10)),
                h: String(Math.round(item.height)),
                text: item.text
            };
            if (anchored) {
                fields.y = String(Math.round(item.mapToItem(null, 0, 0).y));
            }
            out[path] = fields;
            if (fields.vis === "V" && item.text === "A connection is waiting for your answer.") {
                host.sawHeading = true;
            }
        }
        const kids = item.children;
        for (let i = 0; i < kids.length; ++i) {
            walk(kids[i], path + "/" + i, out, op, anchored);
        }
    }

    // Per item and per field: the last value held for at least
    // `transientMs` is "stable". A field that leaves its stable value and
    // returns to it within `transientMs` is a transient, however many
    // intermediate values it passed through (an opacity dip, a hide across a
    // text tick). Changes to an item hidden before and after don't count.
    function sample() {
        if (!recording) {
            return;
        }
        samples += 1;
        const now = Date.now();
        const page = win.pageStack.currentItem;
        if (page && page.flickable) {
            maxContentY = Math.max(maxContentY, page.flickable.contentY);
        }
        const snap = {};
        walk(win.contentItem, "c", snap, 1, false);
        if (win.Controls.Overlay.overlay) {
            walk(win.Controls.Overlay.overlay, "o", snap, 1, false);
        }
        const gone = { vis: "H", op: "0", h: "-", text: "<gone>", y: "-" };
        const keys = Object.keys(snap).concat(Object.keys(history));
        for (let i = 0; i < keys.length; ++i) {
            const key = keys[i];
            const f = snap[key] || gone;
            const rec = history[key] || {};
            const names = ["vis", "op", "h", "text", "y"];
            for (let j = 0; j < names.length; ++j) {
                const name = names[j];
                const value = f[name] === undefined ? "-" : f[name];
                let st = rec[name];
                if (st === undefined) {
                    if (samples === 1) {
                        rec[name] = { stable: value, cur: value, since: now, left: -1, seen: false };
                        continue;
                    }
                    // Created mid-run: its stable baseline is "absent", so an
                    // item that shows up and goes again is a transient too.
                    st = { stable: gone[name], cur: gone[name], since: now - transientMs,
                           left: -1, seen: false };
                    rec[name] = st;
                }
                if (value !== st.cur) {
                    if (value === st.stable && st.left >= 0) {
                        if (now - st.left < transientMs && (st.seen || f.vis === "V")) {
                            transients.push(key + " " + name + ": " + st.stable + " -> " + st.cur
                                            + " (" + (now - st.left) + " ms) -> back");
                        }
                        st.left = -1;
                    } else if (value !== st.stable && st.left < 0) {
                        st.left = now;
                        st.seen = (rec.vis ? rec.vis.cur : f.vis) === "V" || f.vis === "V";
                    } else if (f.vis === "V") {
                        st.seen = true;
                    }
                    st.cur = value;
                    st.since = now;
                } else if (st.cur !== st.stable && now - st.since >= transientMs) {
                    st.stable = st.cur;
                    st.left = -1;
                }
            }
            history[key] = rec;
        }
    }

    TestCase {
        name: "TitleAreaFlicker"
        when: windowShown

        function scrollOver(y) {
            const target = win.contentItem;
            for (let i = 0; i < 12; ++i) {
                mouseWheel(target, win.width / 2, y, 0, (i % 6 < 3) ? -120 : 120);
                wait(30);
            }
            wait(200);
        }

        function run(page, width, height, decideLater) {
            win.width = width;
            win.height = height;
            feedStub.decideLater = decideLater;
            win.pageStack.clear();
            win.pageStack.push(page.createObject(win.pageStack));
            wait(300);
            host.history = {};
            host.recording = true;
            // At 300 px the Security Scan page overflows, so the wheel really
            // moves it. Daemon Health fits even there, as it does at the
            // owner's sizes, where the flicker shows all the same.
            const flick = win.pageStack.currentItem.flickable;
            if (height < 400 && page === scannerPage) {
                verify(flick.contentHeight > flick.height, "page does not overflow at " + height);
            }
            const ys = [8, banner.height / 2, banner.height + 10, win.height - 40];
            for (let i = 0; i < ys.length; ++i) {
                scrollOver(ys[i]);
            }
            host.recording = false;
        }

        function test_no_transient_text_while_scrolling_over_title_area_data() {
            const rows = [];
            const widths = [500, 640];
            const heights = [300, 400];
            const pages = [["scan", scannerPage], ["health", healthPage]];
            for (let w = 0; w < widths.length; ++w) {
                for (let h = 0; h < heights.length; ++h) {
                    for (let p = 0; p < pages.length; ++p) {
                        for (let dl = 0; dl < 2; ++dl) {
                            rows.push({ tag: pages[p][0] + "-" + widths[w] + "x" + heights[h]
                                            + (dl ? "-later" : ""),
                                        page: pages[p][1], width: widths[w], height: heights[h],
                                        dl: dl === 1 });
                        }
                    }
                }
            }
            return rows;
        }

        function test_no_transient_text_while_scrolling_over_title_area(data) {
            host.transients = [];
            host.samples = 0;
            host.maxContentY = 0;
            host.sawHeading = false;
            run(data.page, data.width, data.height, data.dl);
            verify(banner.visible, "the prompt-slot banner is not on screen");
            verify(host.sawHeading, "the banner heading was never sampled on screen");
            if (data.page === scannerPage && data.height < 400) {
                verify(host.maxContentY > 0, "the wheel never moved the Security Scan page");
            }
            verify(host.samples > 100, "too few samples: " + host.samples);
            compare(host.transients.length, 0, host.transients.slice(0, 5).join("\n"));
        }
    }
}
