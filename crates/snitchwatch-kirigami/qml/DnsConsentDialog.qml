import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Controls.Dialog {
    id: dialog
    objectName: "dnsConsentDialog"
    property CuratedDefaultsModel consentModel
    parent: Controls.Overlay.overlay
    anchors.centerIn: parent
    width: Math.min(parent ? parent.width - 32 : 520, 520)
    modal: true
    title: "Allow the system resolver to send DNS?"
    header: Kirigami.Heading {
        id: titleHeading
        level: 2
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
        textFormat: Text.PlainText
        text: dialog.title
        Controls.ToolTip {
            visible: titleHeading.truncated && titleHover.hovered
            contentItem: Controls.Label {
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: palette.toolTipText
                text: dialog.title
            }
        }
        HoverHandler { id: titleHover }
    }
    closePolicy: Controls.Popup.CloseOnEscape
    visible: consentModel ? consentModel.consentOpen : false
    onClosed: if (consentModel && consentModel.consentOpen) consentModel.cancelBroadConsent()

    contentItem: ColumnLayout {
        Controls.Label {
            objectName: "dnsConsentDisclosure"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: dialog.consentModel ? dialog.consentModel.consentDisclosure : ""
        }
        Controls.Label {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            visible: dialog.consentModel && !dialog.consentModel.consentReady
            text: "Waiting for this connected service to prepare the choice…"
        }
        RowLayout {
            Controls.Button {
                objectName: "dnsConsentCancel"
                text: "Cancel"
                onClicked: dialog.consentModel.cancelBroadConsent()
            }
            Controls.Button {
                objectName: "dnsConsentConfirm"
                text: "Confirm DNS choice"
                enabled: dialog.consentModel && dialog.consentModel.consentReady
                onClicked: dialog.consentModel.confirmBroadConsent()
            }
        }
    }
    // Local timeout closes the dialog; the bridge independently expires tickets.
    Timer {
        interval: 120000
        running: dialog.visible
        onTriggered: if (dialog.consentModel) dialog.consentModel.cancelBroadConsent()
    }
}
