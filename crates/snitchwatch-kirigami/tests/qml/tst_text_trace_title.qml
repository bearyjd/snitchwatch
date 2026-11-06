// The window title is drawn by a Heading inside Kirigami's page header, not by
// the (non-visual) pageStack.globalToolBar object. This runs the real
// TextTrace inside a REAL Kirigami.ApplicationWindow + pageStack (no stubs).
import QtQuick
import QtQuick.Controls as Controls
import QtTest
import org.kde.kirigami as Kirigami
import "../../qml" as Production

TestCase {
    id: tc
    name: "TextTraceTitle"
    when: windowShown
    width: 100
    height: 100

    property var lines: []

    Component {
        id: windowComp
        Kirigami.ApplicationWindow {
            id: win
            width: 600
            height: 400
            visible: true
            property alias tracer: tracer
            pageStack.initialPage: markedPage
            Component {
                id: markedPage
                Kirigami.ScrollablePage {
                    property bool textTraceTitle: true
                    title: "Fixed Title"
                    Item { implicitHeight: 10 }
                }
            }
            Component {
                id: plainPage
                Kirigami.ScrollablePage {
                    title: "PAGE_TITLE_CANARY"
                    Item { implicitHeight: 10 }
                }
            }
            Component {
                id: customPage
                Kirigami.ScrollablePage {
                    id: cp
                    property bool textTraceTitle: true
                    property alias delegate: holder.item
                    title: "Fixed Custom"
                    titleDelegate: DefaultPageTitleDelegate { shown: cp.title; sibling: "SIBLING_CANARY" }
                    Item { id: holder; property Item item: null; implicitHeight: 10 }
                }
            }
            Component {
                id: dynamicPage
                Kirigami.ScrollablePage {
                    property bool textTraceTitle: true
                    title: "Fixed Dynamic"
                    titleDelegate: DefaultPageTitleDelegate { shown: "DYNAMIC_CANARY"; sibling: "SIBLING_CANARY" }
                    Item { implicitHeight: 10 }
                }
            }
            function showCustom() { pageStack.replace(customPage); }
            function showDynamic() { pageStack.replace(dynamicPage); }
            function showPlain() { pageStack.replace(plainPage); }
            function pushMarked() { pageStack.push(markedPage); }
            function pushPlain() { pageStack.push(plainPage); }
            Production.TextTrace {
                id: tracer
                target: win.contentItem
                onTraced: function (line) { tc.lines = tc.lines.concat([line]); }
            }
        }
    }

    function joined() { return tc.lines.join("\n"); }

    function test_title_of_a_marked_page_is_traced_in_the_real_window() {
        const win = createTemporaryObject(windowComp, tc);
        verify(win);
        waitForRendering(win.contentItem);
        wait(300);
        win.tracer.sample();
        tc.lines = [];
        win.pageStack.currentItem.title = "Changed Title";
        wait(100);
        win.tracer.sample();
        verify(joined().indexOf('"Fixed Title" -> "Changed Title"') >= 0, joined());
    }

    function test_page_without_the_marker_logs_no_title() {
        const win = createTemporaryObject(windowComp, tc);
        verify(win);
        waitForRendering(win.contentItem);
        wait(300);
        win.showPlain();
        wait(500);
        win.tracer.sample();
        win.pageStack.currentItem.title = "PAGE_TITLE_CANARY2";
        wait(100);
        win.tracer.sample();
        compare(joined().indexOf("PAGE_TITLE_CANARY"), -1, joined());
    }

    function open_window() {
        const win = createTemporaryObject(windowComp, tc);
        verify(win);
        waitForRendering(win.contentItem);
        wait(300);
        return win;
    }

    // A marked page pushed OVER an unmarked page whose title is a canary: the
    // lower page's title delegate must never be read.
    function test_marked_page_over_unmarked_never_logs_the_lower_title() {
        const win = open_window();
        win.showPlain();
        wait(400);
        win.pushMarked();
        wait(400);
        win.tracer.sample();
        tc.lines = [];
        win.pageStack.get(0).title = "PAGE_TITLE_CANARY_LOWER";
        wait(100);
        win.tracer.sample();
        win.pageStack.currentItem.title = "Marked Changed";
        wait(100);
        win.tracer.sample();
        verify(joined().indexOf('"Marked Changed"') >= 0, "the marked page's own title is traced: " + joined());
        compare(joined().indexOf("CANARY"), -1, joined());
        win.pageStack.pop();
        wait(400);
        win.tracer.sample();
        win.pageStack.currentItem.title = "PAGE_TITLE_CANARY_AFTER_POP";
        wait(100);
        win.tracer.sample();
        compare(joined().indexOf("CANARY"), -1, joined());
    }

    // A marked page BELOW an unmarked one: the current page is unmarked, so
    // no title is read, neither its own nor the lower marked page's.
    function test_marked_page_under_unmarked_logs_no_canary() {
        const win = open_window();
        win.pushPlain();
        wait(400);
        win.tracer.sample();
        tc.lines = [];
        win.pageStack.currentItem.title = "PAGE_TITLE_CANARY_TOP";
        wait(100);
        win.tracer.sample();
        win.pageStack.get(0).title = "Lower Marked Changed";
        wait(100);
        win.tracer.sample();
        compare(joined().indexOf("CANARY"), -1, joined());
        win.pageStack.pop();
        wait(400);
        win.pushPlain();
        wait(400);
        win.pageStack.currentItem.title = "PAGE_TITLE_CANARY_AGAIN";
        wait(100);
        win.tracer.sample();
        compare(joined().indexOf("CANARY"), -1, joined());
    }

    // A custom delegate that has the default delegate's type name must not
    // authorize its whole subtree: a dynamic heading and an unmarked sibling
    // never log.
    function test_custom_delegate_with_the_default_name_logs_nothing_dynamic() {
        const win = open_window();
        win.showDynamic();
        wait(500);
        win.tracer.sample();
        tc.lines = [];
        win.tracer.sample();
        verify(win.pageStack.currentItem.title === "Fixed Dynamic");
        wait(100);
        win.tracer.sample();
        compare(joined().indexOf("CANARY"), -1, joined());
        // Nothing of the custom delegate is in a logged line, now or after changes.
        win.pageStack.currentItem.titleDelegate = null;
        wait(100);
        win.tracer.sample();
        compare(joined().indexOf("CANARY"), -1, joined());
    }

    // The heading shows the (literal) page title, so it is traced; the
    // unmarked sibling Text next to it never is.
    function test_custom_delegate_title_heading_traced_but_not_its_sibling() {
        const win = open_window();
        win.showCustom();
        wait(500);
        win.tracer.sample();
        tc.lines = [];
        win.pageStack.currentItem.title = "Custom Changed";
        wait(100);
        win.tracer.sample();
        verify(joined().indexOf('"Fixed Custom" -> "Custom Changed"') >= 0, joined());
        compare(joined().indexOf("CANARY"), -1, joined());
    }
}
