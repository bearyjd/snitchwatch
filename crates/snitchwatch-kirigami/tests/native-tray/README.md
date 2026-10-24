# Native KDE tray regression

Run from a host with Flatpak, `dbus-run-session`, and KDE SDK 6.11 installed:

```sh
bash crates/snitchwatch-kirigami/tests/native-tray/run.sh
```

`XDG_DATA_HOME` selects an isolated Flatpak installation when required.
`SNITCHWATCH_TRAY_TEST_SDK` can select another installed SDK reference.
The fixture uses a private bus with no desktop service directories, a fake
StatusNotifierWatcher, and the offscreen display. It does not launch Snitchwatch
or contact the desktop's tray. Missing KDE integration fails the test instead
of silently falling back to the generic widget backend.

The C++ fixture loads the real `TrayMenu.qml`, observes native QMenu show
events before input, and reads its real D-Bus menu export. It verifies all six
entries remain exported, drives pause/resume/show/hide through D-Bus events,
and explicitly opens the native menu. The typed controller seam records calls;
`tests/tray_menu_qml.rs` separately verifies real bridge messages.

Before the fix, KDE SDK Qt 6.11.2 reported `spontaneous-shows=1 visible=1`,
then failed the startup assertion. With the fix it reports zero spontaneous
shows and a hidden initial menu; export/action/manual-popup checks still pass.
This test does not substitute for real Plasma Wayland input/grab acceptance.
The tested SDK manifest identifies plasma-integration source commit
`d5b8362bc1d0f1a0059c02e6c75427548fa64bd4`.

The relevant primary sources are Qt's
[Labs menu synchronization](https://github.com/qt/qtdeclarative/blob/6.10/src/labs/platform/qquicklabsplatformmenu.cpp)
and KDE's
[SystemTrayMenu visibility implementation](https://github.com/KDE/plasma-integration/blob/master/qt6/src/platformtheme/kdeplatformsystemtrayicon.cpp).
Qt defaults `Menu.visible` to true. KDE forwards it to `QMenu::setVisible`,
which displays an unrequested popup during initialization. Explicitly setting
the root tray menu's visibility to false leaves its actions and D-Bus export
available for the tray's context menu request.
