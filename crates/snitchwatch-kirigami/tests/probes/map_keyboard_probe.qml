// Probe for tests/map_keyboard_qml.rs: the production map band on the
// Connections page (MapBand, MapView, MapMarker, MapDestinationsPopup) over
// stand-in geography and connection models. The shared name guard is spliced
// in at `@a11y-guard`, the phases at `@phases`.
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 900
    height: 700
    property var failures: []
    property int phase: 0
    property int stalls: 0
    // ~60 s of waiting at the tick (was 200 ticks: too little on a loaded
    // machine, #363).
    readonly property int maxStalls: 600
    // The item tree's signature at the previous tick; a step runs only once it
    // has held still for a full tick (layout, incubation and focus settled).
    property string lastSignature: ""
    function treeSignature() {
        const out = [String(probe.activeFocusItem)];
        const walk = (item, depth) => {
            if (!item || depth > 40) return;
            out.push(item.visible ? 1 : 0, item.enabled ? 1 : 0, Math.round(item.x), Math.round(item.y),
                     Math.round(item.width), Math.round(item.height), item.activeFocusOnTab ? 1 : 0);
            for (let i = 0; i < item.children.length; ++i) walk(item.children[i], depth + 1);
        };
        walk(probe.contentItem, 0);
        return out.join(",");
    }
    property bool lastRepeat: false
    readonly property int repeat: Qt.GroupSwitchModifier
    // @a11y-guard

    function counts(n) { return { total: n, allowed: n, denied: 0, pending: 0 }; }
    function ip(address, coordinates) {
        return { key: "ip:" + address, canonicalIp: address, address: address, coordinates: coordinates,
                 counts: probe.counts(1), connections: [{ id: "row-" + address, app: "browser", host: address, port: 443, protocol: "tcp" }] };
    }
    // US: New York holds two IPs at one database point (a coincident stack)
    // and a "City unavailable" branch holds one IP with no coordinates. DE:
    // Berlin with one IP. JP: a country with nothing below it. Unknown: no
    // country, so no position at all.
    function fixture() {
        const nyc = { latitude: 40.7, longitude: -74 };
        const us = {
            key: "country:US", code: "US", name: "United States", counts: probe.counts(4),
            cities: {
                "city:nyc": { key: "city:nyc", name: "New York", cityAvailable: true, place: { coordinates: nyc },
                              counts: probe.counts(2), ips: { "ip:8.8.8.8": probe.ip("8.8.8.8", nyc), "ip:8.8.4.4": probe.ip("8.8.4.4", nyc) } },
                "country:US:unknown-city": { key: "country:US:unknown-city", name: "City unavailable", cityAvailable: false, place: null,
                              counts: probe.counts(2), ips: { "ip:9.9.9.9": probe.ip("9.9.9.9", null) } }
            }
        };
        const de = {
            key: "country:DE", code: "DE", name: "Germany", counts: probe.counts(1),
            cities: { "city:ber": { key: "city:ber", name: "Berlin", cityAvailable: true, place: { coordinates: { latitude: 52.5, longitude: 13.4 } },
                                    counts: probe.counts(1), ips: { "ip:1.2.3.4": probe.ip("1.2.3.4", { latitude: 52.5, longitude: 13.4 }) } } }
        };
        const jp = { key: "country:JP", code: "JP", name: "Japan", counts: probe.counts(1), cities: {} };
        const unknown = {
            key: "unknown", code: null, name: "Unknown", counts: probe.counts(1),
            cities: { "unknown:unknown-city": { key: "unknown:unknown-city", name: "City unavailable", cityAvailable: false, place: null,
                                                counts: probe.counts(1), ips: { "ip:7.7.7.7": probe.ip("7.7.7.7", null) } } }
        };
        return { version: 1, observedConnections: 8, countries: { "country:US": us, "country:DE": de, "country:JP": jp, unknown: unknown } };
    }
    QtObject {
        id: geo
        property bool dbAvailable: true
        property var snapshot: probe.fixture()
        signal hierarchyChanged()
        signal modelReset()
        signal countryActivity(string code)
        function landPath() { return ""; }
        function clustersJson(scale, radius, gap) {
            const one = (x, y, code, name) => ({ x: x, y: y, total: 1, dominant: "allowed", mixed: false, summary: name,
                                                  members: [{ code: code, name: name, total: 1, summary: "1 allowed" }] });
            return JSON.stringify([one(400, 300, "US", "United States"), one(1900, 400, "DE", "Germany"), one(2800, 600, "JP", "Japan")]);
        }
        function hierarchyJson() { return JSON.stringify(snapshot); }
    }
    QtObject {
        id: conn
        property string filterText: "app:browser"
        property string geoScopeKind: ""
        property string geoScopeKey: ""
        property int matchingCount: 8
        property var scopes: []
        function setGeoScope(kind, key) {
            scopes = scopes.concat([kind + "|" + key]);
            geoScopeKind = kind;
            geoScopeKey = key;
            return true;
        }
    }
    TestCase { id: tc; name: "mapkeys"; when: false }

    ColumnLayout {
        anchors.fill: parent
        Controls.Button {
            id: start
            text: "Start here"
            Keys.priority: Keys.BeforeItem
            Keys.onPressed: event => probe.lastRepeat = event.isAutoRepeat
        }
        MapBand {
            id: band
            Layout.fillWidth: true
            Layout.preferredHeight: 420
            geoModel: geo
            connectionsModel: conn
        }
        Controls.Button { id: after; text: "After the map" }
    }

    // @phases
}
