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
        Item {
            id: banner
            objectName: "banner"
            Text { id: heading; objectName: "heading"; text: "idle" }
        }
        Text { id: other; text: "static" }
    }

    Production.TextTrace {
        id: tracer
        target: tree
        trayTooltip: "Snitchwatch - ok"
        onTraced: function (line) { host.lines = host.lines.concat([line]); }
    }

    TestCase {
        name: "TextTrace"
        when: windowShown

        function init() {
            heading.text = "idle";
            banner.visible = true;
            other.text = "static";
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
            const c = Qt.createQmlObject('import QtQuick; Text { objectName: "late"; text: "waiting" }', tree);
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

        function test_tray_tooltip_change_is_logged() {
            tracer.trayTooltip = "Snitchwatch - working";
            verify(joined().indexOf('tray.tooltip: "Snitchwatch - ok" -> "Snitchwatch - working"') >= 0, joined());
        }
    }
}
