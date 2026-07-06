// Geographic breakdown page — Little-Snitch-parity per-country connection
// aggregation, without a heavy map widget.
//
// Two graceful-degradation states before the real list shows:
//   * no GeoLite2-Country.mmdb installed at all -> setup placeholder pointing
//     at where to place one (Kirigami.PlaceholderMessage, not an error);
//   * a database is installed but no connections have been aggregated yet ->
//     "no data yet" placeholder (mirrors ConnectionsPage's empty state).
//
// Each row shows the flag + country name + total, with a thin proportional
// bar (relative to the busiest country) and the allow/deny/pending split.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Geography"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property GeoModel model

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: page.model && !page.model.dbAvailable
        icon.name: "map-globe"
        text: "No GeoIP database installed"
        explanation: "Place a GeoLite2-Country.mmdb file at:\n" + (page.model ? page.model.dbPath : "")
                     + "\n\nLocal-network traffic still appears below; public destinations show as “Unknown” until a database is installed. Snitchwatch never downloads one automatically."
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: page.model && page.model.dbAvailable && page.model.count === 0
        icon.name: "network-connect"
        text: "No geographic data yet"
        explanation: "Country breakdowns for new connections will appear here."
    }

    ListView {
        id: list
        model: page.model && page.model.dbAvailable ? page.model : null
        reuseItems: true

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            hoverEnabled: false

            required property string countryCode
            required property string countryName
            required property string flag
            required property int total
            required property int allowed
            required property int denied
            required property int pending

            readonly property real barFraction: {
                const maxTotal = page.model ? page.model.maxTotal : 0;
                return maxTotal > 0 ? row.total / maxTotal : 0;
            }

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.largeSpacing

                    Controls.Label {
                        text: row.flag
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.4
                    }

                    Controls.Label {
                        text: row.countryName
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }

                    Controls.Label {
                        text: row.total
                        font.bold: true
                    }
                }

                Rectangle {
                    Layout.fillWidth: true
                    height: 4
                    radius: 2
                    color: Kirigami.Theme.alternateBackgroundColor

                    Rectangle {
                        height: parent.height
                        radius: 2
                        width: parent.width * row.barFraction
                        color: Kirigami.Theme.highlightColor
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.largeSpacing

                    Controls.Label {
                        text: "allowed " + row.allowed
                        color: Kirigami.Theme.positiveTextColor
                        font: Kirigami.Theme.smallFont
                    }
                    Controls.Label {
                        text: "denied " + row.denied
                        color: Kirigami.Theme.negativeTextColor
                        font: Kirigami.Theme.smallFont
                    }
                    Controls.Label {
                        text: "pending " + row.pending
                        color: Kirigami.Theme.neutralTextColor
                        font: Kirigami.Theme.smallFont
                    }
                    Item { Layout.fillWidth: true }
                }
            }
        }
    }
}
