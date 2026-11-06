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
            function showPlain() { pageStack.replace(plainPage); }
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
}
