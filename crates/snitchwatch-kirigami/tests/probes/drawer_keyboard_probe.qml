// Probe for tests/drawer_keyboard_qml.rs: the REAL main.qml, driven by real
// key events only (QtTest keyClick/keyPress on its window). The shared name
// guard is spliced in at `@a11y-guard` (it expects `contentItem`,
// `activeFocusItem` and `failures` on `probe`), the phases at `@phases`.
import QtQuick
import QtTest
import com.snitchwatch.shell

QtObject {
    id: probe
    property var shell: null
    property var tc: null
    property var failures: []
    property string phase: "starting"
    readonly property int repeat: Qt.GroupSwitchModifier
    readonly property var contentItem: shell ? shell.contentItem : null
    readonly property var activeFocusItem: shell ? shell.activeFocusItem : null
    readonly property Component tcc: Component { TestCase { when: false; name: "drawer" } }
    // @a11y-guard
    // @phases

    // The whole run is one synchronous sequence that waits (bounded) on each
    // readiness gate; the watchdog fails it, naming the phase, if it stalls.
    property Timer starter: Timer {
        interval: 200
        running: true
        onTriggered: {
            try {
                probe.run();
            } catch (e) {
                probe.failures.push("phase '" + probe.phase + "' threw: " + e);
            }
            probe.finish();
        }
    }
    property Timer watchdog: Timer {
        interval: 45000
        running: true
        onTriggered: {
            probe.failures.push("watchdog: stuck in phase '" + probe.phase + "'");
            probe.finish();
        }
    }
    property bool finished: false
    function finish() {
        if (probe.finished) return;
        probe.finished = true;
        probe.starter.stop();
        probe.watchdog.stop();
        // A Kirigami window that refuses to close (main.qml hides to the tray)
        // blocks Qt.quit(), so drop it first.
        if (probe.shell) probe.shell.destroy();
        probe.shell = null;
        probe.quitter.start();
    }
    property Timer quitter: Timer {
        interval: 200
        onTriggered: {
            if (probe.failures.length > 0) {
                console.warn("drawer keyboard probe: " + probe.failures.join("; "));
            } else {
                console.warn("DRAWER_KEYBOARD_PROBE_DONE");
            }
            Qt.quit();
        }
    }
}
