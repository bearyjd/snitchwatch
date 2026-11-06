// SNITCHWATCH_TEXT_TRACE tracer (docs/superpowers/plans/2026-11-03-text-trace.md).
// Loads the real TextTrace over a stub tree, drives it with sample() and
// asserts the emitted lines name the item path, the field and old -> new.
import QtQuick
import QtQuick.Controls as Controls
import QtTest
import "../../qml" as Production

Item {
    id: host
    width: 300
    height: 200

    property var lines: []

    Item {
        id: tree
        anchors.fill: parent

        // Marked (allowlisted) area, like a banner or status strip.
        Item {
            id: marked
            objectName: "marked"
            property bool textTraceable: true
            anchors.fill: parent
            Item {
                id: banner
                objectName: "banner"
                Text { id: heading; objectName: "heading"; text: "idle" }
            }
            Text { id: other; text: "static" }
            TextEdit { id: roEdit; objectName: "roEdit"; y: 110; width: 100; readOnly: true; text: "readonly-shown" }
            // Defence in depth inside a marked area.
            TextInput { id: markedInput; objectName: "markedInput"; y: 20; width: 100; text: "" }
            Controls.TextField {
                id: markedSecret; objectName: "markedSecret"; y: 60; width: 100
                echoMode: TextInput.Password
                text: ""
            }
            Flickable {
                id: flick
                objectName: "flick"
                y: 130
                width: 100
                height: 20
                clip: true
                contentHeight: 200
                Text { id: inView; objectName: "inView"; y: 2; text: "in-view" }
                Text { id: scrolledOut; objectName: "scrolledOut"; y: 100; text: "CLIPPED_CANARY" }
            }
            Item {
                id: clipBox
                y: 160
                width: 100
                height: 18
                clip: true
                Text {
                    id: twoLines
                    objectName: "twoLines"
                    width: 40
                    wrapMode: Text.WordWrap
                    text: "first SECOND_LINE_CANARY"
                }
            }
            Text { id: outside; objectName: "outside"; x: 1000; text: "OUTSIDE_CANARY" }
            Text {
                id: elided
                objectName: "elided"
                y: 180
                width: 30
                elide: Text.ElideRight
                text: "ELIDED_CANARY_FULL_TEXT"
            }
        }

        Item { id: toolbarStub; objectName: "toolbar"; Text { id: titleText; objectName: "title"; text: "Daemon Health" } }

        // Everything below is NOT marked: never read, whatever it holds.
        Item {
            id: unmarked
            objectName: "unmarked"
            Text { id: plainLabel; objectName: "plainLabel"; text: "UNMARKED_CANARY" }
            TextInput { id: plainInput; objectName: "plainInput"; y: 20; width: 100; text: "" }
            TextEdit { id: edit; objectName: "edit"; y: 40; width: 100; text: "" }
            Controls.TextField { id: field; objectName: "field"; y: 60; width: 100; text: "" }
            Controls.TextField {
                id: secret; objectName: "secret"; y: 80; width: 100
                echoMode: TextInput.Password
                text: ""
            }
            Controls.TextArea { id: area; objectName: "area"; y: 100; width: 100; height: 30; text: "" }
            Controls.ComboBox { id: combo; objectName: "combo"; y: 130; width: 100; editable: true; model: ["a", "b"] }
            Text { objectName: "boundText"; y: 150; text: plainInput.text }
            Text { objectName: "boundDisplay"; y: 150; text: plainInput.displayText }
            Text { objectName: "boundSelected"; y: 150; text: plainInput.selectedText }
            Text { objectName: "boundPreedit"; y: 150; text: plainInput.preeditText }
            Text { objectName: "boundSecret"; y: 150; text: secret.text }
            Repeater {
                id: rep
                model: ["DELEGATE_CANARY"]
                delegate: Text { required property string modelData; text: modelData }
            }
        }
    }

    // A stand-in page stack for the window-title toolbar.
    QtObject { id: fakePage; property bool textTraceTitle: true }
    QtObject { id: plainPage }
    QtObject { id: fakeStack; property var currentItem: fakePage; property var globalToolBar: toolbarStub }

    Production.TextTrace {
        id: tracer
        target: tree
        trayTooltip: "Snitchwatch - ok"
        pageStack: null
        onTraced: function (line) { host.lines = host.lines.concat([line]); }
    }

    TestCase {
        name: "TextTrace"
        when: windowShown

        function init() {
            heading.text = "idle";
            banner.visible = true;
            other.text = "static";
            flick.contentY = 0;
            scrolledOut.text = "CLIPPED_CANARY";
            outside.text = "OUTSIDE_CANARY";
            roEdit.text = "readonly-shown";
            tracer.sample();
            host.lines = [];
        }

        function joined() { return host.lines.join("\n"); }

        function test_quiet_sample_logs_nothing() {
            tracer.sample();
            compare(host.lines.length, 0);
        }

        function test_rendered_frames_sample_without_a_manual_call() {
            heading.text = "frame-driven";
            tryVerify(function () { return host.lines.length > 0; }, 2000);
            verify(joined().indexOf('-> "frame-driven"') >= 0, joined());
        }

        function test_text_change_names_path_field_and_values() {
            heading.text = "working";
            tracer.sample();
            compare(host.lines.length, 1);
            const l = host.lines[0];
            verify(l.indexOf("text-trace t=") === 0, l);
            verify(l.indexOf("banner") >= 0, "path lacks the objectName: " + l);
            verify(l.indexOf("Text#0[heading]") >= 0, "path lacks the item: " + l);
            verify(l.indexOf('text: "idle" -> "working"') >= 0, l);
        }

        function test_brief_hide_is_logged_both_ways() {
            banner.visible = false;
            tracer.sample();
            wait(5);
            banner.visible = true;
            tracer.sample();
            const s = joined();
            verify(s.indexOf("[heading] vis: V -> H") >= 0, s);
            verify(s.indexOf("[heading] vis: H -> V") >= 0, s);
        }

        function test_opacity_change_is_logged() {
            banner.opacity = 0.5;
            tracer.sample();
            banner.opacity = 1;
            verify(joined().indexOf("[heading] op: 10 -> 5") >= 0, joined());
        }

        function test_new_visible_item_and_removal() {
            const c = Qt.createQmlObject('import QtQuick; Text { objectName: "late"; text: "waiting" }', marked);
            tracer.sample();
            verify(joined().indexOf('+ ') >= 0 && joined().indexOf('[late]') >= 0
                   && joined().indexOf('"waiting"') >= 0, joined());
            host.lines = [];
            c.destroy();
            wait(10);
            tracer.sample();
            verify(joined().indexOf("- ") >= 0 && joined().indexOf("[late]") >= 0, joined());
        }

        function test_text_set_while_hidden_is_never_printed() {
            heading.text = "public";
            tracer.sample();
            host.lines = [];
            banner.visible = false;
            heading.text = "private";
            tracer.sample();
            const s = joined();
            verify(s.indexOf("[heading] vis: V -> H text: \"public\"") >= 0, s);
            compare(s.indexOf("private"), -1, s);
        }

        function test_hidden_before_and_after_is_not_logged() {
            banner.visible = false;
            tracer.sample();
            host.lines = [];
            heading.text = "still hidden";
            tracer.sample();
            compare(host.lines.length, 0);
        }

        function test_tray_tooltip_change_is_logged_and_labelled() {
            tracer.trayTooltip = "Snitchwatch - working";
            verify(joined().indexOf('tray-tooltip (not necessarily visible): "Snitchwatch - ok" -> "Snitchwatch - working"') >= 0, joined());
            compare(joined().indexOf("tray.tooltip"), -1);
        }

        function test_recent_block_detail_never_reaches_the_log() {
            tracer.trayTooltip = "Blocked: BLOCK_CANARY \u2192 evil-host.example. Firewall issue";
            tracer.trayTooltip = "Blocked: OTHER_CANARY \u2192 other.example";
            tracer.trayTooltip = "Snitchwatch - ok";
            compare(joined().indexOf("CANARY"), -1, joined());
            compare(joined().indexOf("evil-host"), -1, joined());
            verify(joined().indexOf("Blocked: (details withheld)") >= 0, joined());
        }

        function test_unmarked_items_are_never_read() {
            plainLabel.text = "UNMARKED_CANARY2";
            tracer.sample();
            compare(host.lines.length, 0, joined());
        }

        function test_canaries_in_editors_bound_texts_delegates_and_combo_never_log() {
            const c = "API_TOKEN=CANARY_9f3a";
            plainInput.text = c;
            edit.text = c;
            field.text = c;
            secret.text = c;
            area.text = c;
            combo.editText = c;
            combo.popup.open();
            plainInput.select(0, 5);
            rep.model = [c];
            tracer.sample();
            field.insert(0, "CANARY_insert");
            tracer.sample();
            combo.popup.close();
            compare(host.lines.length, 0, joined());
        }

        function test_marked_editable_fields_are_still_redacted() {
            markedInput.text = "CANARY_marked";
            markedSecret.text = "CANARY_marked";
            tracer.sample();
            compare(joined().indexOf("CANARY"), -1, joined());
            const created = Qt.createQmlObject('import QtQuick; TextInput { objectName: "late"; text: "CANARY_late" }', marked);
            tracer.sample();
            verify(joined().indexOf("[late]") >= 0 && joined().indexOf("<editable field redacted>") >= 0, joined());
            compare(joined().indexOf("CANARY"), -1, joined());
            created.destroy();
        }

        function test_read_only_marked_text_still_logs() {
            roEdit.text = "readonly-changed";
            tracer.sample();
            verify(joined().indexOf('"readonly-shown" -> "readonly-changed"') >= 0, joined());
        }

        function test_opt_out_inside_a_marked_subtree() {
            const c = Qt.createQmlObject('import QtQuick; Item { property bool textTraceable: false; Text { text: "OPTOUT_CANARY" } }', marked);
            tracer.sample();
            compare(joined().indexOf("OPTOUT_CANARY"), -1, joined());
            c.destroy();
        }

        function test_clipped_and_offscreen_text_is_never_logged() {
            scrolledOut.text = "CLIPPED_CANARY2";
            outside.text = "OUTSIDE_CANARY2";
            tracer.sample();
            compare(joined().indexOf("CANARY"), -1, joined());
        }

        function test_partly_clipped_two_line_text_hides_the_invisible_line() {
            tryVerify(function () { return twoLines.height > 25; }, 2000);
            twoLines.text = "first SECOND_LINE_CANARY changed";
            tracer.sample();
            const t = Qt.createQmlObject('import QtQuick; Text { objectName: "late2"; width: 40; wrapMode: Text.WordWrap; text: "first SECOND_LINE_CANARY" }', clipBox);
            tryVerify(function () { return t.height > 25; }, 2000);
            tracer.sample();
            compare(joined().indexOf("CANARY"), -1, joined());
            verify(joined().indexOf("[late2] vis: V text: \"(partly hidden)\"") >= 0, joined());
            t.destroy();
        }

        function test_scrolling_text_into_view_logs_it_and_out_stops() {
            flick.contentY = 100;
            tracer.sample();
            verify(joined().indexOf("CLIPPED_CANARY") >= 0, joined());
            host.lines = [];
            flick.contentY = 0;
            scrolledOut.text = "CLIPPED_AFTER";
            tracer.sample();
            compare(joined().indexOf("CLIPPED_AFTER"), -1, joined());
            verify(joined().indexOf("[scrolledOut] vis: V -> H") >= 0, joined());
        }

        function test_elided_text_logs_marker_not_raw_text() {
            tryVerify(function () { return elided.truncated; }, 2000);
            elided.text = "ELIDED_CANARY_CHANGED";
            tracer.sample();
            tracer.sample();
            compare(joined().indexOf("ELIDED_CANARY"), -1, joined());
        }

        function test_elided_marker_is_logged_when_item_appears() {
            const e = Qt.createQmlObject('import QtQuick; Text { objectName: "e2"; width: 20; elide: Text.ElideRight; text: "ELIDED_NEW_CANARY" }', marked);
            tryVerify(function () { return e.truncated; }, 2000);
            tracer.sample();
            verify(joined().indexOf("(elided)") >= 0, joined());
            compare(joined().indexOf("CANARY"), -1, joined());
            e.destroy();
        }

        function test_window_title_is_traced_only_for_a_fixed_title_page() {
            tracer.pageStack = fakeStack;
            tracer.sample();
            titleText.text = "Security Scan";
            tracer.sample();
            verify(joined().indexOf('[title] text: "Daemon Health" -> "Security Scan"') >= 0
                   || joined().indexOf('"Security Scan"') >= 0, joined());
            host.lines = [];
            fakeStack.currentItem = plainPage;
            tracer.sample();
            host.lines = [];
            titleText.text = "TITLE_CANARY";
            tracer.sample();
            compare(joined().indexOf("TITLE_CANARY"), -1, joined());
            fakeStack.currentItem = fakePage;
            tracer.pageStack = null;
        }
    }
}
