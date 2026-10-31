// About page — which build is running, for bug reports.
//
// Display only, plus one button that puts the same facts on the clipboard as
// plain text. All values come from `AboutModel` (Rust, see `about.rs`), which
// strips control characters and caps lengths; they are still shown as
// `Text.PlainText`. The daemon version is injected by main.qml from the
// Traffic model, which already receives it; the bridge's own version is not
// on the wire and so is not shown.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "About"

    // Where the Flatpak runtime describes this instance; tests point it at
    // a fixture.
    property string flatpakInfoPath: "/.flatpak-info"
    // The connected opensnitchd's version; empty while none is known.
    property string daemonVersion: ""
    property bool daemonConnected: false
    readonly property alias about: about
    property bool copied: false

    readonly property var rows: {
        try {
            return JSON.parse(about.fieldsJson);
        } catch (e) {
            return [];
        }
    }

    AboutModel {
        id: about
        flatpakInfoPath: page.flatpakInfoPath
        daemonVersion: page.daemonVersion
        daemonConnected: page.daemonConnected
        Component.onCompleted: refresh()
        onFlatpakInfoPathChanged: refresh()
        onDaemonVersionChanged: refresh()
        onDaemonConnectedChanged: refresh()
    }

    ClipboardCopy { id: clipboard }

    ColumnLayout {
        width: page.width
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            level: 2
            text: "Snitchwatch"
            Layout.fillWidth: true
        }

        GridLayout {
            Layout.fillWidth: true
            columns: 2
            columnSpacing: Kirigami.Units.largeSpacing
            rowSpacing: Kirigami.Units.smallSpacing

            Repeater {
                model: page.rows
                delegate: Item {
                    required property var modelData
                    required property int index
                    Layout.columnSpan: 2
                    Layout.fillWidth: true
                    implicitHeight: row.implicitHeight
                    RowLayout {
                        id: row
                        width: parent.width
                        spacing: Kirigami.Units.largeSpacing
                        Controls.Label {
                            objectName: "aboutLabel_" + modelData.key
                            Layout.preferredWidth: Kirigami.Units.gridUnit * 12
                            Layout.alignment: Qt.AlignTop
                            font.bold: true
                            wrapMode: Text.Wrap
                            textFormat: Text.PlainText
                            text: modelData.label
                        }
                        Controls.Label {
                            objectName: "aboutValue_" + modelData.key
                            Layout.fillWidth: true
                            wrapMode: Text.WrapAnywhere
                            textFormat: Text.PlainText
                            text: modelData.value
                        }
                    }
                }
            }
        }

        RowLayout {
            spacing: Kirigami.Units.largeSpacing
            Controls.Button {
                id: copyButton
                objectName: "aboutCopyButton"
                // Restyled in place, not swapped for `RoundedButton`: the drawer
                // keyboard probe asserts this control's QML type name (`Button`).
                implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
                horizontalPadding: Theme.controlPaddingH
                background: RoundedSurface { control: copyButton }
                text: "Copy version info"
                icon.name: "edit-copy"
                Accessible.name: "Copy version info"
                Accessible.description: "Copies the version details above to the clipboard as plain text"
                onClicked: {
                    clipboard.copy(about.reportText);
                    page.copied = true;
                }
            }
            Controls.Label {
                objectName: "aboutCopiedNote"
                visible: page.copied
                textFormat: Text.PlainText
                text: "Copied to the clipboard."
            }
        }
    }
}
