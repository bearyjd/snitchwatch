// Traffic tab (Task 11 — view layer over TrafficModel).
//
// Maps from `web/js/traffic.js` (uPlot) + `web/traffic.css`. Spike verdict
// (recorded in docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md,
// Task 11): neither the `QtCharts` nor `QtGraphs` QML modules are installed
// in this environment (`qmake6 -query QT_INSTALL_QML` has no `QtCharts`/
// `QtGraphs` directory) — there is nothing to spike against, so this renders
// the chart on a plain QML `Canvas` instead: a couple of hundred points
// (`TrafficStore`'s 300-second window) repainted once per second is trivial
// for `Canvas`, and it adds zero new dependencies. See
// `crate::traffic::ring_store`'s module docs for the full tradeoff writeup.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.Page {
    id: page
    title: "Traffic"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property TrafficModel model

    property var timestampsMs: []
    property var bytesIn: []
    property var bytesOut: []

    function parseSeries() {
        if (!page.model) {
            return;
        }
        try {
            const parsed = JSON.parse(page.model.seriesJson);
            page.timestampsMs = parsed.timestampsMs || [];
            page.bytesIn = parsed.bytesIn || [];
            page.bytesOut = parsed.bytesOut || [];
        } catch (e) {
            // Malformed/empty payload — degrade to an empty chart rather
            // than throwing (the PlaceholderMessage below covers this case).
            page.timestampsMs = [];
            page.bytesIn = [];
            page.bytesOut = [];
        }
        chart.requestPaint();
    }

    // Small axis-label-only formatter (byte-rate scaling for gridlines).
    // The authoritative current-rate readout above the chart uses
    // TrafficModel's Rust-side `format_rate` via currentInLabel/OutLabel;
    // this only formats the three gridline ticks.
    function formatAxisBytes(v) {
        if (v <= 0) return "0";
        if (v < 1e3) return v.toFixed(0) + "B/s";
        if (v < 1e6) return (v / 1e3).toFixed(1) + "kB/s";
        if (v < 1e9) return (v / 1e6).toFixed(1) + "MB/s";
        return (v / 1e9).toFixed(1) + "GB/s";
    }

    // The model's live outbound feed is started once, in main.qml's
    // Component.onCompleted (its lifetime spans the window, not this page) —
    // mirrors BlocklistsPage.qml/RulesPage.qml, which likewise don't call
    // startBridgeFeed() themselves.
    Component.onCompleted: page.parseSeries()

    Connections {
        target: page.model
        function onSeriesJsonChanged() {
            page.parseSeries();
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: "office-chart-line"
        text: "No traffic data yet"
        explanation: "Per-second in/out byte counts will appear here once the daemon starts reporting traffic."
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: Kirigami.Units.largeSpacing
        visible: page.model && page.model.count > 0

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.largeSpacing

            RowLayout {
                spacing: Kirigami.Units.smallSpacing
                Rectangle {
                    width: Kirigami.Units.gridUnit * 0.5
                    height: width
                    radius: 2
                    color: Kirigami.Theme.highlightColor
                    Layout.alignment: Qt.AlignVCenter
                }
                Controls.Label {
                    text: "In: " + (page.model ? page.model.currentInLabel : "--")
                }
            }
            RowLayout {
                spacing: Kirigami.Units.smallSpacing
                Rectangle {
                    width: Kirigami.Units.gridUnit * 0.5
                    height: width
                    radius: 2
                    color: Kirigami.Theme.neutralTextColor
                    Layout.alignment: Qt.AlignVCenter
                }
                Controls.Label {
                    text: "Out: " + (page.model ? page.model.currentOutLabel : "--")
                }
            }
            Item {
                Layout.fillWidth: true
            }
        }

        Canvas {
            id: chart
            Layout.fillWidth: true
            Layout.fillHeight: true
            antialiasing: true

            onWidthChanged: requestPaint()
            onHeightChanged: requestPaint()

            readonly property color gridColor: Kirigami.Theme.disabledTextColor
            readonly property color axisTextColor: Kirigami.Theme.textColor
            readonly property color inColor: Kirigami.Theme.highlightColor
            readonly property color outColor: Kirigami.Theme.neutralTextColor

            onPaint: {
                const ctx = getContext("2d");
                ctx.clearRect(0, 0, width, height);

                const n = page.bytesIn.length;
                if (n === 0) {
                    return;
                }

                let maxVal = 1;
                for (let i = 0; i < n; i++) {
                    maxVal = Math.max(maxVal, page.bytesIn[i], page.bytesOut[i]);
                }

                const padLeft = 56;
                const padBottom = 4;
                const padTop = 8;
                const padRight = 4;
                const plotW = Math.max(1, width - padLeft - padRight);
                const plotH = Math.max(1, height - padBottom - padTop);

                ctx.strokeStyle = chart.gridColor;
                ctx.globalAlpha = 0.35;
                ctx.lineWidth = 1;
                ctx.font = "10px sans-serif";
                ctx.fillStyle = chart.axisTextColor;
                ctx.globalAlpha = 1.0;
                for (let g = 0; g <= 2; g++) {
                    const frac = g / 2;
                    const y = padTop + plotH * (1 - frac);
                    ctx.globalAlpha = 0.35;
                    ctx.beginPath();
                    ctx.moveTo(padLeft, y);
                    ctx.lineTo(padLeft + plotW, y);
                    ctx.stroke();
                    ctx.globalAlpha = 1.0;
                    ctx.fillText(page.formatAxisBytes(maxVal * frac), 2, y + 3);
                }

                function plotSeries(series, color) {
                    ctx.strokeStyle = color;
                    ctx.lineWidth = 2;
                    ctx.beginPath();
                    for (let i = 0; i < n; i++) {
                        const x = padLeft + (n === 1 ? plotW : (plotW * i) / (n - 1));
                        const y = padTop + plotH * (1 - Math.min(1, series[i] / maxVal));
                        if (i === 0) {
                            ctx.moveTo(x, y);
                        } else {
                            ctx.lineTo(x, y);
                        }
                    }
                    ctx.stroke();
                }

                plotSeries(page.bytesIn, chart.inColor);
                plotSeries(page.bytesOut, chart.outColor);
            }
        }
    }
}
