// Profiles tab — view layer over ProfilesModel.
//
// Same list/detail shape as BlocklistsPage.qml/RulesPage.qml: a ListView of
// profiles bound to `ProfilesModel`, with a Kirigami.OverlaySheet detail
// view for rename / edit network matchers / activate / deactivate / delete.
//
// Network matchers are edited as a single comma-separated text field (the
// "simple string list editor" the design calls for) — `ProfilesModel`
// parses/joins the list on the Rust side (`profiles::parse_matchers`), so
// this page never touches individual matcher strings.
//
// create/rename/updateMatchers/deleteProfile/activateProfile/deactivateProfile
// are plain qinvokables on `ProfilesModel`; they emit `profileChangeRequested`
// with a JSON-encoded `ClientMessage` for the live bridge feed to forward —
// the same signal-out pattern `BlocklistsModel.subscribe`/`RulesModel.toggleEnabled`
// use (no bridge changes, no local optimistic mutation — the row reflects
// the server's next `SetProfiles`/`ProfileChanged` push).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Profiles"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property ProfilesModel model

    // Snapshot of the profile currently shown in the detail sheet.
    property string inspectId: ""
    property string inspectName: ""
    property string inspectMatchers: ""
    property bool inspectActive: false
    property bool confirmingDelete: false

    // New-profile creation box lives in the page header, mirroring
    // BlocklistsPage's subscribe box placement.
    titleDelegate: RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            text: page.title
            level: 1
            Layout.alignment: Qt.AlignVCenter
        }
        Controls.TextField {
            id: newProfileName
            Layout.fillWidth: true
            placeholderText: "New profile name…"
        }
        Controls.Button {
            text: "Create"
            icon.name: "list-add"
            enabled: newProfileName.text.trim().length > 0
            onClicked: {
                page.model.createProfile(newProfileName.text.trim(), "");
                newProfileName.text = "";
            }
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: "preferences-system-network"
        text: "No profiles yet"
        explanation: "Create a profile above, e.g. \"At Home\" or \"Public Wi-Fi\", then set its network matchers to auto-activate it."
    }

    ListView {
        id: list
        model: page.model
        currentIndex: -1
        reuseItems: true

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            highlighted: ListView.isCurrentItem

            required property int index
            required property string profileId
            required property string name
            required property string networkMatchers
            required property bool isActive

            onClicked: {
                list.currentIndex = row.index;
                page.openInspector(row);
            }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                Controls.Label {
                    text: row.isActive ? "●" : "○"
                    color: row.isActive ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.disabledTextColor
                    Layout.alignment: Qt.AlignVCenter
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        text: row.name
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        text: row.networkMatchers.length > 0 ? row.networkMatchers : "No network matchers (manual activation only)"
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                }

                Controls.Label {
                    visible: row.isActive
                    text: "Active"
                    color: Kirigami.Theme.positiveTextColor
                    Layout.alignment: Qt.AlignVCenter
                }
            }
        }
    }

    function openInspector(row) {
        page.inspectId = row.profileId;
        page.inspectName = row.name;
        page.inspectMatchers = row.networkMatchers;
        page.inspectActive = row.isActive;
        page.confirmingDelete = false;
        inspector.open();
    }

    // Profile detail + rename + matcher editor + activate/deactivate/delete.
    // Kept as an OverlaySheet, same as BlocklistsPage/RulesPage's inspectors,
    // so it behaves identically at every width.
    Kirigami.OverlaySheet {
        id: inspector
        title: page.inspectName

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            Kirigami.FormLayout {
                Layout.fillWidth: true

                Controls.TextField {
                    id: renameField
                    Kirigami.FormData.label: "Name"
                    text: page.inspectName
                }
                Controls.Button {
                    Kirigami.FormData.label: " "
                    text: "Save name"
                    enabled: renameField.text.trim().length > 0 && renameField.text.trim() !== page.inspectName
                    onClicked: {
                        page.model.renameProfile(page.inspectId, renameField.text.trim());
                        page.inspectName = renameField.text.trim();
                    }
                }

                Controls.TextField {
                    id: matchersField
                    Kirigami.FormData.label: "Network matchers"
                    placeholderText: "Home*, Office-5G"
                    text: page.inspectMatchers
                }
                Controls.Button {
                    Kirigami.FormData.label: " "
                    text: "Save matchers"
                    enabled: matchersField.text !== page.inspectMatchers
                    onClicked: {
                        page.model.updateMatchers(page.inspectId, matchersField.text);
                        page.inspectMatchers = matchersField.text;
                    }
                }

                Controls.Label {
                    Kirigami.FormData.label: "Status"
                    text: page.inspectActive ? "Active" : "Inactive"
                    color: page.inspectActive ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.neutralTextColor
                }
            }

            Kirigami.Separator {
                Layout.fillWidth: true
            }

            Controls.Button {
                Layout.fillWidth: true
                visible: !page.inspectActive
                text: "Activate"
                icon.name: "dialog-ok-apply"
                onClicked: {
                    page.model.activateProfile(page.inspectId);
                    page.inspectActive = true;
                }
            }
            Controls.Button {
                Layout.fillWidth: true
                visible: page.inspectActive
                text: "Deactivate"
                icon.name: "dialog-cancel"
                onClicked: {
                    page.model.deactivateProfile();
                    page.inspectActive = false;
                }
            }

            Kirigami.Separator {
                Layout.fillWidth: true
            }

            // Two-step confirmation kept inline (no separate dialog type
            // introduced) — mirrors RulesPage's existing button-row pattern.
            Controls.Button {
                Layout.fillWidth: true
                visible: !page.confirmingDelete
                text: "Delete profile"
                icon.name: "edit-delete-remove"
                onClicked: page.confirmingDelete = true
            }

            ColumnLayout {
                Layout.fillWidth: true
                visible: page.confirmingDelete
                spacing: Kirigami.Units.smallSpacing

                Controls.Label {
                    Layout.fillWidth: true
                    text: "Delete this profile permanently?"
                    color: Kirigami.Theme.negativeTextColor
                    wrapMode: Text.Wrap
                }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.largeSpacing
                    Controls.Button {
                        Layout.fillWidth: true
                        text: "Cancel"
                        onClicked: page.confirmingDelete = false
                    }
                    Controls.Button {
                        Layout.fillWidth: true
                        text: "Confirm delete"
                        icon.name: "edit-delete-remove"
                        onClicked: {
                            page.model.deleteProfile(page.inspectId);
                            page.confirmingDelete = false;
                            inspector.close();
                        }
                    }
                }
            }
        }
    }
}
