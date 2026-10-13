//! Integration probe for the Monitor map band (`MapBand.qml`, `MapView.qml`,
//! `MapMarker.qml`) inside the real `ConnectionsPage`.
//!
//! The geo model is a stub (a `QtObject` returning canned `clustersJson`), so
//! no GeoIP database is needed; the `ConnectionsModel` is the real one so a
//! marker click is proven by `filterText` actually becoming `country:DE`.
//! cxx-qt-lib cannot synthesise mouse events, so clicks are the button's
//! `clicked()` signal, as in `inline_verdict_qml.rs`.
//!
//! Proves: the band is visible by default with its empty state before any data;
//! a marker has the spoken `Accessible.name` and filters on click; a cluster
//! lists its countries instead; collapse and expand (session state); height
//! clamping so the band gives way first on a short page; pulses are throttled
//! to one per marker per 500 ms and off under reduced motion; the setup text
//! with no database. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::{bridge_bindings as _, connections_model as _, geo_model as _};

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/map_view_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn the_map_band_is_featured_accessible_and_filters_the_list() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    visible: true
    width: 900
    height: 800

    QtObject {
        id: stubGeo
        property bool dbAvailable: true
        property var clusters: []
        signal modelReset()
        signal countryActivity(string code)
        function landPath() { return "M0 0 L3600 0 L3600 1800 L0 1800 Z"; }
        function clustersJson(scale, radius) { return JSON.stringify(clusters); }
    }
    QtObject {
        id: noDb
        property bool dbAvailable: false
        function landPath() { return ""; }
        function clustersJson(scale, radius) { return "[]"; }
    }

    ConnectionsModel { id: conn }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        model: conn
        geoModel: stubGeo
    }
    MapView { id: noModel; width: 400; height: 160; geoModel: null }
    MapView { id: setup; width: 400; height: 160; geoModel: noDb }
    MapBand { id: shortBand; width: 600; geoModel: stubGeo; availableHeight: 200 }
    MapBand { id: tallBand; width: 600; geoModel: stubGeo; availableHeight: 0 }

    function check(cond, msg) {
        if (!cond) throw new Error(msg);
    }

    function marker(i) { return page.mapBand.mapView.markerRepeater.itemAt(i); }

    function cluster1(code, name, d, p) {
        return { x: 1100, y: 420, total: d + p, allowed: 0, denied: d, pending: p,
                 dominant: d >= p ? "denied" : "pending", mixed: d > 0 && p > 0,
                 summary: name + ": " + d + " denied, " + p + " pending",
                 members: [{ code: code, name: name, total: d + p, summary: d + " denied, " + p + " pending" }] };
    }

    Timer {
        interval: 300
        running: true
        repeat: false
        onTriggered: {
            try {
                // Visible by default, with an empty state rather than nothing.
                check(MapBandState.collapsed === false, "a fresh start must show the map");
                const band = page.mapBand;
                const view = band.mapView;
                check(band.visible && view.visible && band.bodyHeight > 0, "band not shown by default");
                check(view.height > 0 && view.width > 0, "map not laid out: " + view.width + "x" + view.height);
                check(view.empty && view.emptyState.visible, "no empty state before data");
                check(view.emptyState.children[0].text === "No destinations to place yet",
                      "wrong empty text: " + view.emptyState.children[0].text);
                check(noModel.emptyState.visible && noModel.emptyState.children[0].text === "No map data",
                      "no-model empty state");
                check(setup.dbMissing && setup.emptyState.visible
                      && setup.emptyState.children[0].text === "No GeoIP database installed"
                      && setup.emptyState.children[1].text.indexOf("never downloads") > 0,
                      "missing-database state");

                // Data arrives: a Germany marker and a two-country cluster.
                stubGeo.clusters = [
                    cluster1("DE", "Germany", 3, 1),
                    { x: 700, y: 500, total: 3, allowed: 3, denied: 0, pending: 0,
                      dominant: "allowed", mixed: false,
                      summary: "2 countries (United States and Canada): 3 allowed",
                      members: [
                        { code: "US", name: "United States", total: 2, summary: "2 allowed" },
                        { code: "CA", name: "Canada", total: 1, summary: "1 allowed" } ] }
                ];
                stubGeo.modelReset();
                check(!view.empty && !view.emptyState.visible, "empty state stays after data");
                check(view.markerRepeater.count === 2, "expected 2 markers, got " + view.markerRepeater.count);
                check(band.countryCount === 3, "country count " + band.countryCount);

                // Accessible names state the counts; markers are keyboard reachable.
                const de = marker(0);
                check(de.Accessible.name === "Germany: 3 denied, 1 pending", "name: " + de.Accessible.name);
                check(marker(1).Accessible.name === "2 countries (United States and Canada): 3 allowed",
                      "cluster name: " + marker(1).Accessible.name);
                check(de.activeFocusOnTab && marker(1).activeFocusOnTab, "markers must take focus");
                check(de.dominant === "denied" && marker(1).dominant === "allowed", "dominant verdicts");

                // A click filters to that country.
                de.clicked();
                check(conn.filterText === "country:DE", "filter is '" + conn.filterText + "'");

                // A cluster lists its countries instead of guessing one.
                conn.setFilterQuery("");
                marker(1).clicked();
                check(conn.filterText === "", "a cluster click must not filter");
                check(view.countryMenu.members.length === 2 && view.countryMenu.members[0].code === "US",
                      "cluster menu members");
                view.countryMenu.close();

                // Pulses: one per marker per 500 ms.
                check(de.pulse(10000) === true && de.pulseCount === 1, "first pulse");
                check(de.pulse(10200) === false && de.pulseCount === 1, "pulse not throttled");
                check(de.pulse(10499) === false && de.pulseCount === 1, "pulse not throttled at 499 ms");
                check(de.pulse(10500) === true && de.pulseCount === 2, "pulse after 500 ms");
                check(marker(1).pulseCount === 0, "other markers are throttled independently");
                check(view.pulse("CA", 20000) === true && marker(1).pulseCount === 1, "pulse by cluster member");
                check(view.pulse("ZZ", 20000) === false, "pulse for a country with no marker");
                stubGeo.countryActivity("DE");
                check(de.pulseCount === 3, "countryActivity did not pulse: " + de.pulseCount);
                stubGeo.countryActivity("DE");
                check(de.pulseCount === 3, "a burst of events must be throttled");

                // Reduced motion: no animation at all.
                view.reducedMotion = true;
                check(de.reducedMotion === true, "reduced motion not passed to markers");
                check(de.pulse(30000) === false && de.pulseCount === 3, "pulse ran under reduced motion");
                view.reducedMotion = false;

                // Collapse and expand: session state, chevron driven.
                band.chevron.clicked();
                check(MapBandState.collapsed && !view.visible && band.bodyHeight === 0, "did not collapse");
                check(band.visible, "the header must stay when collapsed");
                band.chevron.clicked();
                check(!MapBandState.collapsed && view.visible && band.bodyHeight > 0, "did not expand");

                // Resizing is clamped; a short page takes the band to its
                // minimum before the list loses any height.
                MapBandState.bodyHeight = 1;
                check(band.bodyHeight === band.minimumBody, "below minimum: " + band.bodyHeight);
                MapBandState.bodyHeight = 100000;
                check(band.bodyHeight === band.maximumBody, "above maximum: " + band.bodyHeight);
                MapBandState.bodyHeight = 0;
                check(shortBand.bodyHeight === shortBand.minimumBody,
                      "short page: " + shortBand.bodyHeight + " vs " + shortBand.minimumBody);
                check(tallBand.bodyHeight > shortBand.bodyHeight, "an unconstrained band keeps its height");
            } finally {
                MapBandState.collapsed = false;
                MapBandState.bodyHeight = 0;
                Qt.quit();
            }
        }
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
        }
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "map probe failed: root object was null (QML parse error or unregistered type)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) against the probe or the shell's pages:\n{}",
        bad_lines.join("\n")
    );
}
