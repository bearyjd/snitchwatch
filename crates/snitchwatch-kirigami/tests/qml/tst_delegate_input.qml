// Pins the Qt input-routing behaviour ConnectionsPage.qml's verdict buttons
// depend on. Run with `just qml-test`.
//
// **Scope — read before trusting this file.** This is a structural MIRROR of
// ConnectionsPage.qml's delegate, not the page itself. `qmltestrunner` cannot
// load `com.snitchwatch.shell`: cxx-qt registers those types from Rust and
// links them statically into each test binary, so there is no loadable QML
// plugin on disk (`qmldir` says `optional plugin`, and no `.so` is produced).
// Loading the real page here fails with `module ... is not installed`.
//
// So this file answers "how does Qt route a click through this widget shape?"
// — which the cargo/cxx-qt probes physically cannot ask, because cxx-qt-lib
// exposes no way to synthesise a mouse event. It does NOT prove anything
// about ConnectionsPage.qml's current contents; `tests/qml_source_guards.rs`
// covers that half.
//
// What it pins:
//   1. A Button carrying BOTH a TapHandler and onClicked dispatches TWICE for
//      one click. This is why the verdict buttons carry only `onClicked`.
//   2. Plain `onClicked` fires even with a full-bleed MouseArea at z:0 below,
//      so no extra pointer handler is needed to "win" the grab.
//   3. Clicking a Button does NOT leak to that MouseArea (the row inspector
//      must not open when the user hits Allow/Deny), while clicking empty row
//      space does reach it.
//
// Measured identical on Basic/Fusion/Material/Universal — Button click logic
// lives in the shared QQuickAbstractButton base, not in a style.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtTest

Item {
    id: root
    width: 400
    height: 60

    property int tapCount: 0
    property int clickCount: 0
    property int dualTapCount: 0
    property int dualClickCount: 0
    property int mouseAreaCount: 0

    function resetCounters() {
        root.tapCount = 0;
        root.clickCount = 0;
        root.dualTapCount = 0;
        root.dualClickCount = 0;
        root.mouseAreaCount = 0;
    }

    // Mirrors the real delegate: Item > MouseArea(z:0) + content(z:1).
    Item {
        id: row
        anchors.fill: parent

        MouseArea {
            anchors.fill: parent
            z: 0
            onClicked: root.mouseAreaCount++
        }

        RowLayout {
            anchors.fill: parent
            anchors.margins: 4
            z: 1

            // Filler, so the left edge of the row is empty space.
            Item { Layout.fillWidth: true }

            // Current shape: onClicked only.
            Controls.Button {
                id: verdictButton
                flat: true
                text: "Allow"
                onClicked: root.clickCount++
            }

            // Historical shape: TapHandler + onClicked. Kept solely to pin the
            // double-dispatch that justified removing the TapHandler.
            Controls.Button {
                id: dualButton
                flat: true
                text: "Legacy"
                TapHandler {
                    acceptedButtons: Qt.LeftButton
                    grabPermissions: PointerHandler.CanTakeOverFromAnything
                    onTapped: root.dualTapCount++
                }
                onClicked: root.dualClickCount++
            }
        }
    }

    TestCase {
        name: "DelegateInputRouting"
        when: windowShown

        // A lone onClicked is enough: it fires despite the MouseArea at z:0,
        // and the MouseArea does not also fire.
        function test_verdict_button_dispatches_exactly_once() {
            root.resetCounters();
            mouseClick(verdictButton, verdictButton.width / 2, verdictButton.height / 2);
            compare(root.clickCount, 1, "onClicked must fire exactly once per click");
            compare(root.mouseAreaCount, 0,
                    "clicking a verdict button must NOT also open the row inspector");
        }

        // The regression this pins: adding a TapHandler alongside onClicked
        // makes ONE click dispatch TWICE. Without a re-entry guard that
        // double-submits every pending verdict under a process group.
        function test_taphandler_plus_onclicked_double_dispatches() {
            root.resetCounters();
            mouseClick(dualButton, dualButton.width / 2, dualButton.height / 2);
            compare(root.dualClickCount, 1, "onClicked still fires with a TapHandler present");
            compare(root.dualTapCount, 1, "TapHandler also fires — it does not suppress onClicked");
            compare(root.dualTapCount + root.dualClickCount, 2,
                    "one click, two dispatches: this is why the TapHandler was removed");
        }

        // Empty row space still reaches the MouseArea, so row-click-to-inspect
        // keeps working after the delegate restructure.
        function test_empty_row_area_reaches_mousearea() {
            root.resetCounters();
            mouseClick(row, 10, row.height / 2);
            compare(root.mouseAreaCount, 1, "clicking empty row space must open the inspector");
            compare(root.clickCount, 0, "row click must not trigger a verdict");
        }
    }
}
