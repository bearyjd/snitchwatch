// Tray context menu, shown through the platform's StatusNotifierItem tray.
//
// Flat on purpose: Plasma's dbusmenu export rendered a nested Labs.Menu as a
// bare "Pause filtering" entry with no submenu, and activating it did nothing
// (VM run r6). Every pause length is therefore its own top-level item, wired
// straight to TrayController.pauseFor — the bridge accepts only these three
// lengths and ends every pause on its own (issue #47). Guarded by
// tray.rs's `tray_menu_is_flat` and driven end to end by
// tests/tray_menu_qml.rs; the objectNames are what that probe triggers.
import QtQuick
import Qt.labs.platform as Labs
import com.snitchwatch.shell

Labs.Menu {
    id: trayMenu

    required property TrayController controller
    // The main window, for Show/Hide. Must provide raiseAndActivate().
    required property var window

    // Pause items only while filtering can be paused, Resume only while it
    // is paused; "reconnect" (DaemonDown) and "default" offer neither.
    readonly property bool canPause: controller.menuLabel === "pause_filtering"
    readonly property bool canResume: controller.menuLabel === "resume_filtering"

    Labs.MenuItem {
        text: trayMenu.window.visible ? "Hide window" : "Show window"
        onTriggered: trayMenu.window.visible ? trayMenu.window.hide() : trayMenu.window.raiseAndActivate()
    }
    Labs.MenuItem {
        objectName: "pauseFor300"
        visible: trayMenu.canPause
        text: "Pause for 5 minutes"
        onTriggered: trayMenu.controller.pauseFor(300)
    }
    Labs.MenuItem {
        objectName: "pauseFor1800"
        visible: trayMenu.canPause
        text: "Pause for 30 minutes"
        onTriggered: trayMenu.controller.pauseFor(1800)
    }
    Labs.MenuItem {
        objectName: "pauseFor3600"
        visible: trayMenu.canPause
        text: "Pause for 1 hour"
        onTriggered: trayMenu.controller.pauseFor(3600)
    }
    Labs.MenuItem {
        objectName: "resumeFiltering"
        visible: trayMenu.canResume
        text: trayMenu.controller.pausedUntil
            ? "Resume filtering (until " + trayMenu.controller.pausedUntil + ")"
            : "Resume filtering"
        onTriggered: trayMenu.controller.resume()
    }
    Labs.MenuItem {
        separator: true
    }
    Labs.MenuItem {
        text: "Quit"
        onTriggered: Qt.quit()
    }
}
