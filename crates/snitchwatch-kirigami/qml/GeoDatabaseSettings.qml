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
    property bool cityImport: false

    function buildDateText(epoch) {
        const date = new Date(epoch * 1000);
        if (epoch <= 0 || isNaN(date.getTime()))
            return "Database build date unknown.";
        return "Database built: " + date.toISOString().split("T")[0]
            + " (UTC). Bundled data updates with Snitchwatch releases.";
    }

    SelectableText {
        wrapMode: Text.NoWrap
        level: 3
        text: "GeoIP database"
    }
    SelectableText {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        text: "Country data is bundled. Optional DB-IP City Lite adds approximate city estimates without an account. Download the City Lite archive in your browser, decompress it, then import the extracted .mmdb file below. No background download is performed. MaxMind Country and City databases are also supported."
    }
    SelectableText {
        objectName: "geoDatabaseCapability"
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        text: settings.model && settings.model.dbCityAvailable
            ? "City-capable database active. Locations are approximate database estimates; several IPs may share one position. A city may be unavailable for some IPs."
            : "Country data active. Install optional City data for approximate city estimates."
    }
    SelectableText {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        text: "IP Geolocation by DB-IP (https://db-ip.com), CC BY 4.0. Lite data has reduced accuracy and coverage. Imports are stored privately beside the fallback paths below. Importing selects that dataset after restart; Country data remains the fallback."
    }
    SelectableText {
        wrapMode: Text.NoWrap
        text: "Manual Country fallback location"
    }
    SelectableText {
        objectName: "geoDatabasePath"
        Layout.fillWidth: true
        wrapMode: Text.WrapAnywhere
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbInstallPath : ""
    }
    SelectableText {
        wrapMode: Text.NoWrap
        text: "Manual City fallback location"
    }
    SelectableText {
        objectName: "geoCityDatabasePath"
        Layout.fillWidth: true
        wrapMode: Text.WrapAnywhere
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbCityInstallPath : ""
    }
    SelectableText {
        wrapMode: Text.NoWrap
        visible: settings.model && settings.model.dbAvailable
        text: "Current database"
    }
    SelectableText {
        objectName: "geoDatabaseSource"
        visible: settings.model && settings.model.dbAvailable
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbSource : ""
    }
    SelectableText {
        objectName: "geoDatabaseBuild"
        visible: settings.model && settings.model.dbAvailable
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        text: settings.model ? settings.buildDateText(settings.model.dbBuildEpoch) : ""
    }
    SelectableText {
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
            objectName: "geoCityDownloadButton"
            text: "Download optional DB-IP City Lite…"
            icon.name: "download"
            onClicked: {
                if (!Qt.openUrlExternally("https://db-ip.com/db/download/ip-to-city-lite"))
                    browserError.visible = true;
            }
        }
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
            text: "Import Country database…"
            icon.name: "document-open"
            enabled: settings.model && !settings.model.dbInstallBusy
            onClicked: { settings.cityImport = false; databaseFile.open(); }
        }
        Controls.Button {
            objectName: "geoCityInstallButton"
            text: "Import optional City database…"
            icon.name: "document-open"
            enabled: settings.model && !settings.model.dbInstallBusy
            onClicked: { settings.cityImport = true; databaseFile.open(); }
        }
    }
    SelectableText {
        id: browserError
        Layout.fillWidth: true
        visible: false
        wrapMode: Text.Wrap
        text: "Could not open the browser. Visit https://db-ip.com/db/download/ip-to-city-lite for City Lite or https://db-ip.com/db/download/ip-to-country-lite for Country Lite, or https://www.maxmind.com/en/accounts/current/geoip/downloads for MaxMind downloads (a MaxMind account is required)."
        color: Kirigami.Theme.negativeTextColor
    }
    Controls.BusyIndicator {
        running: settings.model && settings.model.dbInstallBusy
        visible: running
    }
    SelectableText {
        objectName: "geoInstallError"
        Layout.fillWidth: true
        visible: settings.model && settings.model.dbInstallError.length > 0
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: settings.model ? settings.model.dbInstallError : ""
        color: Kirigami.Theme.negativeTextColor
    }
    SelectableText {
        objectName: "geoRestartNotice"
        Layout.fillWidth: true
        visible: settings.model && settings.model.dbRestartRequired
        wrapMode: Text.Wrap
        text: "Database installed. Restart Snitchwatch to load it. The imported dataset is selected for the next launch. Explicit SNITCHWATCH_GEOIP_DB and SNITCHWATCH_GEOIP_CITY_DB overrides still take priority."
    }
    FileDialog {
        id: databaseFile
        title: settings.cityImport ? "Select an extracted City database (.mmdb)" : "Select an extracted Country database (.mmdb)"
        fileMode: FileDialog.OpenFile
        nameFilters: ["GeoIP database (*.mmdb)"]
        onAccepted: {
            if (!settings.model) return;
            if (settings.cityImport) settings.model.installCityDatabase(selectedFile);
            else settings.model.installDatabase(selectedFile);
        }
    }
}
