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

The mandatory Fedora 43 `kirigami` CI step runs the native variant:

```sh
bash crates/snitchwatch-kirigami/tests/native-tray/run.sh --native
```

It uses the job's existing Qt development dependencies plus
[`plasma-integration`](https://packages.fedoraproject.org/pkgs/plasma-integration/plasma-integration/)
and [`qt6-qtbase-private-devel`](https://packages.fedoraproject.org/pkgs/qt6-qtbase/qt6-qtbase-private-devel/fedora-43.html).
Qt's installed pkg-config metadata supplies the include root, exact versioned
private-header paths, and moc directory on both Fedora and the KDE SDK.

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
The observed SDK commit was
`305b8ec8d506448558798256bd9afcfd856c7e62357a42e320a6678a08f81465`;
the Platform commit was
`fe192771c0992ad873e6a9ccc6e4f087c76314ce5ced165650cc7ea045a6bbaa`.
Their KDE platform plugins were byte-identical, SHA256
`ebd8a33587a25f9338c0927d73889e499873a11714f2b1d587f7d4e4a4bdd0c4`.

The relevant primary sources are Qt's
[Labs menu synchronization](https://github.com/qt/qtdeclarative/blob/6.10/src/labs/platform/qquicklabsplatformmenu.cpp)
and KDE's
[SystemTrayMenu visibility implementation](https://github.com/KDE/plasma-integration/blob/master/qt6/src/platformtheme/kdeplatformsystemtrayicon.cpp).
Qt defaults `Menu.visible` to true. KDE forwards it to `QMenu::setVisible`,
which displays an unrequested popup during initialization. Explicitly setting
the root tray menu's visibility to false leaves its actions and D-Bus export
available for the tray's context menu request.
