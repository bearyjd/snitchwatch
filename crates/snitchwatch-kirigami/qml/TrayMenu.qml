// Tray context menu, shown through the platform's StatusNotifierItem tray.
//
// Flat on purpose: Plasma's dbusmenu export rendered a nested Labs.Menu as a
// bare "Pause filtering" entry with no submenu, and activating it did nothing
// (VM run r6). A possible second cause: on a Labs.Menu, `visible` means "the
// popup is open", so the old submenu's `visible:` binding never meant "show
// this entry". Every pause length is therefore its own top-level item, wired
// straight to TrayController.pauseFor — the bridge accepts only these three
// lengths and ends every pause on its own (issue #47).
//
// Every item is always visible and gated with `enabled:` instead. Whether a
// live `visible` flip reaches a menu the tray has already exported is
// unverified, and the menu is built before the first bridge state arrives,
// when nothing could be offered yet. Driven end to end by
// tests/tray_menu_qml.rs; the objectNames are what that probe triggers.
//
// A pause also lets the connections already waiting for an answer through,
// Allow once (issue #78), so each pause item says so, but only while the
// connected bridge advertises `pauseAnswersWaiting`: a bridge from before
// that leaves them waiting, and the menu must not promise otherwise.
import QtQuick
import Qt.labs.platform as Labs
import com.snitchwatch.shell

Labs.Menu {
    id: trayMenu

    required property TrayController controller
    // The main window, for Show/Hide. Must provide raiseAndActivate().
    required property var window

    // Pause only while filtering can be paused, Resume only while it is
    // paused; "reconnect" (DaemonDown) and "default" offer neither.
    readonly property bool canPause: controller.menuLabel === "pause_filtering"
    readonly property bool canResume: controller.menuLabel === "resume_filtering"
    // What each pause item adds when the bridge answers the waiting prompts.
    readonly property string waitingSuffix: controller.pauseAnswersWaiting
        ? " (also lets waiting connections through once)"
        : ""

    Labs.MenuItem {
        text: trayMenu.window.visible ? "Hide window" : "Show window"
        onTriggered: trayMenu.window.visible ? trayMenu.window.hide() : trayMenu.window.raiseAndActivate()
    }
    Labs.MenuItem {
        separator: true
    }
    Labs.MenuItem {
        objectName: "pauseFor300"
        enabled: trayMenu.canPause
        text: "Pause for 5 minutes" + trayMenu.waitingSuffix
        onTriggered: trayMenu.controller.pauseFor(300)
    }
    Labs.MenuItem {
        objectName: "pauseFor1800"
        enabled: trayMenu.canPause
        text: "Pause for 30 minutes" + trayMenu.waitingSuffix
        onTriggered: trayMenu.controller.pauseFor(1800)
    }
    Labs.MenuItem {
        objectName: "pauseFor3600"
        enabled: trayMenu.canPause
        text: "Pause for 1 hour" + trayMenu.waitingSuffix
        onTriggered: trayMenu.controller.pauseFor(3600)
    }
    Labs.MenuItem {
        objectName: "resumeFiltering"
        enabled: trayMenu.canResume
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
