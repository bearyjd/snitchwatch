// Loads the production map, camera and pointer handlers. The test import
// directory exposes only the two real QML singletons needed by this slice.
import QtQuick
import QtTest
import com.snitchwatch.shell
import "../../qml" as Production

Item {
    width: 900
    height: 500
    QtObject {
        id: geo
        property bool dbAvailable: true
        property real lastScale: 0
        property bool splitOnZoom: false
        signal modelReset()
        signal countryActivity(string code)
        function landPath() { return "M0 60 L3600 60 L3600 1500 L0 1500 Z"; }
        function clustersJson(scale, radius, gap) {
            lastScale = scale;
            if (splitOnZoom) {
                const de = { code: "DE", name: "Germany", total: 1, summary: "1 allowed" };
                const fr = { code: "FR", name: "France", total: 1, summary: "1 allowed" };
                if (scale < 0.5) return JSON.stringify([{ x: 1800, y: 900, total: 2, dominant: "allowed", mixed: false,
                    summary: "2 countries", members: [de, fr] }]);
                return JSON.stringify([de, fr].map((m, i) => ({ x: 1600 + i * 400, y: 900,
                    total: 1, dominant: "allowed", mixed: false, summary: m.name, members: [m] })));
            }
            return JSON.stringify([{ x: 1800, y: 900, total: 1, dominant: "allowed", mixed: false,
                summary: "Germany: 1 allowed", members: [{ code: "DE", name: "Germany", total: 1, summary: "1 allowed" }] }]);
        }
    }
    QtObject {
        id: connections
        property string query: "app:browser"
        function setFilterQuery(value) { query = value; }
    }
    Production.MapView {
        id: map
        width: 900
        height: 400
        geoModel: geo
        connectionsModel: connections
        reducedMotion: true
        refreshCooldownMs: 1
    }
    TestCase {
        name: "ProductionMapCamera"
        when: windowShown
        function near(actual, expected, message) {
            verify(Math.abs(actual - expected) < 0.02, message + ": " + actual + " != " + expected);
        }
        function init() {
            map.width = 900;
            map.height = 400;
            connections.query = "app:browser";
            verify(map.camera !== undefined, "production map exposes its shared camera");
            map.camera.reset();
            map.forceActiveFocus();
            wait(20);
        }
        function cleanup() {
            geo.splitOnZoom = false;
            map.refresh();
            map.camera.minimumZoom = 1;
            map.camera.maximumZoom = 16;
            MapBandState.camera = { zoom: 1, x: 1800, y: 900 };
        }
        function test_cluster_focus_splits_with_effective_scale() {
            geo.splitOnZoom = true;
            map.refresh();
            compare(map.markerRepeater.count, 1);
            mouseClick(map.markerRepeater.itemAt(0));
            verify(map.camera.zoom > 1, "country cluster activation focuses its existing aggregate position");
            compare(map.countryMenu.members.length, 2, "country menu remains available");
            map.countryMenu.close();
            map.camera.zoomAt(2, 450, 200);
            tryCompare(map.markerRepeater, "count", 2);
            compare(connections.query, "app:browser", "cluster focus leaves search intact");
        }
        function test_focus_fit_and_configurable_limits() {
            map.camera.maximumZoom = 32;
            map.camera.focusPoint(1900, 800, 30);
            compare(map.camera.zoom, 30);
            near(map.camera.project(1900, 800).x, 450, "focused x");
            near(map.camera.project(1900, 800).y, 200, "focused y");
            verify(map.camera.fitBounds(1500, 600, 2100, 960, 20));
            near(map.camera.zoom, 4.5, "360 grid units fit in 360 viewport pixels");
            near(map.camera.project(1500, 600).x, 150, "fit west");
            near(map.camera.project(2100, 960).y, 380, "fit south");
            verify(!map.camera.fitBounds(3500, 600, 100, 960, 20), "wrapped extent requires explicit caller policy");
            verify(!map.camera.focusPoint(NaN, 800, 3), "non-finite location rejected");
            map.camera.maximumZoom = 2;
            compare(map.camera.zoom, 2, "changed maximum clamps existing camera");
        }
        function test_small_viewport_and_collapsed_geometry_stay_finite() {
            map.width = 1;
            map.height = 1;
            map.camera.focusPoint(3600, 1800, 16);
            near(map.camera.project(3600, 1800).x, 1, "tiny viewport east bound");
            near(map.camera.project(3600, 1800).y, 1, "tiny viewport south bound");
            map.width = 0;
            map.height = 0;
            map.camera.zoomAt(2, 0, 0);
            map.camera.panBy(20, 20);
            const point = map.camera.unproject(0, 0);
            verify(isFinite(point.x) && isFinite(point.y), "collapsed viewport retains a finite grid center");
            map.width = 900;
            map.height = 400;
            map.camera.reset();
            near(map.camera.project(1800, 900).x, 450, "reopened viewport midpoint");
        }
        function test_controls_and_session_restore() {
            const component = Qt.createComponent("../../qml/MapBand.qml");
            compare(component.status, Component.Ready, component.errorString());
            const band = createTemporaryObject(component, map.parent, { width: 900, height: 480, geoModel: geo, connectionsModel: connections });
            verify(band !== null);
            wait(20);
            const plus = findChild(band, "mapZoomIn");
            const minus = findChild(band, "mapZoomOut");
            const fit = findChild(band, "mapFit");
            verify(plus && minus && fit, "accessible camera controls are present");
            mouseClick(plus);
            verify(band.mapView.camera.zoom > 1);
            mouseClick(minus);
            compare(band.mapView.camera.zoom, 1);
            band.mapView.camera.focusPoint(1850, 800, 3);
            compare(MapBandState.camera.zoom, 3, "camera is saved only in session singleton");
            band.destroy();
            wait(20);
            const restored = createTemporaryObject(component, map.parent, { width: 900, height: 480, geoModel: geo, connectionsModel: connections });
            verify(restored !== null);
            wait(20);
            compare(restored.mapView.camera.zoom, 3);
            near(restored.mapView.camera.centerX, 1850, "recreated band restores longitude");
            mouseClick(findChild(restored, "mapFit"));
            compare(restored.mapView.camera.zoom, 1);
            compare(connections.query, "app:browser", "controls preserve search");
        }
        function test_projection_and_alignment() {
            near(map.mapScale, 2/9, "independent 900/3600 fit scale");
            const origin = map.camera.project(0, 0);
            near(origin.x, 50, "cropped origin x");
            near(origin.y, 0, "cropped origin y");
            map.camera.zoomAt(2, 450, 200);
            const point = map.camera.project(1800, 900);
            near(point.x, 450, "grid midpoint x");
            near(point.y, 200, "cropped midpoint y");
            const marker = map.markerRepeater.itemAt(0);
            near(marker.x + marker.width / 2, point.x, "marker x matches geometry projection");
            near(marker.y + marker.height / 2, point.y, "marker y matches geometry projection");
            near(map.landItem.x, map.camera.project(0, 0).x, "shape shares x transform");
            near(map.landItem.y, map.camera.project(0, 0).y, "shape shares y transform");
            tryVerify(() => Math.abs(geo.lastScale - 4/9) < 0.001);
        }
        function test_zoom_keeps_country_borders_readable() {
            const outline = map.landItem.data[0];
            verify(outline && outline.strokeWidth !== undefined, "production country outline exists");
            near(outline.strokeWidth * map.mapScale, 10/9, "fitted-world stroke width");
            map.camera.zoomAt(16, 450, 200);
            near(outline.strokeWidth * map.mapScale, 10/9, "zoomed outline stays at fitted-world screen width");
        }
        function test_clipped_markers_leave_keyboard_traversal() {
            const marker = map.markerRepeater.itemAt(0);
            marker.forceActiveFocus();
            verify(marker.activeFocus);
            map.camera.focusPoint(2700, 900, 8);
            wait(20);
            verify(!marker.enabled && !marker.activeFocusOnTab, "completely clipped marker cannot be activated or tabbed to");
            verify(!marker.activeFocus, "moving outside viewport releases existing marker focus");
            map.forceActiveFocus();
            keyClick(Qt.Key_Tab);
            verify(!marker.activeFocus, "real Tab skips clipped production marker");
            map.camera.reset();
            verify(marker.enabled && marker.activeFocusOnTab, "visible marker becomes keyboard reachable again");
        }
        function test_wheel_anchor_limits_and_search() {
            map.camera.zoomAt(2, 450, 200);
            const before = map.camera.unproject(350, 180);
            mouseWheel(map, 350, 180, 0, 120, Qt.NoButton);
            tryVerify(() => map.camera.zoom > 2);
            const after = map.camera.unproject(350, 180);
            near(after.x, before.x, "wheel anchor longitude");
            near(after.y, before.y, "wheel anchor latitude");
            compare(connections.query, "app:browser");
            map.camera.zoomAt(1e6, 450, 200);
            compare(map.camera.zoom, 16);
            map.camera.zoomAt(1e-6, 450, 200);
            compare(map.camera.zoom, 1);
            near(map.camera.centerX, 1800, "fit horizontal center");
            near(map.camera.centerY, 900, "fit vertical center");
        }
        function test_drag_marker_cancels_click_and_bounds() {
            map.camera.zoomAt(4, 450, 200);
            const marker = map.markerRepeater.itemAt(0);
            mouseDrag(marker, marker.width / 2, marker.height / 2, 80, 35, Qt.LeftButton);
            verify(map.camera.centerX < 1800, "drag starting over marker pans the map");
            compare(connections.query, "app:browser", "drag must not activate country filter");
            map.camera.panBy(1e6, 1e6);
            near(map.camera.project(0, 0).x, 0, "pan cannot expose west edge");
            near(map.camera.project(0, 0).y, 0, "pan cannot expose north edge");
            map.camera.panBy(-2e6, -2e6);
            near(map.camera.project(3600, 1800).x, 900, "pan cannot expose east edge");
            near(map.camera.project(3600, 1800).y, 400, "pan cannot expose south edge");
        }
        function test_country_click_keyboard_reset_and_resize() {
            const marker = map.markerRepeater.itemAt(0);
            mouseClick(marker);
            compare(connections.query, "country:DE", "existing country selection remains available");
            map.forceActiveFocus();
            keyClick(Qt.Key_Plus);
            verify(map.camera.zoom > 1);
            keyClick(Qt.Key_Right);
            verify(map.camera.centerX > 1800);
            const x = map.camera.centerX;
            const y = map.camera.centerY;
            map.width = 800;
            map.height = 350;
            wait(20);
            near(map.camera.centerX, x, "resize preserves grid longitude");
            near(map.camera.centerY, y, "resize preserves grid latitude");
            keyClick(Qt.Key_Home);
            compare(map.camera.zoom, 1);
            near(map.camera.centerX, 1800, "reset longitude");
            near(map.camera.centerY, 900, "reset latitude");
            compare(connections.query, "country:DE", "camera reset retains country filter");
        }
        function test_touch_pinch() {
            map.camera.zoomAt(2, 450, 200);
            const touch = touchEvent(map);
            touch.press(0, map, 350, 170).press(1, map, 550, 230).commit();
            wait(30);
            touch.move(0, map, 330, 164).move(1, map, 570, 236).commit();
            wait(30);
            touch.move(0, map, 290, 152).move(1, map, 610, 248).commit();
            wait(30);
            touch.move(0, map, 250, 140).move(1, map, 650, 260).commit();
            wait(30);
            verify(map.camera.zoom > 2, "real two-finger pinch zooms production map");
            near(map.camera.project(1800, 900).x, 450, "pinch center x");
            near(map.camera.project(1800, 900).y, 200, "pinch center y");
            touch.move(0, map, 270, 150).move(1, map, 670, 270).commit();
            wait(30);
            near(map.camera.project(1800, 900).x, 470, "two-finger translation x");
            near(map.camera.project(1800, 900).y, 210, "two-finger translation y");
            touch.release(0, map, 270, 150).release(1, map, 670, 270).commit();
        }
    }
}
