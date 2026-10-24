// Geographic breakdown page — Little-Snitch-parity per-country connection
// aggregation, without a heavy map widget.
//
// Two graceful-degradation states before the real list shows:
//   * no country database installed at all -> setup placeholder pointing
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

    ListView {
        id: list
        model: page.model && page.model.dbAvailable ? page.model : null
        reuseItems: true
        header: ColumnLayout {
            width: list.width
            spacing: Kirigami.Units.largeSpacing
            Controls.Label {
                objectName: "geoPageDatabaseSource"
                Layout.fillWidth: true
                visible: page.model && page.model.dbAvailable
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                padding: Kirigami.Units.smallSpacing
                text: page.model ? page.model.dbSource : ""
            }
            Controls.Label {
                objectName: "geoCapacityWarning"
                Layout.fillWidth: true
                visible: page.model && page.model.capacityReached
                wrapMode: Text.Wrap
                color: Kirigami.Theme.neutralTextColor
                text: "Geography reached its 100,000-connection display limit. Counts describe a subset of cached connections until the feed resets."
            }
            ColumnLayout {
                Layout.fillWidth: true
                Layout.margins: Kirigami.Units.largeSpacing * 2
                visible: page.model && !page.model.dbAvailable
                spacing: Kirigami.Units.largeSpacing

                Kirigami.PlaceholderMessage {
                    Layout.fillWidth: true
                    icon.name: "map-globe"
                    text: "No GeoIP database installed"
                    explanation: "Install a country database below to identify public destinations. Local-network traffic does not need a database."
                }
                GeoDatabaseSettings {
                    Layout.fillWidth: true
                    model: page.model
                }
            }
        }

        Kirigami.PlaceholderMessage {
            parent: list
            anchors.centerIn: parent
            width: list.width - (Kirigami.Units.largeSpacing * 4)
            visible: page.model && page.model.dbAvailable && page.model.count === 0
            icon.name: "network-connect"
            text: "No geographic data yet"
            explanation: "Country breakdowns for new connections will appear here."
        }

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
                        // From the GeoIP database file on disk (issue #51).
                        textFormat: Text.PlainText
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
