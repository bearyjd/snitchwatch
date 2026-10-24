import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtQuick.Dialogs
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: settings
    property GeoModel model
    spacing: Kirigami.Units.smallSpacing

    function buildDateText(epoch) {
        const date = new Date(epoch * 1000);
        if (epoch <= 0 || isNaN(date.getTime()))
            return "Database build date unknown.";
        return "Database built: " + date.toISOString().split("T")[0]
            + " (UTC). Bundled data updates with Snitchwatch releases.";
    }

    Kirigami.Heading {
        level: 3
        text: "GeoIP database"
    }
    Controls.Label {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        text: "Snitchwatch can use bundled DB-IP Country Lite or an installed country database. To use newer data, download DB-IP Country Lite (no account required), decompress the .gz archive, then select the extracted .mmdb file. Existing MaxMind GeoLite2 Country and GeoIP2 Country databases are also supported."
    }
    Controls.Label {
        text: "Install location"
    }
    Controls.Label {
        objectName: "geoDatabasePath"
        Layout.fillWidth: true
        wrapMode: Text.WrapAnywhere
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbInstallPath : ""
    }
    Controls.Label {
        visible: settings.model && settings.model.dbAvailable
        text: "Current database"
    }
    Controls.Label {
        objectName: "geoDatabaseSource"
        visible: settings.model && settings.model.dbAvailable
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbSource : ""
    }
    Controls.Label {
        objectName: "geoDatabaseBuild"
        visible: settings.model && settings.model.dbAvailable
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        text: settings.model ? settings.buildDateText(settings.model.dbBuildEpoch) : ""
    }
    Controls.Label {
        objectName: "geoActiveDatabasePath"
        visible: settings.model && settings.model.dbAvailable
        Layout.fillWidth: true
        wrapMode: Text.WrapAnywhere
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbPath : ""
    }
    Flow {
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        Controls.Button {
            objectName: "geoDownloadButton"
            text: "Download DB-IP Country Lite…"
            icon.name: "download"
            onClicked: {
                if (!Qt.openUrlExternally("https://db-ip.com/db/download/ip-to-country-lite"))
                    browserError.visible = true;
            }
        }
        Controls.Button {
            text: "MaxMind downloads…"
            onClicked: {
                if (!Qt.openUrlExternally("https://www.maxmind.com/en/accounts/current/geoip/downloads"))
                    browserError.visible = true;
            }
        }
        Controls.Button {
            objectName: "geoInstallButton"
            text: "Install downloaded database…"
            icon.name: "document-open"
            enabled: settings.model && !settings.model.dbInstallBusy
            onClicked: databaseFile.open()
        }
    }
    Controls.Label {
        id: browserError
        Layout.fillWidth: true
        visible: false
        wrapMode: Text.Wrap
        text: "Could not open the browser. Visit https://db-ip.com/db/download/ip-to-country-lite for DB-IP Country Lite, or https://www.maxmind.com/en/accounts/current/geoip/downloads for MaxMind downloads (a MaxMind account is required)."
        color: Kirigami.Theme.negativeTextColor
    }
    Controls.BusyIndicator {
        running: settings.model && settings.model.dbInstallBusy
        visible: running
    }
    Controls.Label {
        objectName: "geoInstallError"
        Layout.fillWidth: true
        visible: settings.model && settings.model.dbInstallError.length > 0
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbInstallError : ""
        color: Kirigami.Theme.negativeTextColor
    }
    Controls.Label {
        objectName: "geoRestartNotice"
        Layout.fillWidth: true
        visible: settings.model && settings.model.dbRestartRequired
        wrapMode: Text.Wrap
        text: "Database installed. Restart Snitchwatch to load it. An explicit SNITCHWATCH_GEOIP_DB override still takes priority over this install location."
    }
    FileDialog {
        id: databaseFile
        title: "Select an extracted country database (.mmdb)"
        fileMode: FileDialog.OpenFile
        nameFilters: ["Country database (*.mmdb)"]
        onAccepted: if (settings.model) settings.model.installDatabase(selectedFile)
    }
}
