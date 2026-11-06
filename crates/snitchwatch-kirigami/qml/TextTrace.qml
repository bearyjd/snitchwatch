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
//
// ALLOWLIST: text is read ONLY under an item that opts in with
// `property bool textTraceable: true` (an item's own value overrides what it
// inherits, so `false` opts a part of a marked subtree back out). Unmarked
// items are never read: not their text, not displayText/selectedText. The
// window title comes from the page stack's toolbar, and only while the current
// page declares `textTraceTitle: true` (a fixed, literal title). Marks live in
// the component files; docs/superpowers/plans/2026-11-03-text-trace.md lists them.
//
// Defence in depth for marked items: text is logged only while the item is
// visible, effective opacity > 0 and FULLY inside the window and every
// clip:true ancestor ("(partly hidden)" otherwise); editable fields log a
// placeholder, truncated text logs "(elided)". The tray tooltip is the one
// labelled exception (see onTrayTooltipChanged) and withholds the
// RecentBlock "Blocked: <what>" detail.
import QtQuick

Item {
    id: tracer

    // Root of the item tree to watch, and the popup overlay (may be null).
    property Item target: null
    property Item overlay: null
    // The tray tooltip is not an item, so main.qml hands the string in.
    property string trayTooltip: ""
    // The page stack whose toolbar carries the window title; tests may replace it.
    property var pageStack: Window.window ? Window.window.pageStack : null

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

    readonly property string redacted: "<editable field redacted>"

    // True for anything the user can type into: the content must never be
    // logged. Covers TextInput/TextEdit/TextField/TextArea (they expose
    // cursorPosition), any non-Normal echoMode (password) and readOnly: false.
    function isEditable(item) {
        if (item.echoMode !== undefined && item.echoMode !== TextInput.Normal) {
            return true;
        }
        if (item.readOnly !== undefined) {
            return item.readOnly === false;
        }
        return item.cursorPosition !== undefined || item.inputMethodHints !== undefined;
    }

    // Window-coordinate bounds of an item (corners only; ignores rotation).
    function bounds(item) {
        const a = item.mapToItem(null, 0, 0);
        const b = item.mapToItem(null, item.width, item.height);
        return {
            l: Math.min(a.x, b.x), t: Math.min(a.y, b.y),
            r: Math.max(a.x, b.x), b: Math.max(a.y, b.y)
        };
    }

    function intersect(r, c) {
        return { l: Math.max(r.l, c.l), t: Math.max(r.t, c.t),
                 r: Math.min(r.r, c.r), b: Math.min(r.b, c.b) };
    }

    // The item that draws the CURRENT page's default title, or null: only
    // while that page's title is a fixed literal (`textTraceTitle`).
    // pageStack.globalToolBar is a non-visual configuration object, so use the
    // page's own `globalToolBarItem` (Kirigami Page: the ToolBarPageHeader
    // rendered for this page only) and look in it for the title delegate
    // (DefaultPageTitleDelegate, holding the title Heading). Other pages'
    // headers, such as an unmarked page below, are never authorized.
    // Pages with a custom titleDelegate mark their own heading instead.
    function titleToolbar() {
        const ps = pageStack;
        const page = ps ? ps.currentItem : null;
        const header = page && page.textTraceTitle === true ? page.globalToolBarItem : null;
        return header ? findTitle(header) : null;
    }

    function findTitle(item) {
        if (/TitleDelegate$/.test(typeName(item))) {
            return item;
        }
        const kids = item.children;
        for (let i = 0; kids && i < kids.length; ++i) {
            const hit = findTitle(kids[i]);
            if (hit) {
                return hit;
            }
        }
        return null;
    }

    // `clipRect` is the window rect narrowed by every clip:true ancestor;
    // `allowed` is the inherited allowlist state.
    function walk(item, path, out, opacity, clipRect, allowed, toolbar) {
        const op = opacity * item.opacity;
        let clip = clipRect;
        if (item.clip) {
            clip = intersect(clipRect, bounds(item));
        }
        let ok = allowed;
        if (item === toolbar) {
            ok = true;
        } else if (item.textTraceable !== undefined) {
            ok = item.textTraceable === true;
        }
        if (ok && typeof item.text === "string"
                && (item.lineCount !== undefined || item.cursorPosition !== undefined)) {
            const r = bounds(item);
            const within = r.l >= clip.l - 1 && r.r <= clip.r + 1 && r.t >= clip.t - 1 && r.b <= clip.b + 1;
            const touches = r.l < clip.r && r.r > clip.l && r.t < clip.b && r.b > clip.t;
            let shown = item.text;
            if (isEditable(item)) {
                shown = redacted;
            } else if (item.truncated === true) {
                shown = "(elided)";
            } else if (!within) {
                shown = "(partly hidden)";
            }
            out[path] = {
                vis: item.visible && op > 0 && touches ? "V" : "H",
                op: String(Math.round(op * 10)),
                text: shown
            };
        }
        const kids = item.children;
        for (let i = 0; i < kids.length; ++i) {
            walk(kids[i], path + "/" + segment(kids[i], i), out, op, clip, ok, toolbar);
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
        const win = tracer.Window.window;
        const view = win ? { l: 0, t: 0, r: win.width, b: win.height }
                         : { l: -Infinity, t: -Infinity, r: Infinity, b: Infinity };
        const toolbar = titleToolbar();
        walk(target, segment(target, 0), snap, 1, view, false, toolbar);
        if (overlay) {
            walk(overlay, "overlay", snap, 1, view, false, toolbar);
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

    // The ONE exception to "on-screen text only": deliberately NOT
    // visibility-gated. The tray tooltip is app-composed status text
    // (bridge/daemon state, see tray_health::compose_tooltip), shown only on
    // hover, invisible to item-tree probing, and a prime candidate for the
    // "waiting" flash. Its lines carry a distinct label so they can never be
    // mistaken for on-screen text. Change-driven, not frame-sampled.
    onTrayTooltipChanged: {
        const cur = safeTooltip(trayTooltip);
        if (primed && cur !== trayTooltipOld) {
            emit("tray-tooltip (not necessarily visible): " + quote(trayTooltipOld) + " -> " + quote(cur));
        }
        trayTooltipOld = cur;
    }
    property string trayTooltipOld: ""

    // "Blocked: <what>" carries the attacker-influenced process and host of a
    // RecentBlock (src/tray.rs derive_tooltip), and compose_tooltip appends
    // more after it, so the source string cannot be split safely: log only
    // the fixed lead-in and drop the rest.
    function safeTooltip(t) {
        return t.indexOf("Blocked:") === 0 ? "Blocked: (details withheld)" : t;
    }

    Component.onCompleted: {
        trayTooltipOld = safeTooltip(trayTooltip);
        sample();
    }

    Connections {
        target: tracer.Window.window
        function onFrameSwapped() { tracer.sample(); }
    }
}
