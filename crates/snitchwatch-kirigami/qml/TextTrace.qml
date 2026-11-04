// SNITCHWATCH_TEXT_TRACE=1 only (main.qml mounts this through a Loader gated
// on appInfo.textTrace, so with the flag off none of it exists). Logs every
// change of on-screen text, effective visibility and opacity, with a
// millisecond timestamp and the item's path, to find which item briefly
// flashes (docs/superpowers/plans/2026-11-03-text-trace.md).
//
// Change-driven: it samples on the window's frameSwapped, no timer. Each
// sample walks `target` (the window content) and `overlay` (popups and
// tooltips) and diffs every text item against the previous sample. Items
// hidden before and after a change are skipped. Lines go out through
// console.warn: console.log is a debug-level message and is dropped in a
// default Qt build, so a flatpak run would show nothing.
import QtQuick

Item {
    id: tracer

    // Root of the item tree to watch, and the popup overlay (may be null).
    property Item target: null
    property Item overlay: null
    // The tray tooltip is not an item, so main.qml hands the string in.
    property string trayTooltip: ""

    // One finished log line; also written with console.warn.
    signal traced(string line)

    readonly property int maxText: 120
    property var previous: ({})
    property bool primed: false
    readonly property double startMs: Date.now()

    function quote(s) {
        const t = s.length > maxText ? s.substring(0, maxText) + "..." : s;
        return JSON.stringify(t);
    }

    function emit(message) {
        const line = "text-trace t=" + (Date.now() - startMs) + "ms " + message;
        console.warn(line);
        traced(line);
    }

    function typeName(item) {
        const m = /^(?:QQuick)?([A-Za-z0-9]+?)(?:_QML.*|_QMLTYPE.*)?\(/.exec(item.toString());
        return m ? m[1] : "Item";
    }

    function segment(item, index) {
        return typeName(item) + "#" + index + (item.objectName ? "[" + item.objectName + "]" : "");
    }

    function walk(item, path, out, opacity) {
        const op = opacity * item.opacity;
        if (typeof item.text === "string" && item.lineCount !== undefined) {
            out[path] = {
                vis: item.visible && op > 0.01 ? "V" : "H",
                op: String(Math.round(op * 10)),
                text: item.text
            };
        }
        const kids = item.children;
        for (let i = 0; i < kids.length; ++i) {
            walk(kids[i], path + "/" + segment(kids[i], i), out, op);
        }
    }

    function diff(path, old, cur) {
        if (old === undefined) {
            if (cur.vis === "V") {
                emit("+ " + path + " vis: V text: " + quote(cur.text));
            }
            return;
        }
        if (cur === undefined) {
            if (old.vis === "V") {
                emit("- " + path + " vis: V -> gone text: " + quote(old.text));
            }
            return;
        }
        if (old.vis === "H" && cur.vis === "H") {
            return;
        }
        // Only text that was actually on screen is printed: the side that was
        // visible, never a string set while the item was hidden.
        const shown = cur.vis === "V" ? cur.text : old.text;
        const names = ["vis", "op"];
        for (let i = 0; i < names.length; ++i) {
            if (old[names[i]] !== cur[names[i]]) {
                emit(path + " " + names[i] + ": " + old[names[i]] + " -> " + cur[names[i]]
                     + " text: " + quote(shown));
            }
        }
        if (old.vis === "V" && cur.vis === "V" && old.text !== cur.text) {
            emit(path + " text: " + quote(old.text) + " -> " + quote(cur.text) + " vis: V");
        }
    }

    function sample() {
        if (!target) {
            return;
        }
        const snap = {};
        walk(target, segment(target, 0), snap, 1);
        if (overlay) {
            walk(overlay, "overlay", snap, 1);
        }
        if (primed) {
            Object.keys(snap).forEach(function (k) { diff(k, previous[k], snap[k]); });
            Object.keys(previous).forEach(function (k) {
                if (snap[k] === undefined) {
                    diff(k, previous[k], undefined);
                }
            });
        }
        previous = snap;
        primed = true;
    }

    onTrayTooltipChanged: {
        if (primed) {
            emit("tray.tooltip: " + quote(trayTooltipOld) + " -> " + quote(trayTooltip));
        }
        trayTooltipOld = trayTooltip;
    }
    property string trayTooltipOld: ""

    Component.onCompleted: {
        trayTooltipOld = trayTooltip;
        sample();
    }

    Connections {
        target: tracer.Window.window
        function onFrameSwapped() { tracer.sample(); }
    }
}
